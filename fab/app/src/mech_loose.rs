//! The Mechanics view's Supports scene: the plate's planned supports as loose pieces around its
//! parts. The plan is the plate's slice, the one the Slice view shows: the check waits for it and
//! never plans one of its own. The parts are held and lift off the plate, as they do when a print
//! comes off: what only stood there stays behind, what's caught in them comes along. A shake moves them and the pieces rattle. Each piece's verdict comes from a low-priority search that runs behind the sim.

use crate::mechanics::Built;
use fab::api::{MechLight, MechPieceView};
use fab_loose::bodies::{piece_body, with_field};
use fab_loose::grid::{self, DistGrid};
use fab_loose::mesh::PieceMesh;
use fab_loose::pieces::{coarse, pieces_of, Piece};
use fab_loose::solid::Solid;
use fab_loose::verdict::{check_piece, Light, PieceVerdict};
use fab_model::eval::Evaluated;
use makepad_csg_mech::sim::{AnchorKind, AnchorSpec, Assembly, GRAVITY};
use makepad_fab_slice::live::PlatePaths;
use makepad_fab_toolpath::par;
use makepad_fab_toolpath::skeleton::Skeleton;
use makepad_widgets::makepad_platform::thread::Class;
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

/// Contacts are made this close (mm).
const CONTACT_MARGIN: f64 = 1.5;
/// mm/s: what a hand shaking a part does (5 Hz, +-50 mm peaks near 1600). A ball's way against the
/// part's field is swept, so it can't go through a wall in a step at this speed.
const MAX_SPEED: f64 = 2000.0;
/// The shake's side to side, against the default shake's: over 1 g, to lift the pieces.
const SHAKE_TEMPO: f64 = 2.5;
/// The parts rise this far past the tallest piece (mm): what only stood on the plate is left clear of them.
const LIFT_CLEAR: f64 = 5.0;
const SAYS_ONCE: &str = "This shows supports after they have broken free. It says nothing about how hard that is.";

/// What the check needs once the sim has started.
pub struct Loose {
    parts: Vec<(Arc<Solid>, Arc<DistGrid>)>,
    /// (part, piece), in the sim's body order after the parts.
    pieces: Vec<(usize, Piece)>,
    ev: Arc<Evaluated>,
    name: String,
}

/// A running check. Dropping it stops the search.
pub struct LooseRun {
    loose: Arc<Loose>,
    results: Vec<Option<PieceVerdict>>,
    verdicts: Receiver<(usize, PieceVerdict)>,
    stop: Arc<AtomicBool>,
    recolour: Option<Receiver<Arc<fab::model::Scene>>>,
}

