//! Tool ghosts: while a cutter is selected, or its parameter is dragged, typed or hovered,
//! the tool body itself shows at the live value (a hole's cylinder, a counterbore),
//! translucent with an outline, long before the part's booleans are redone.
//!
//! An independent strand on the task pool builds only the tool
//! objects from the program with the live value applied ([`fab_model::eval::Evaluator::object_meshes`]),
//! takes each into the plate frame of the part it cuts and finds its feature edges (a helical
//! tool shows as its film alone). Newest request wins: a drag never queues up stale ghosts.

use fab::params::{AreaKind, GhostBody, GhostView, MarkedArea};
use fab_model::params::{self, PValue, PartParams, ToolRole};
use fab_model::program::{part_id, split_id};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;
use makepad_widgets::makepad_platform::thread::{Class, Strand, Version};

/// What a ghost is built from.
pub struct GhostRequest {
    pub gen: u64,
    pub program: Arc<fab_model::program::Compiled>,
    pub model: Arc<Vec<PartParams>>,
    pub key: String,
    /// None when the committed selected cutter is requested without a parameter edit.
    pub value: Option<PValue>,
    /// A selected tool (`instance/solid`) takes precedence over the parameter's tool set.
    pub solid: Option<String>,
    /// The parts a ghost may show on, with their model-to-plate frames (row-major 3x4).
    pub frames: Vec<(String, [[f64; 4]; 3])>,
}

#[derive(Debug)]
pub struct GhostAction(pub u64, pub Option<Arc<GhostView>>);

/// What a plate's marked areas are built from: the program, and the parts with their
/// model-to-plate frames (row-major 3x4).
pub struct AreasRequest {
    pub gen: u64,
    pub program: Arc<fab_model::program::Compiled>,
    pub frames: Vec<(String, [[f64; 4]; 3])>,
}

#[derive(Debug)]
pub struct AreasAction(pub u64, pub Arc<Vec<MarkedArea>>);

/// One step of a part's build to show: the part's mesh as it was at `solid`, built off the UI
/// thread from the program the plate was made from.
pub struct StepRequest {
    pub gen: u64,
    pub program: Arc<fab_model::program::Compiled>,
    pub model: Arc<Vec<PartParams>>,
    pub value: Option<(String, PValue)>,
    /// The plate the part is on: its whole mesh and frame, and what else is shown.
    pub plate: Arc<fab_model::eval::Evaluated>,
    /// The scene's name.
    pub name: String,
    pub instance: String,
    /// The step's solid in the instance's script.
    pub solid: String,
    /// The placed part, an index into `plate`'s parts.
    pub part: usize,
    /// The step's number and the part's step count, for the log.
    pub n: usize,
    pub of: usize,
}

impl StepRequest {
    fn render(&self, eval: &mut fab_model::eval::Evaluator, cancelled: &dyn Fn() -> bool) -> Result<fab_plate::RenderMesh, String> {
        let inst = self.program.instance(&self.instance).ok_or_else(|| format!("no instance {}", self.instance))?;
        let frame = self.plate.placed_frame(self.part).ok_or("the placed part is gone")?;
        let render = |eval: &mut fab_model::eval::Evaluator, script: &fab_model::program::Script| {
            let i = script.program.solids.iter().position(|s| s.id == self.solid).ok_or_else(|| format!("no solid {}", self.solid))?;
            eval.step_render(script, i, frame, Some(cancelled))
        };
        match &self.value {
            Some((key, value)) => render(eval, &params::with_value(&self.program, &self.model, key, value)?),
            None => render(eval, &inst.script),
        }
    }
}

/// A step's plate and scene, ready to show.
#[derive(Debug)]
pub struct StepShown {
    pub gen: u64,
    pub n: usize,
    pub of: usize,
    pub plate: Arc<fab_model::eval::Evaluated>,
    pub scene: Arc<fab::model::Scene>,
    pub mesh_ms: f64,
    pub scene_ms: f64,
}

pub struct GhostWorker {
    strand: Strand<fab_model::eval::Evaluator>,
    version: Version,
    /// The areas' own: a ghost never drops them, nor they a ghost.
    areas: Version,
    /// The step requests' own: they replace only each other.
    steps: Version,
}

impl GhostWorker {
    pub fn start(cx: &Cx, machine: fab_plate::machine::Machine) -> GhostWorker {
        let mut eval = fab_model::eval::Evaluator::new(machine);
        eval.memory_guard = Some(crate::live_slice::model_memory_guard());
        GhostWorker {
            strand: Strand::with_state(&cx.task_pool(), "fab ghost", eval),
            version: Version::new(),
            areas: Version::new(),
            steps: Version::new(),
        }
    }

