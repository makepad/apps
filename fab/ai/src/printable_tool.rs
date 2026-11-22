//! Imported-mesh repair on the session's model worker. Assets are immutable: undo
//! restores the old script reference, while both meshes remain available to redo.

use crate::{img2mesh, inspect, out::*, printable, Change, Edit, PlaceArg, RenderRequest, Reply, Session};
use fab_model::program::Compiled;
use makepad_csg_graph::{Graph, NodeId, Op};
use makepad_model::json::Value;
use makepad_model_script::lang::csg_sdf::mesh_field::MeshField;
use std::{ops::Range, sync::Arc};

/// Keep unrelated part overrides and write only figure defaults the part does not inherit.
pub fn print_settings(existing: &[(String, String)], inherited: &dyn Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let mut settings = existing.to_vec();
    for &(key, value) in img2mesh::PRINT_DEFAULTS {
        settings.retain(|(k, _)| k != key);
        if !inherited(key).is_some_and(|v| fab_model::project::same_setting(&v, value)) {
            settings.push((key.into(), value.into()));
        }
    }
    settings
}

fn stats_value(stats: &printable::Stats) -> S {
    obj(vec![
        ("triangles", int(stats.triangles)), ("volume_mm3", n(stats.volume_mm3)),
        ("open_edges", int(stats.open_edges)), ("nonmanifold_edges", int(stats.nonmanifold_edges)),
        ("shells", int(stats.shells)), ("size", mm3(stats.size)),
    ])
}

pub fn report_value(report: &printable::Report) -> S {
    obj(vec![
        ("before", stats_value(&report.before)), ("after", stats_value(&report.after)),
        ("closing_radius_mm", n(report.closing_radius_mm)), ("cell_mm", n(report.cell_mm)),
        ("holes_capped", int(report.loops_capped)), ("islands_removed", int(report.islands_removed)),
        ("thickened_mm3", n(report.thickened_mm3)), ("base_cut_mm", n(report.base_cut_mm)),
        ("base_area_mm2", n(report.base_area_mm2)),
        ("rotated", opt(report.rotated.map(|(axis, angle)| obj(vec![("axis", arr(axis.into_iter().map(n))), ("angle", n(angle))])))),
        ("overhang_before_mm2", n(report.overhang_before_mm2)), ("overhang_after_mm2", n(report.overhang_after_mm2)),
        ("scale", n(report.scale)), ("notes", arr(report.notes.iter().map(s))),
    ])
}

struct Imported {
    instance: String,
    object: String,
    file: String,
    asset: String,
    name_span: Range<usize>,
    scale: [f64; 3],
    settings: Vec<(String, String)>,
}

/// Follow placements to the original mesh, retaining the evaluated Size/scale.
fn mesh_node(graph: &Graph, mut node: NodeId) -> Option<(NodeId, [f64; 3])> {
    let mut scale = [1.0; 3];
    loop {
        match &graph.node(node).op {
            Op::Transform { source, scale: factor, .. } => {
                for i in 0..3 { scale[i] *= factor[i]; }
                node = *source;
            }
            Op::Mesh { .. } => return Some((node, scale)),
            _ => return None,
        }
    }
}

fn imported(compiled: &Compiled, object: &str) -> Option<Imported> {
    let (instance, emitted) = compiled.object(object)?;
    let program = &instance.script.program;
    let part = compiled.part(object).map_or(emitted.solid, |(_, part)| part);
    let (node, mut scale) = mesh_node(&program.graph, program.solids.get(part)?.node)?;
    let solid = program.solids.iter().find(|solid| solid.proto == "Import" && mesh_node(&program.graph, solid.node).is_some_and(|(n, _)| n == node))?;
    let loc = makepad_model_script::lang::edit::part_call(program, solid).ok()?;
    let (_, args) = program.names.syntax.node(&loc)?.as_call()?;
    let name = args.first()?;
    let asset = name.string()?.to_string();
    let placed = instance.placed_of(part)?;
    // the placement's factors are along the printer's X, Y, Z: the model's X, Z, Y
    let by = [placed.scale[0], placed.scale[2], placed.scale[1]];
    for i in 0..3 { scale[i] *= by[i]; }
    if scale.iter().any(|s| !s.is_finite() || *s <= 0.0) { return None; }
    Some(Imported {
        instance: instance.name.clone(), object: emitted.name.clone(), file: instance.script.file.clone(),
        asset, name_span: name.span.clone(), scale, settings: placed.settings.clone(),
    })
}

/// A source-backed imported mesh (including its transform/Size), eligible for repair.
pub fn is_imported_mesh(compiled: &Compiled, object: &str) -> bool {
    imported(compiled, object).is_some()
}