impl Drop for LooseRun {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn light(v: &PieceVerdict) -> MechLight {
    match v.verdict.colour() {
        Light::Green => MechLight::Green,
        Light::Amber => MechLight::Amber,
        Light::Red => MechLight::Red,
    }
}

fn colour(light: MechLight) -> [f32; 4] {
    match light {
        MechLight::Waiting => [0.72, 0.70, 0.62, 1.0],
        MechLight::Green => [0.36, 0.72, 0.42, 1.0],
        MechLight::Amber => [0.93, 0.68, 0.22, 1.0],
        MechLight::Red => [0.88, 0.26, 0.22, 1.0],
    }
}

/// The part as a distance on the plate: its field when it is one, else its mesh.
fn solid_of(ev: &Evaluated, i: usize) -> Result<Solid, String> {
    if let Some(field) = ev.fields.get(i).and_then(Option::as_ref) {
        let offset = ev.frames.get(i).map_or([0.0; 3], |f| f.offset);
        let bounds = field.bounds.as_ref().map(|b| (std::array::from_fn(|k| b.min[k] + offset[k]), std::array::from_fn(|k| b.max[k] + offset[k])));
        let (shape, _, to_printer, _) = field.distance();
        return Ok(Solid::of_field(shape, *to_printer, bounds));
    }
    let mesh = &ev.plate.parts[i].mesh;
    if mesh.triangles.is_empty() {
        return Err(format!("{} has no solid geometry to check", ev.plate.parts[i].name));
    }
    Ok(Solid::of_mesh(mesh.positions.clone(), mesh.triangles.clone()))
}

/// The sim of the plate's supports from `paths`, the plate's slice, on a worker. `stop`: the work
/// is no longer wanted.
pub fn build(ev: &Arc<Evaluated>, paths: &PlatePaths, name: &str, stop: &(dyn Fn() -> bool + Sync)) -> Result<(Built, Loose), String> {
    // Each part's skeleton and where its plan sits on the plate.
    let skeletons: Vec<(Arc<Skeleton>, [f64; 2])> = paths.skeletons().into_iter().map(|(s, at)| (s, [at.x_mm(), at.y_mm()])).collect();
    // A tree's warps are pads with no chain: what prints is its chains, pads and slabs.
    if skeletons.iter().all(|(s, _)| s.chains.is_empty() && s.slabs.is_empty() && s.roofs.iter().all(Vec::is_empty)) {
        return Err("This plate's settings print no supports".into());
    }
    let t = std::time::Instant::now();
    let mut parts = Vec::new();
    let mut pieces: Vec<(usize, Piece)> = Vec::new();
    for (i, part) in ev.plate.parts.iter().enumerate() {
        let solid = Arc::new(solid_of(ev, i)?);
        // an exact grid's build says how far it is, for the status line
        let phase = |step: &str, _: Option<(u32, u32)>, done: f64| {
            *crate::mechanics::GRID_PROGRESS.lock().unwrap() = Some(format!("Building {}'s exact contact geometry: {step}, {:.0}%…", part.name, 100.0 * done));
        };
        let ctl = par::Ctl { cancel: Some(stop), phase: Some(&phase), ..Default::default() };
        let grid = Arc::new(DistGrid::check(ctl, &solid, ev.profile.layer, grid::contact()).map_err(|_| "cancelled".to_string())?);
        let (skeleton, at) = &skeletons[i];
        pieces.extend(pieces_of(&part.object, skeleton, *at, &grid).into_iter().map(|p| (i, p)));
        parts.push((solid, grid));
    }
    coarse(&mut pieces.iter_mut().map(|(_, p)| p).collect::<Vec<_>>());
    let density = ev.profile.material.density;
    let mut asm = Assembly { up: [0.0, 0.0, 1.0], sleep: makepad_csg_mech::sim::SLEEP_SPEED, contact_margin: CONTACT_MARGIN, max_speed: MAX_SPEED, shake_tempo: SHAKE_TEMPO, contact_layer: grid::contact().layer(), ..Default::default() };
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    let mut bodies = Vec::new();
    for (i, part) in ev.plate.parts.iter().enumerate() {
        let body = with_field(crate::mechanics::free_body(ev, i, Some(paths))?, &parts[i].1);
        asm.anchors.push(AnchorSpec { body: asm.bodies.len(), pivot: body.mass.com, kind: AnchorKind::Fixed, friction: Default::default() });
        asm.bodies.push(body);
        bodies.push((part.object.clone(), crate::mechanics::IDENTITY, false));
        for k in 0..3 {
            lo[k] = lo[k].min(parts[i].0.lo[k]);
            hi[k] = hi[k].max(parts[i].0.hi[k]);
        }
    }
    asm.shake_about = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    let mut tallest: f64 = 0.0;
    for (_, piece) in &pieces {
        asm.bodies.push(piece_body(piece, density));
        bodies.push((piece.name.clone(), crate::mechanics::IDENTITY, false));
        tallest = piece.balls.iter().map(|b| (b.c[2] + b.r) as f64).fold(tallest, f64::max);
    }
    asm.lift = tallest + LIFT_CLEAR;
    let balls: usize = pieces.iter().map(|(_, p)| p.balls.len()).sum();
    let built = t.elapsed().as_secs_f64();
    let shown: Vec<(String, &PieceMesh, [f32; 4])> = pieces.iter().map(|(_, p)| (p.name.clone(), &p.mesh, colour(MechLight::Waiting))).collect();
    let scene = crate::plate_scene::build_loose(ev, name, &shown);
    log!("fab loose: {} pieces, {balls} balls: grids and pieces {built:.2} s", pieces.len());
    let verdict = format!("{} support piece{}. {SAYS_ONCE}", pieces.len(), if pieces.len() == 1 { "" } else { "s" });
    Ok((
        Built { hold: crate::mechanics::m4(&crate::mechanics::IDENTITY), asm, gravity: [0.0, 0.0, -GRAVITY], floor: Some(-0.05), bodies, verdict, joints_ok: Vec::new(), scene: Some(scene), ball: None, riders: None, frame: None, fields: Vec::new() },
        Loose { parts, pieces, ev: ev.clone(), name: name.to_string() },
    ))
}

impl LooseRun {
    /// Start the check of each piece, at low priority.
    pub fn start(cx: &mut Cx, loose: Loose) -> LooseRun {
        let loose = Arc::new(loose);
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        for i in 0..loose.pieces.len() {
            let (loose, stop, tx) = (loose.clone(), stop.clone(), tx.clone());
            // A closed pool (the app ends) drops the job.
            if let Ok(task) = cx.task_pool().submit_internal(Class::Background, move || {
                let (part, piece) = &loose.pieces[i];
                let (solid, grid) = &loose.parts[*part];
                if let Ok(v) = check_piece(piece, grid, solid, &|| stop.load(Ordering::Relaxed)) {
                    let _ = tx.send((i, v));
                    SignalToUI::set_ui_signal();
                }
            }) {
                task.detach();
            }
        }
        LooseRun { results: vec![None; loose.pieces.len()], loose, verdicts: rx, stop, recolour: None }
    }