    /// Replace queued area builds with this plate's.
    pub fn areas(&self, req: AreasRequest) {
        self.strand.submit(Class::Interactive, self.areas.next(), move |eval| {
            let areas = build_areas(eval, &req);
            Cx::post_action(AreasAction(req.gen, Arc::new(areas)));
        }).detach();
    }

    /// Replace queued step builds with this one. The running one stops at its next check once a
    /// newer request is queued; a stale or failed build posts nothing.
    pub fn step(&self, req: StepRequest) {
        let ticket = self.steps.next();
        let stale = ticket.clone();
        self.strand.submit(Class::Interactive, ticket, move |eval| {
            let started = std::time::Instant::now();
            let cancelled = || stale.is_stale();
            let render = match req.render(eval, &cancelled) {
                Ok(render) => render,
                Err(e) => {
                    if !stale.is_stale() {
                        log!("fab steps: step {}/{} of {} failed: {e}", req.n, req.of, req.instance);
                    }
                    return;
                }
            };
            if stale.is_stale() {
                return;
            }
            let mesh_ms = started.elapsed().as_secs_f64() * 1e3;
            let shown = req.plate.with_step(req.part, render);
            let scene = crate::plate_scene::build_step(&shown, &req.name, req.part, &req.plate.render[req.part]);
            let scene_ms = started.elapsed().as_secs_f64() * 1e3 - mesh_ms;
            if stale.is_stale() {
                return;
            }
            Cx::post_action(StepShown { gen: req.gen, n: req.n, of: req.of, plate: Arc::new(shown), scene, mesh_ms, scene_ms });
        }).detach();
    }

    /// Drop the step being built and any queued: the view went back to the whole part.
    pub fn cancel_steps(&self) {
        self.steps.bump();
    }

    /// Replace queued ghosts; publish the running ghost before taking the newest.
    pub fn request(&self, req: GhostRequest) {
        self.strand.submit(Class::Interactive, self.version.next(), move |eval| {
            let ghost = build(eval, &req);
            Cx::post_action(GhostAction(req.gen, ghost.map(Arc::new)));
        }).detach();
    }
}

fn build(eval: &mut fab_model::eval::Evaluator, req: &GhostRequest) -> Option<GhostView> {
    let instance = split_id(&req.key)?.0;
    let inst = req.program.instance(instance)?;
    let tools = match &req.solid {
        Some(object) => {
            let id = split_id(object)?.1;
            inst.script.program.solids.iter().filter(|s| s.id == id || s.id.strip_prefix(id).is_some_and(|tail| tail.starts_with('[')))
                .map(|s| (s.id.clone(), ToolRole::Subtract)).collect()
        }
        None => {
            let p = req.model.iter().flat_map(|pp| pp.params.iter()).find(|p| p.key == req.key)?;
            params::tool_objects(&req.program, p)
        }
    };
    if tools.is_empty() {
        return None;
    }
    let live = req.value.as_ref().map(|value| params::with_value(&req.program, &req.model, &req.key, value)).transpose().ok()?;
    let script = live.as_ref().unwrap_or(&inst.script);
    let names: Vec<String> = tools.iter().map(|(o, _)| o.clone()).collect();
    let meshes = eval.object_meshes(script, &names, None).ok()?;
    let mut bodies = Vec::new();
    for (object, pos, tris) in meshes {
        let role = tools.iter().find(|(o, _)| *o == object).map(|(_, r)| *r).unwrap_or(ToolRole::Subtract);
        let helical = params::is_helical(&req.program, instance, &object);
        // A script's objects share its parameters: each tool shows on the parts it belongs to.
        let marked = fab_model::program::area_solids(&script.program);
        let frames = if script.program.solids.iter().position(|s| s.id == object).is_some_and(|i| marked.contains(&i)) {
            // An area the script marks (no trees, print settings) goes with every part of it.
            req.frames.iter().filter(|(o, _)| split_id(o).is_some_and(|(i, _)| i == instance)).map(|(_, f)| f).collect()
        } else if req.solid.is_some() {
            // Source ancestry survives bypass, unlike the evaluated graph's inputs.
            let mut ancestors = Vec::new();
            let mut node = script.program.solid(&object);
            while let Some(s) = node {
                ancestors.push(fab_model::program::part_id(instance, &s.id));
                node = s.parent.map(|i| &script.program.solids[i]);
            }
            req.frames.iter().filter(|(o, _)| ancestors.contains(o)).map(|(_, f)| f).collect()
        } else {
            params::tool_frames(&req.model, instance, &object, &req.frames)
        };
        for frame in frames {
            let m: &[[f64; 4]] = &frame[..];
            let positions: Vec<[f64; 3]> = pos.iter().map(|q| params::xf_point(m, *q)).collect();
            let edges = if helical { Vec::new() } else { feature_edges(&positions, &tris) };
            bodies.push(GhostBody { subtract: role == ToolRole::Subtract, positions, triangles: tris.clone(), edges });
        }
    }
    Some(GhostView { key: req.key.clone(), bodies, fade_from: None })
}