fn options(value: &Value) -> Result<printable::Options, String> {
    let mut opts = printable::Options { height_mm: None, ..Default::default() };
    for key in ["nozzle_mm", "min_feature_mm", "cell_mm", "height_mm"] {
        let number = match value.get(key) {
            None | Some(Value::Null) => continue,
            Some(Value::F64(v)) => *v,
            Some(Value::Int(v)) => *v as f64,
            _ => return Err(format!("'{key}' is a positive number in mm")),
        };
        if !number.is_finite() || number <= 0.0 { return Err(format!("'{key}' is a positive number in mm")); }
        match key {
            "nozzle_mm" => opts.nozzle_mm = number,
            "min_feature_mm" => opts.min_feature_mm = number,
            "cell_mm" => opts.cell_mm = Some(number),
            "height_mm" => opts.height_mm = Some(number),
            _ => unreachable!(),
        }
    }
    for (key, target) in [("orient", &mut opts.orient), ("flat_base", &mut opts.flat_base)] {
        match value.get(key) {
            None | Some(Value::Null) => {},
            Some(Value::Bool(v)) => *target = *v,
            _ => return Err(format!("'{key}' is true or false")),
        }
    }
    if let Some(v) = value.get("max_triangles").filter(|v| !matches!(v, Value::Null)) {
        opts.max_triangles = match v {
            Value::Int(v) if *v > 0 => usize::try_from(*v).map_err(|_| "'max_triangles' is too large")?,
            _ => return Err("'max_triangles' is a positive whole number".into()),
        };
    }
    Ok(opts)
}

impl Session {
    /// Read inherited settings from the plate first, then the selected process.
    pub(crate) fn printable_settings(&mut self, existing: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
        let process = if let Some(setup) = self.print_setup.clone() {
            if self.last.is_none() { self.rebuild()?; }
            let ev = self.last.as_ref().ok_or("the plate has not been evaluated")?;
            Some(setup(ev)?.0.process)
        } else {
            None
        };
        let plate = &self.model.project().settings;
        Ok(print_settings(existing, &|key| {
            plate.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
                .or_else(|| process.as_ref()?.values.get(key).map(|v| v.at(0).to_string()))
        }))
    }

    /// Repair the asset at this part's current scale; replace only its import reference
    /// and print defaults in one transaction. No mesh work runs on the UI thread.
    pub fn make_printable(&mut self, object: &str, opts: Value) -> Reply {
        if self.compiled.is_none() || self.last.is_none() {
            if let Err(error) = self.rebuild() { return Reply::err(error); }
        }
        let Some(target) = self.compiled.as_ref().and_then(|c| imported(c, object)) else {
            return Reply::err("choose a mesh part made by m.import(\"name\", ...) and its transforms");
        };
        let opts = match options(&opts) { Ok(opts) => opts, Err(error) => return Reply::err(error) };
        let settings = match self.printable_settings(&target.settings) { Ok(settings) => settings, Err(error) => return Reply::err(error) };
        let Some(asset) = self.model.assets.get(&target.asset) else { return Reply::err("the imported mesh asset is missing") };
        let mut mesh = asset.data.clone();
        for p in &mut mesh.positions {
            for i in 0..3 { p[i] = (p[i] as f64 * target.scale[i]) as f32; }
        }
        let started = std::time::Instant::now();
        let (mut repaired, report) = match printable::make_printable(&mesh, &opts) {
            Ok(result) => result,
            Err(error) => return Reply::err(error),
        };
        let seconds = started.elapsed().as_secs_f64();
        // Preserve the script's Size expression and placement scale: store the repair
        // back in asset units so neither is applied twice when the script rebuilds.
        for p in &mut repaired.positions {
            for i in 0..3 { p[i] = (p[i] as f64 / target.scale[i]) as f32; }
        }
        let model = &self.model;
        let name = img2mesh::free_name(&format!("{}_printable", target.asset), &|n| model.assets.contains_key(n) || model.source(&fab_model::script_file(n)).is_some() || model.project().scripts.iter().any(|s| s.name == n));
        let previous = self.model.assets.clone();
        let mut assets = (*previous).clone();
        assets.insert(name.clone(), Arc::new(MeshField::new(repaired)));
        self.model.assets = Arc::new(assets);
        let reply = self.commit("make_printable", vec![
            Change::Script { file: target.file.clone(), edit: Edit::Splice { range: target.name_span, text: quote(&name) } },
            Change::Place { instance: target.instance, object: target.object, arg: PlaceArg::Settings(settings) },
        ]);
        if !reply.ok { self.model.assets = previous; return reply; }
        let Some(ev) = self.last.clone() else { return reply };
        let line_width = self.eval.materials.line_width;
        let check_mesh = inspect::check_mesh(&Value::Obj(vec![("object".into(), Value::Str(object.into()))]), &ev, &mut self.eval, line_width)
            .unwrap_or_else(|error| obj(vec![("error", s(error))]));
        let check = fab_printability::analyse_plate(&ev.machine, &ev.plate, &self.eval.materials);
        let fields = obj(vec![
            ("imported", s(&name)), ("script", s(&target.file)), ("objects", arr([s(object)])),
            ("printable", report_value(&report)), ("check_mesh", check_mesh), ("check", crate::check_value(&check)),
            ("stages", arr([obj(vec![("stage", s("making it printable")), ("seconds", n(seconds))])])),
            ("head", s(self.model.head().text())),
        ]);
        Reply { ok: true, text: text(&fields), images: Vec::new(), update: reply.update,
            render: Some(RenderRequest::Sheets { views: Vec::new(), objects: vec![object.into()], extra: None, camera: crate::Camera::default(), marks: Vec::new() }) }
    }
}