    /// The pieces as the panel shows them.
    pub fn rows(&self) -> Vec<MechPieceView> {
        self.loose.pieces.iter().zip(&self.results).map(|((i, p), v)| {
            let widest = p.balls.iter().max_by(|a, b| a.r.total_cmp(&b.r)).map_or([0.0; 3], |b| b.c);
            let part = &self.loose.ev.plate.parts[*i];
            MechPieceView { name: p.name.clone(), label: p.name.replacen(&part.object, &part.name, 1), light: v.as_ref().map_or(MechLight::Waiting, light), words: v.as_ref().map_or(String::new(), |v| v.verdict.words().to_string()), grip: widest.map(|c| c * 0.001) }
        }).collect()
    }

    /// Take the check's news: rows changed (true), and a scene with the pieces coloured once
    /// all of them have their verdict.
    pub fn poll(&mut self, cx: &mut Cx) -> (bool, Option<Arc<fab::model::Scene>>) {
        let mut news = false;
        while let Ok((i, v)) = self.verdicts.try_recv() {
            self.results[i] = Some(v);
            news = true;
        }
        if news && self.results.iter().all(Option::is_some) {
            let (loose, results) = (self.loose.clone(), self.results.clone());
            let (tx, rx) = channel();
            if let Ok(task) = cx.task_pool().submit_internal(Class::Visible, move || {
                let shown: Vec<(String, &PieceMesh, [f32; 4])> = loose.pieces.iter().zip(&results).map(|((_, p), v)| (p.name.clone(), &p.mesh, colour(v.as_ref().map_or(MechLight::Waiting, light)))).collect();
                let _ = tx.send(crate::plate_scene::build_loose(&loose.ev, &loose.name, &shown));
                SignalToUI::set_ui_signal();
            }) {
                task.detach();
                self.recolour = Some(rx);
            }
        }
        let scene = self.recolour.as_ref().and_then(|rx| rx.try_recv().ok());
        if scene.is_some() {
            self.recolour = None;
        }
        (news, scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_loose::verdict::check_piece;
    use fab_model::eval::Evaluator;
    use fab_model::FabModel;
    use fab_plate::machine::Machine;
    use std::collections::BTreeMap;

    /// A small demo lattice, with its supports as `kind` plans them.
    /// At this size all its overhang is inside it, so they are asked to stand there.
    fn lattice(kind: &str) -> Arc<Evaluated> {
        // Tree supports need the larger internal overhang to produce a piece.
        let size = if kind == "normal(auto)" { 8 } else { 12 };
        let project = format!("use mod.fab.*\nsettings({{enable_support: \"1\" support_type: \"{kind}\" support_threshold_angle: \"45\" fab_support_inside: \"1\"}})\nscript(\"lattice_cube.cad.splash\", params: {{lattice_size: {size}mm lattice_period: 6mm}})\nplace(\"lattice_cube\", object: \"lattice\", at: [100mm 100mm], name: \"Lattice\")\n");
        let scripts = BTreeMap::from([("lattice_cube.cad.splash".to_string(), include_str!("../resources/lattice_cube.cad.splash").to_string())]);
        let m = FabModel::load("lattice", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        Arc::new(m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1)
    }

    fn checked(kind: &str) {
        let ev = lattice(kind);
        let (built, loose) = build(&ev, &crate::live_slice::sliced(&ev), "lattice", &|| false).unwrap_or_else(|f| panic!("{kind}: {f}"));
        assert!(!loose.pieces.is_empty(), "{kind}: the lattice needs support");
        assert_eq!(built.asm.bodies.len(), 1 + loose.pieces.len(), "{kind}: the part and one body a piece");
        for (part, piece) in &loose.pieces {
            let (solid, grid) = (&loose.parts[*part].0, &loose.parts[*part].1);
            assert!(check_piece(piece, grid, solid, &|| false).is_ok(), "{kind}: {}", piece.name);
        }
    }

    #[test]
    fn a_lattice_on_tree_supports_checks() {
        checked("tree(auto)");
    }

    /// Its trees hold the lattice's bars wherever a tip may stand, their low points among them.
    /// What is left, in the tip's clearance along the wave's flank and the pillar, Printability
    /// tells as out of any tree's reach, or as closed in by the part where not even the thinnest
    /// column comes near it: together, all that is not held.
    #[test]
    fn a_lattice_on_tree_supports_is_held_where_a_tip_may_stand() {
        let ev = lattice("tree(auto)");
        let paths = crate::live_slice::sliced(&ev);
        let plan = &paths.plans()[0].0;
        // What the trees tell follows their paths: measured here when it is not in yet.
        let report = plan.skeleton.tell(par::Ctl::default()).expect("nothing cancels it");
        let hold = report.hold.expect("trees measure what they hold");
        assert!(hold.held >= 4.8, "held {:.2} of {:.2} mm2", hold.held, hold.need);
        let left: f64 = report.given_up.iter().chain(&report.inside).map(|g| g.area).sum();
        assert!((hold.need - hold.held - left).abs() < 0.05, "what is not held, {:.2} mm2, is given up or closed in: {left:.2} mm2", hold.need - hold.held);
        let heights: Vec<(f64, f64)> = plan.layers.iter().map(|l| (l.0, l.1)).collect();
        let told: f64 = fab_printability::support_issues("Lattice", &report, &heights, [0.0; 2]).iter().filter(|i| matches!(i.kind, "support_unreached" | "support_inside")).map(|i| i.area_mm2).sum();
        assert!((told - left).abs() < 0.05, "Printability tells {told:.2} mm2 out of reach or closed in, of {left:.2}");
    }

    #[test]
    fn a_lattice_on_normal_supports_checks() {
        checked("normal(auto)");
    }
}