/// The areas the scripts mark (`m.no_trees`, `m.settings`): each marked solid's mesh on every
/// part of its script, with its outline.
fn build_areas(eval: &mut fab_model::eval::Evaluator, req: &AreasRequest) -> Vec<MarkedArea> {
    let mut out = Vec::new();
    for inst in &req.program.instances {
        let p = &inst.script.program;
        let settings = fab_model::program::settings_solids(p);
        let names: Vec<String> = fab_model::program::area_solids(p).into_iter().filter(|i| p.solids[*i].parent.is_none()).map(|i| p.solids[i].id.clone()).collect();
        if names.is_empty() {
            continue;
        }
        let Ok(meshes) = eval.object_meshes(&inst.script, &names, None) else { continue };
        let frames: Vec<&[[f64; 4]; 3]> = req.frames.iter().filter(|(o, _)| split_id(o).is_some_and(|(i, _)| i == inst.name)).map(|(_, f)| f).collect();
        for (object, pos, tris) in meshes {
            let bodies = frames.iter().map(|frame| {
                let m: &[[f64; 4]] = &frame[..];
                let positions: Vec<[f64; 3]> = pos.iter().map(|q| params::xf_point(m, *q)).collect();
                let edges = feature_edges(&positions, &tris);
                GhostBody { subtract: true, positions, triangles: tris.clone(), edges }
            }).collect();
            let kind = if p.solids.iter().position(|s| s.id == object).is_some_and(|i| settings.contains(&i)) { AreaKind::Settings } else { AreaKind::NoTrees };
            let follows = p.solid(&object).is_some_and(|s| s.proto == "Offset");
            out.push(MarkedArea { object: part_id(&inst.name, &object), kind, follows, bodies, rows: Vec::new() });
        }
    }
    out
}

/// Edges where the surface folds by more than ~25 degrees, and open edges: a tool's outline
/// (a cylinder's rims, a box's edges) without its tessellation.
fn feature_edges(pos: &[[f64; 3]], tris: &[[u32; 3]]) -> Vec<[u32; 2]> {
    let normal = |t: &[u32; 3]| {
        let (a, b, c) = (pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12);
        [n[0] / l, n[1] / l, n[2] / l]
    };
    let mut faces: HashMap<(u32, u32), Vec<usize>> = HashMap::with_capacity(tris.len() * 3 / 2);
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            faces.entry((a.min(b), a.max(b))).or_default().push(i);
        }
    }
    let cos = 25f64.to_radians().cos();
    let mut out: Vec<[u32; 2]> = faces
        .into_iter()
        .filter(|(_, f)| {
            if f.len() != 2 {
                return true;
            }
            let (n0, n1) = (normal(&tris[f[0]]), normal(&tris[f[1]]));
            n0[0] * n1[0] + n0[1] * n1[1] + n0[2] * n1[2] < cos
        })
        .map(|(e, _)| [e.0, e.1])
        .collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod step_tests {
    use super::*;

    #[test]
    fn command_draft_renders_the_rebound_step_in_the_committed_placement() {
        let machine = fab_plate::machine::Machine::dual_nozzle_350();
        let mut session = fab_ai::Session::new(machine.clone(), std::env::temp_dir());
        let file = "block.cad.splash";
        let src = format!("{}let left = m.brep.box({{size: [20mm 20mm 20mm]}})\nlet right = m.brep.box({{size: [10mm 10mm 10mm] at: [40mm 0 0]}})\nlet legs = m.brep.union(left, right)\nobject(legs)\n", fab_model::program::EMPTY);
        let plate = session.replace_model(fab_model::FabModel::with_script("block", file, &src), None).unwrap();
        let whole = plate.render[0].indices.clone();
        let compiled = session.compiled.clone().unwrap();
        let (draft, _, model) = session.preview_changes(&[fab_model::Change::Script { file: file.into(), edit: makepad_model_script::lang::edit::Edit::After { solid: "left".into(), code: "m.brep.fillet(left, 1mm)".into() } }], None).unwrap();
        let mut eval = fab_model::eval::Evaluator::new(machine);
        let mut req = StepRequest { gen: 1, program: compiled, model, value: None, plate, name: "block".into(), instance: "block".into(), solid: "left".into(), part: 0, n: 1, of: 2 };
        let original = req.render(&mut eval, &|| false).unwrap();
        req.program = draft;
        let rounded = req.render(&mut eval, &|| false).unwrap();
        assert!(rounded.indices.len() > original.indices.len(), "the draft resolves left at its new index and shows the added fillet");
        assert_eq!(req.plate.render[0].indices, whole, "the committed whole-part mesh remains the ghost through the drag");
    }
}
