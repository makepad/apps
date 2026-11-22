//! The Mechanics workspace's sim: joints, links, or ordinary free plate parts
//! turned into a world on a worker thread (the dynamics engine's for a jointed
//! piece, box3d's for the rest), then run live by a
//! `csg_mech::runner::Runner` (240 Hz on its own thread). Every frame the
//! newest snapshot's poses go to the view as rigid moves of the parts'
//! elements (no geometry rebuilt), its strings' paths as moves of their
//! segments' rods, and the joint angles to the panel. The panel's buttons and
//! the pointer/hand tools arrive as `MechAction`s.

use fab::api::{AppState, MechAction, MechCheckView, MechJointView, MechMotorView, MechOp, MechView, PlateMode};
use fab::model::ElementId;
use fab_elec_mech::BoardMass;
use fab_model::eval::Evaluated;
use makepad_csg_mech::mass::MassProps;
use makepad_csg_mech::runner::{Command, Event as SimEvent, Options, PoseRecord, Probe, Runner, Snapshot};
use makepad_csg_mech::sim::{Assembly, BodyAction, BodySpec, JointKind, Pose, GRAVITY};
use makepad_csg_mech::MotorKind;
use makepad_csg_mech::geom::{dot, madd, norm, scale};
use makepad_widgets::*;
use makepad_widgets::makepad_platform::thread::{Class, TaskPool};
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use fab_loose::apart::{Layers, Piece};
use makepad_fab_slice::live::PlatePaths;

pub(crate) type M4 = [[f64; 4]; 4];

/// The app's workpool as the mechanics engine's thread pool (`makepad_dynamics::thread::TaskPool`): its
/// parallel work spreads over the pool's workers with the sim thread helping, which waits for each spread.
/// A sim in view spreads at Visible; once the app is hidden, at Background, below what shows.
pub struct SimPool {
    pool: TaskPool,
    threads: usize,
    hidden: Arc<AtomicBool>,
}

impl SimPool {
    /// All the pool's workers and the sim thread; `hidden` is set while the app is hidden.
    pub fn new(pool: TaskPool, hidden: Arc<AtomicBool>) -> SimPool {
        let threads = pool.worker_count() + 1;
        SimPool { pool, threads, hidden }
    }
}

impl makepad_dynamics::thread::TaskPool for SimPool {
    fn fan_out(&self, n: usize, f: &(dyn Fn(usize) + Sync)) {
        let class = if self.hidden.load(Ordering::Relaxed) { Class::Background } else { Class::Visible };
        self.pool.fan_out_as(class, self.threads, n, f);
    }
    fn threads(&self) -> usize {
        self.threads
    }
}

pub(crate) fn m4(a: &[[f64; 4]; 3]) -> M4 {
    [a[0], a[1], a[2], [0.0, 0.0, 0.0, 1.0]]
}
pub(crate) fn mul(a: &M4, b: &M4) -> M4 {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..4).map(|k| a[i][k] * b[k][j]).sum()))
}
/// The inverse of an affine (rotation + uniform scale + translation).
pub(crate) fn inv(a: &M4) -> M4 {
    let r = [[a[0][0], a[0][1], a[0][2]], [a[1][0], a[1][1], a[1][2]], [a[2][0], a[2][1], a[2][2]]];
    let det = r[0][0] * (r[1][1] * r[2][2] - r[1][2] * r[2][1]) - r[0][1] * (r[1][0] * r[2][2] - r[1][2] * r[2][0]) + r[0][2] * (r[1][0] * r[2][1] - r[1][1] * r[2][0]);
    let id = 1.0 / det;
    let c = |i: usize, j: usize| {
        let (i1, i2) = ((i + 1) % 3, (i + 2) % 3);
        let (j1, j2) = ((j + 1) % 3, (j + 2) % 3);
        (r[j1][i1] * r[j2][i2] - r[j1][i2] * r[j2][i1]) * id
    };
    let ri: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| c(i, j)));
    let t = [a[0][3], a[1][3], a[2][3]];
    let mut out = [[0.0; 4]; 4];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = ri[i][j];
        }
        out[i][3] = -(ri[i][0] * t[0] + ri[i][1] * t[1] + ri[i][2] * t[2]);
    }
    out[3][3] = 1.0;
    out
}
/// A rotation as an axis and an angle (radians), through its quaternion: exact from none up to a half turn.
pub(crate) fn axis_angle(r: &M4) -> ([f64; 3], f64) {
    let trace = r[0][0] + r[1][1] + r[2][2];
    // the largest component first, which keeps the division sound
    let (w, v) = if trace > 0.0 {
        let k = (1.0 + trace).sqrt() * 2.0;
        (k / 4.0, [(r[2][1] - r[1][2]) / k, (r[0][2] - r[2][0]) / k, (r[1][0] - r[0][1]) / k])
    } else {
        let i = (0..3).fold(0, |b, k| if r[k][k] > r[b][b] { k } else { b });
        let (j, l) = ((i + 1) % 3, (i + 2) % 3);
        let k = (1.0 + r[i][i] - r[j][j] - r[l][l]).sqrt() * 2.0;
        let mut v = [0.0; 3];
        v[i] = k / 4.0;
        v[j] = (r[j][i] + r[i][j]) / k;
        v[l] = (r[l][i] + r[i][l]) / k;
        ((r[l][j] - r[j][l]) / k, v)
    };
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n < 1e-12 {
        return ([0.0, 0.0, 1.0], 0.0);
    }
    (v.map(|c| c / n), 2.0 * n.atan2(w))
}

pub(crate) fn apply(a: &M4, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i][0] * p[0] + a[i][1] * p[1] + a[i][2] * p[2] + a[i][3])
}

/// The rotation (row-major) taking direction `a` onto direction `b`.
fn rotation_between(a: [f64; 3], b: [f64; 3]) -> [[f64; 3]; 3] {
    use makepad_csg_mech::geom::{cross, rotate};
    let (a, b) = (norm(a), norm(b));
    let c = dot(a, b);
    let k = cross(a, b);
    let axis = if makepad_csg_mech::geom::len(k) < 1e-9 { makepad_csg_mech::geom::basis(a).0 } else { norm(k) };
    let ang = c.clamp(-1.0, 1.0).acos();
    let cols = [rotate([1.0, 0.0, 0.0], axis, ang), rotate([0.0, 1.0, 0.0], axis, ang), rotate([0.0, 0.0, 1.0], axis, ang)];
    std::array::from_fn(|i| std::array::from_fn(|j| cols[j][i]))
}

/// Plate part `i` as a free body in the printer frame (mm), whole: its mesh, or a field part's outlines in
/// the plate's slice `paths` (its mass and hulls from them: what prints).
pub(crate) fn free_body(ev: &Evaluated, i: usize, paths: Option<&PlatePaths>) -> Result<BodySpec, String> {
    let part = &ev.plate.parts[i];
    let density = ev.profile.material.density;
    let body = if part.mesh.triangles.is_empty() {
        let layers = sliced(ev, i, paths)?;
        let whole = Piece { layers: layers.regions.iter().cloned().enumerate().filter(|(_, r)| !r.is_empty()).collect() };
        outline_body(&part.name, &layers, &whole, density, 0.0)
    } else {
        makepad_csg_mech::mesh_body(&part.name, &part.mesh.positions, &part.mesh.triangles, density, 1.0, &[])
    };
    if body.hulls.is_empty() {
        return Err(format!("{} has no solid volume to simulate", part.name));
    }
    Ok(body)
}

/// A field part's outlines in the plate's slice (`paths`, which a sim with field parts waits for).
fn sliced(ev: &Evaluated, i: usize, paths: Option<&PlatePaths>) -> Result<Layers, String> {
    let part = &ev.plate.parts[i];
    if ev.plate.groups().iter().any(|g| g.contains(&i)) {
        return Err(format!("{} prints glued to the other bodies of its object: Mechanics can't move a field body of a glued object yet", part.name));
    }
    let (plan, at) = paths.and_then(|p| p.plans().get(i)).ok_or_else(|| format!("{} is not in the plate's slice", part.name))?;
    let layers = Layers::of(plan, *at);
    if layers.regions.iter().all(Vec::is_empty) {
        return Err(format!("{} has no layers in the plate's slice", part.name));
    }
    Ok(layers)
}

/// A piece of a field part as a body: its mass from its outlines (filled), its hulls from their
/// corners, lifted `over` mm.
fn outline_body(name: &str, layers: &Layers, piece: &Piece, density: f64, over: f64) -> BodySpec {
    let mut mass = piece.mass(layers, density, 1.0);
    mass.com[2] += over;
    let corners: Vec<[f64; 3]> = piece.corners(layers).into_iter().map(|p| [p[0], p[1], p[2] + over]).collect();
    BodySpec { name: name.into(), mass, hulls: makepad_csg_mech::proxy_hulls(&corners, &[]), tubes: Vec::new(), balls: Vec::new(), rods: Vec::new(), field: None }
}

/// A field part's bodies, one per piece that prints apart ([`fab_loose::apart`]): per piece its body and
/// the field it is drawn by (scene object, field, box on the plate): the part's field within the
/// piece's mask. A piece within another's box meets it through that field's distance on a grid, as a
/// mesh part's solids do; the others meet as their hulls. The part lifted `over` mm.
struct FieldPieces {
    bodies: Vec<BodySpec>,
    shown: Vec<(String, fab::api::PartSdf, ([f64; 3], [f64; 3]))>,
}

fn field_pieces(ev: &Evaluated, i: usize, layers: &Layers, pieces: &[Piece], over: f64) -> Result<FieldPieces, String> {
    let part = &ev.plate.parts[i];
    let field = ev.fields.get(i).and_then(Option::as_ref).ok_or_else(|| format!("{} has no solid geometry to simulate", part.name))?;
    let (shape, key, to_printer, _) = field.distance();
    let boxes: Vec<([f64; 3], [f64; 3])> = pieces.iter().map(|p| p.bounds(layers)).map(|(lo, hi)| ([lo[0], lo[1], lo[2] + over], [hi[0], hi[1], hi[2] + over])).collect();
    let nested: Vec<bool> = (0..pieces.len()).map(|k| boxes.iter().enumerate().any(|(j, b)| j != k && (0..3).all(|a| b.0[a] < boxes[k].1[a] && boxes[k].0[a] < b.1[a]))).collect();
    let masks = if pieces.len() > 1 { layers.masks(pieces) } else { Vec::new() };
    build_checkpoint()?;
    let lifted = |m: &[[f64; 4]; 3]| -> [[f64; 4]; 3] { [m[0], m[1], [m[2][0], m[2][1], m[2][2], m[2][3] + over]] };
    let density = ev.profile.material.density;
    // each piece's body and field on its own worker; then the grids of those within another's box
    let built = makepad_fab_toolpath::par::map(pieces.len(), |k| -> Result<(BodySpec, fab::api::PartSdf), String> {
        let name = if pieces.len() == 1 { part.name.clone() } else { format!("{} solid {}", part.name, k + 1) };
        let body = outline_body(&name, layers, &pieces[k], density, over);
        if body.hulls.is_empty() {
            return Err(format!("{name} has no solid volume to simulate"));
        }
        let drawn = match masks.get(k) {
            Some(runs) => {
                let piece = fab_loose::apart::masked(shape, to_printer, runs, pieces[k].bounds(layers))?;
                fab::api::PartSdf { key: piece.key(), shape: Arc::new(piece), to_printer: lifted(to_printer) }
            }
            None => fab::api::PartSdf { shape: shape.clone(), to_printer: lifted(to_printer), key: *key },
        };
        Ok((body, drawn))
    });
    let mut out = FieldPieces { bodies: Vec::new(), shown: Vec::new() };
    for (k, b) in built.into_iter().enumerate() {
        let (body, drawn) = b?;
        let body = if nested[k] { with_grid(ev, body, &fab_loose::solid::Solid::of_field(&drawn.shape, drawn.to_printer, Some(boxes[k])))? } else { body };
        out.bodies.push(body);
        out.shown.push((crate::plate_scene::link_object(&part.object, k), drawn, boxes[k]));
    }
    Ok(out)
}

/// A body that meets others through its solid's distance on a grid, at the printer's resolution
/// (its layer height), laid out as the contact setting says. The grid is built a batch at a time
/// across the workers, which give their worker up to waiting work between batches; it says how
/// far it is for the status line, and stops once it is no longer wanted.
fn with_grid(ev: &Evaluated, body: BodySpec, solid: &fab_loose::solid::Solid) -> Result<BodySpec, String> {
    let stop = || makepad_widgets::makepad_platform::thread::checkpoint().is_err();
    let name = body.name.clone();
    let phase = move |step: &str, _: Option<(u32, u32)>, done: f64| {
        *GRID_PROGRESS.lock().unwrap() = Some(format!("Building {name}'s exact contact geometry: {step}, {:.0}%…", 100.0 * done));
    };
    let ctl = makepad_fab_toolpath::par::Ctl { cancel: Some(&stop), phase: Some(&phase), ..Default::default() };
    let grid = fab_loose::grid::DistGrid::sim(ctl, solid, ev.profile.layer, fab_loose::grid::contact()).map_err(|_| format!("{}'s build was stopped", body.name))?;
    Ok(fab_loose::bodies::with_field(body, &Arc::new(grid)))
}

/// What an exact grid's build last said of itself, for the status line while the sim is built:
/// set by the build's workers, taken by the UI thread, each holding it for a moment.
pub(crate) static GRID_PROGRESS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The contact setting as the grids take it, and as the panel shows it.
fn contact_of(c: fab::api::MechContact) -> fab_loose::grid::Contact {
    match c {
        fab::api::MechContact::Fast => fab_loose::grid::Contact::Fast,
        fab::api::MechContact::Exact => fab_loose::grid::Contact::Exact,
    }
}

fn contact_view(c: fab_loose::grid::Contact) -> fab::api::MechContact {
    match c {
        fab_loose::grid::Contact::Fast => fab::api::MechContact::Fast,
        fab_loose::grid::Contact::Exact => fab::api::MechContact::Exact,
    }
}

/// The free parts' build gives its worker up to waiting work, and stops once it is no longer wanted.
fn build_checkpoint() -> Result<(), String> {
    makepad_widgets::makepad_platform::thread::checkpoint().map_err(|_| "The physics build was stopped".to_string())
}

/// The plate has a field part a free or chain sim moves: its outlines come from the plate's slice.
fn has_field_parts(ev: &Evaluated) -> bool {
    ev.plate.parts.iter().enumerate().any(|(i, p)| p.mesh.triangles.is_empty() && !ev.weaves.iter().any(|w| w.part == i))
}

/// Visible board placements and their unit names, in one pass over the excluded parts.
fn free_boards(ev: &Evaluated) -> impl Iterator<Item = (&str, &fab_plate::ExcludedPart)> {
    ev.plate.excluded.iter().filter(|p| !p.handles.hidden).filter_map(|p| {
        Some((p.link.as_deref()?.strip_prefix("board:")?, p))
    })
}

/// The plate's parts as free bodies in the printer frame (mm), every part liftable from the floor and
/// throwable. A part whose solids print apart (a bearing's rings and rollers, a ball in its cage) is a
/// body per solid, in the Mechanics view only: a scene of the sim's own shows the solids as objects
/// `<object>~<k>`, and the part itself is unchanged. A mesh part's solids are its mesh's ([`mesh_pieces`]);
/// a field part's are what its outlines in the plate's slice `paths` join into ([`fab_loose::apart`]),
/// each drawn by the part's field within its mask. A solid within another's box meets it through their
/// distance grids, so it turns inside them; the others meet as their hulls. A part its script holds by
/// hand ([`Evaluated::hands`]) starts in that hand a little over the bed, the hand on the solid it is in:
/// a bearing held by its outer ring turns on it, where on the bed the bed would carry its rollers (they
/// skid round on it instead of rolling). The rest start on the bed.
///
/// [`mesh_pieces`]: makepad_csg_mech::mesh_pieces
pub(crate) fn free_parts(ev: &Evaluated, name: &str, paths: Option<&PlatePaths>, masses: &BTreeMap<String, BoardMass>) -> Result<Built, String> {
    // A board is never printed, yet rides in the sim like any part where it shows: one body, its
    // parts soldered on though their meshes stand apart from the slab. Split, its pieces would show
    // in its place under the board the electronics host draws there, and a pull would carry them
    // off without it.
    let mut boards = free_boards(ev).peekable();
    if ev.plate.parts.is_empty() && boards.peek().is_none() {
        return Err("Add a part to the plate to pick it up and throw it".into());
    }
    // the slicer's closing fuses solids nearer than twice its radius: those print as one
    let fused = 2.0 * makepad_fab_slicer::slice::SliceParams::default().closing_radius;
    let mut asm = Assembly { up: [0.0, 0.0, 1.0], sleep: makepad_csg_mech::sim::SLEEP_SPEED, contact_layer: fab_loose::grid::contact().layer(), ..Default::default() };
    let (mut bodies, mut solids_shown, mut fields_shown, mut split, mut held) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (i, part) in ev.plate.parts.iter().enumerate() {
        build_checkpoint()?;
        // where its script's hands hold it, on the plate
        let to_plate = ev.model_to_plate.get(i).unwrap_or(&IDENTITY);
        let hands: Vec<[f64; 3]> = ev.hands.get(i).into_iter().flatten().map(|p| std::array::from_fn(|r| (0..3).map(|c| to_plate[r][c] * p[c]).sum::<f64>() + to_plate[r][3])).collect();
        let over = if hands.is_empty() { 0.0 } else { HELD_OVER_BED };
        if part.mesh.triangles.is_empty() {
            let layers = sliced(ev, i, paths)?;
            build_checkpoint()?;
            let pieces = layers.pieces();
            build_checkpoint()?;
            if pieces.len() == 1 && hands.is_empty() {
                asm.bodies.push(outline_body(&part.name, &layers, &pieces[0], ev.profile.material.density, 0.0));
                bodies.push((part.object.clone(), IDENTITY, false));
                continue;
            }
            split.push(part.object.clone());
            for at in &hands {
                let k = fab_loose::apart::which(&layers, &pieces, *at).unwrap_or(0);
                held.push((asm.bodies.len() + k, [at[0], at[1], at[2] + over]));
            }
            let made = field_pieces(ev, i, &layers, &pieces, over)?;
            for (body, (object, sdf, (lo, hi))) in made.bodies.into_iter().zip(made.shown) {
                asm.bodies.push(body);
                bodies.push((object.clone(), IDENTITY, false));
                fields_shown.push((object, sdf, lo, hi, part.color));
            }
            continue;
        }
        let solids = makepad_csg_mech::mesh_pieces(&part.mesh.positions, &part.mesh.triangles, fused);
        build_checkpoint()?;
        if solids.is_empty() || (solids.len() == 1 && hands.is_empty()) {
            asm.bodies.push(free_body(ev, i, paths)?);
            bodies.push((part.object.clone(), IDENTITY, false));
            continue;
        }
        split.push(part.object.clone());
        let boxes: Vec<([f64; 3], [f64; 3])> = solids.iter().map(|(p, _)| p.iter().fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), v| (std::array::from_fn(|k| lo[k].min(v[k])), std::array::from_fn(|k| hi[k].max(v[k]))))).collect();
        // each hand on the solid it is in (its field lowest there: below zero inside)
        let depths: Vec<Vec<f64>> = if hands.is_empty() || solids.len() == 1 {
            Vec::new()
        } else {
            makepad_fab_toolpath::par::map(solids.len(), |k| {
                let mut d = vec![0.0; hands.len()];
                fab_loose::solid::Solid::of_mesh(solids[k].0.clone(), solids[k].1.clone()).values(&hands, &mut d);
                d
            })
        };
        for (h, at) in hands.iter().enumerate() {
            let k = (0..solids.len()).min_by(|a, b| depths[*a][h].total_cmp(&depths[*b][h])).unwrap();
            held.push((asm.bodies.len() + k, [at[0], at[1], at[2] + over]));
        }
        let solids: Vec<(Vec<[f64; 3]>, Vec<[u32; 3]>)> = solids.into_iter().map(|(p, t)| (p.into_iter().map(|v| [v[0], v[1], v[2] + over]).collect(), t)).collect();
        // each solid's hulls on its own worker; then the grids of those within another's box, each
        // across the workers a slab at a time
        let built = makepad_fab_toolpath::par::map(solids.len(), |k| -> Result<(BodySpec, fab_loose::mesh::PieceMesh), String> {
            let (positions, triangles) = &solids[k];
            let solid = if solids.len() == 1 { part.name.clone() } else { format!("{} solid {}", part.name, k + 1) };
            let body = makepad_csg_mech::mesh_body(&solid, positions, triangles, ev.profile.material.density, 1.0, &[]);
            if body.hulls.is_empty() {
                return Err(format!("{solid} has no solid volume to simulate"));
            }
            Ok((body, fab_loose::mesh::creased(positions, triangles, 40.0)))
        });
        for (k, b) in built.into_iter().enumerate() {
            let (body, shown) = b?;
            let nested = boxes.iter().enumerate().any(|(j, b)| j != k && (0..3).all(|a| b.0[a] < boxes[k].1[a] && boxes[k].0[a] < b.1[a]));
            let body = if nested { with_grid(ev, body, &fab_loose::solid::Solid::of_mesh(solids[k].0.clone(), solids[k].1.clone()))? } else { body };
            let object = crate::plate_scene::link_object(&part.object, k);
            asm.bodies.push(body);
            solids_shown.push((object.clone(), shown, part.color));
            bodies.push((object, IDENTITY, false));
        }
    }
    for (name, board) in boards {
        build_checkpoint()?;
        let part = &board.part;
        if part.mesh.triangles.is_empty() {
            return Err(format!("{} has no mesh to simulate", part.name));
        }
        let mut body = makepad_csg_mech::mesh_body(&part.name, &part.mesh.positions, &part.mesh.triangles, ev.profile.material.density, 1.0, &[]);
        if body.hulls.is_empty() {
            return Err(format!("{} has no solid volume to simulate", part.name));
        }
        // Use mesh mass until the committed design supplies this board's mass and triggers a rebuild.
        if let Some((_, frame)) = &board.frame {
            if let Some(&BoardMass { volume, mass, com, inertia }) = masses.get(name) {
                let plate_from_board = mul(&m4(&board.model_to_plate), &m4(frame));
                body.mass = moved_mass(MassProps { volume, mass, com, inertia }, &plate_from_board);
            }
        } else {
            log!("fab mech: board:{name} has no frame, using its mesh volume mass");
        }
        asm.bodies.push(body);
        bodies.push((part.object.clone(), IDENTITY, false));
    }
    let low = asm.bodies.iter().flat_map(|b| b.hulls.iter().flatten()).map(|p| p[2]).fold(0.0, f64::min);
    let in_hand: Vec<String> = held.iter().map(|&(body, _)| format!(" · {} in hand", asm.bodies[body].name)).collect();
    asm.anchors = held.into_iter().map(|(body, pivot)| makepad_csg_mech::sim::AnchorSpec { body, pivot, kind: makepad_csg_mech::sim::AnchorKind::Held, friction: Default::default() }).collect();
    let verdict = format!("{} free part{}{} · pick up, pin or throw", bodies.len(), if bodies.len() == 1 { "" } else { "s" }, in_hand.concat());
    let scene = (!split.is_empty()).then(|| {
        let shown: Vec<(String, &fab_loose::mesh::PieceMesh, [f32; 4])> = solids_shown.iter().map(|(o, m, c)| (o.clone(), m, *c)).collect();
        let fields: Vec<(String, [f64; 3], [f64; 3])> = fields_shown.iter().map(|(o, _, lo, hi, _)| (o.clone(), *lo, *hi)).collect();
        crate::plate_scene::build_solids(ev, name, &shown, &fields, &split)
    });
    let fields = fields_shown.into_iter().map(|(object, sdf, _, _, color)| fab::api::MechField { object, sdf, color }).collect();
    Ok(Built { hold: m4(&IDENTITY), asm, gravity: [0.0, 0.0, -GRAVITY], floor: Some(low - 0.05), bodies, verdict, joints_ok: Vec::new(), scene, ball: None, riders: None, frame: None, fields })
}

/// How far over the bed a hand holds a part its script holds by hand (mm).
const HELD_OVER_BED: f64 = 10.0;

/// Carry a centre of mass and its inertia through a rigid transform in constant time.
fn moved_mass(mut mass: MassProps, m: &M4) -> MassProps {
    mass.com = apply(m, mass.com);
    let inertia = mass.inertia;
    // R I R^T
    mass.inertia = std::array::from_fn(|a| std::array::from_fn(|b| {
        (0..3).map(|k| (0..3).map(|l| m[a][k] * inertia[k][l] * m[b][l]).sum::<f64>()).sum()
    }));
    mass
}

/// A body moved rigidly by `m` (its hulls, tubes, centre of mass and inertia).
fn moved(mut b: BodySpec, m: &M4) -> BodySpec {
    for h in &mut b.hulls {
        for p in h.iter_mut() {
            *p = apply(m, *p);
        }
    }
    for (pts, _) in &mut b.tubes {
        for p in pts.iter_mut() {
            *p = apply(m, *p);
        }
    }
    b.mass = moved_mass(b.mass, m);
    b
}

/// The radius a string is drawn at (mm).
pub(crate) const STRING_RADIUS: f64 = 0.3;
/// Each span of a string (from point to point of its path) is drawn as this many straight segments, so a
/// slack one sags.
const STRING_SEGMENTS: usize = 8;

/// The scene objects the strings are drawn as, segment by segment: string `s`'s segment `k`.
fn string_object(s: usize, k: usize) -> String {
    format!("{}mech-string~{s}~{k}", fab::api::CONTEXT_PREFIX)
}

fn string_objects(asm: &Assembly) -> Vec<String> {
    asm.strings.iter().enumerate().flat_map(|(s, st)| (0..(st.through.len() + 1) * STRING_SEGMENTS).map(move |k| string_object(s, k))).collect()
}

/// A string as the view draws it: per point of its path the body that holds it, and its segments' objects
/// (their elements once seen).
struct StringDraw {
    bodies: Vec<usize>,
    segments: Vec<(String, Option<ElementId>)>,
}

/// The line a string is drawn along (printer frame, mm), from its path's points (`points`, printer frame) and
/// its slack: each span split into [`STRING_SEGMENTS`], straight when taut; slack, each span sags down by a
/// parabola as much longer than the span as its share of the slack, and lies on the bed rather than go under it.
pub(crate) fn string_line(points: &[[f64; 3]], slack: f64) -> Vec<[f64; 3]> {
    use makepad_csg_mech::geom::{len, sub};
    let total: f64 = points.windows(2).map(|w| len(sub(w[1], w[0]))).sum();
    let mut line: Vec<[f64; 3]> = points.first().copied().into_iter().collect();
    for w in points.windows(2) {
        let span = len(sub(w[1], w[0]));
        let share = if total > 0.0 { slack * span / total } else { 0.0 };
        let sag = (3.0 * span * share / 8.0).sqrt();
        for i in 1..=STRING_SEGMENTS {
            let t = i as f64 / STRING_SEGMENTS as f64;
            let mut p = madd(w[0], sub(w[1], w[0]), t);
            p[2] = (p[2] - 4.0 * sag * t * (1.0 - t)).max(p[2].min(STRING_RADIUS));
            line.push(p);
        }
    }
    line
}

/// The move that takes a string segment's rod (1 mm along x from the origin, metres) from `a` to `b`
/// (printer frame, mm), its radius kept.
fn segment_move(a: [f64; 3], b: [f64; 3]) -> [[f32; 4]; 3] {
    let d = makepad_csg_mech::geom::sub(b, a);
    let (u, v) = if makepad_csg_mech::geom::len(d) > 1e-9 { makepad_csg_mech::geom::basis(d) } else { ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]) };
    std::array::from_fn(|k| [d[k] as f32, u[k] as f32, v[k] as f32, (a[k] * 0.001) as f32])
}

/// One body of the sim as the view knows it.
struct Body {
    /// Its object in the view's scene (a plate part, or a link `<object>~<k>`), and that element once seen.
    name: String,
    element: Option<ElementId>,
    /// Sim frame (model mm, as held) -> printer frame (metres) and back.
    to_world: M4,
    from_world: M4,
    /// Printer frame (metres) -> the model frame the part was printed in (its geometry at rest).
    rest_from_world: M4,
    /// Its box in its sim frame (mm, as built): which of a part's solids a point is on.
    bounds: ([f64; 3], [f64; 3]),
}

pub(crate) struct Built {
    /// Model frame -> how the piece is shown held (a hanging piece turned so its gravity points down and lifted
    /// off the bed; identity for a piece on the bed).
    pub(crate) hold: M4,
    pub(crate) asm: Assembly,
    pub(crate) gravity: [f64; 3],
    pub(crate) floor: Option<f64>,
    /// Per body: its scene object's name, its frame (mm) to the plate, and whether the body was built in the
    /// shown frame (`hold` already undone: a plate part added to a scene that is shown held).
    pub(crate) bodies: Vec<(String, [[f64; 4]; 3], bool)>,
    pub(crate) verdict: String,
    pub(crate) joints_ok: Vec<bool>,
    /// A scene of its own (links as separate objects), shown while the sim runs.
    pub(crate) scene: Option<Arc<fab::model::Scene>>,
    /// The ball a cloth drapes over (printer mm).
    pub(crate) ball: Option<([f64; 3], f64)>,
    /// A flexible part's pieces, bent with the parts bonded to it (shown in `scene`).
    pub(crate) riders: Option<crate::mech_flex::Riders>,
    /// The frame (mm) to the plate every body is shown in, the sim's own model frame (each body's own when none).
    pub(crate) frame: Option<[[f64; 4]; 3]>,
    /// The pieces of field parts, each drawn by its own field in `scene`.
    pub(crate) fields: Vec<fab::api::MechField>,
}

/// What a frame of the sim showed anew.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Shown {
    /// Nothing: the snapshot shown already.
    #[default]
    Same,
    /// A new snapshot with the parts where they were (its clock, its joints' angles).
    Still,
    /// The parts moved, or a ghost came or went.
    Moved,
}
/// What the action bar says of the sim.
type Bar = (bool, bool, bool, bool, String, fab::api::MechScene);
fn bar_state(m: &MechView) -> Bar {
    (m.running, m.shaking, m.targets_pinned, m.targets_frozen, fab::ui::mech_actions::speed_text(m), m.scene)
}

pub(crate) const IDENTITY: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

/// The jointed piece Mechanics shows: the selected jointed object, else the first one on the plate.
fn piece(ev: &Evaluated, state: &AppState) -> Option<Arc<fab_model::mech::Mech>> {
    let selected: Vec<String> = state.scene_state.selection.set.iter().filter_map(|e| state.plate.part_of(*e)).map(|i| state.plate.parts[i].object.clone()).collect();
    ev.mech.iter().find(|m| m.names.iter().any(|n| selected.contains(n))).or_else(|| ev.mech.first()).cloned()
}

/// The sim of a jointed piece, at once: its bodies and its joints as declared, no joint check (that
/// runs only when asked). A hinge with no limits declared turns freely, and contact between its
/// parts away from the hinge's own region stops it (the collision proxies leave that region out).
/// `refs`: per body, its plate part's object and model -> plate frame. A flexible part bonded to
/// a row of parts is shown as its pieces (`riders`; the caller builds their scene), not as its body.
/// Its failure's text when the bends of a flexing piece cannot be measured.
pub(crate) fn jointed(mech: &fab_model::mech::Mech, refs: &[(String, [[f64; 4]; 3])]) -> Result<Built, String> {
    let asm = mech.assembly(&[], 1.0).map_err(|f| format!("The bends can't be measured: {f}"))?;
    let up = norm(asm.up);
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    // held fixed or by hand, a piece is held where it printed (a car by its chassis, a marionette by its
    // control): as on the bed
    let on_bed = !mech.hangs();
    let (gravity, floor, hold) = if on_bed {
        // on the bed: straight down onto a floor at the piece's lowest point
        let low = asm.bodies.iter().flat_map(|b| b.hulls.iter().flatten()).map(|p| -dot(*p, up)).fold(f64::NEG_INFINITY, f64::max);
        (scale(up, -GRAVITY), Some(-(low + 0.05)), identity)
    } else {
        // hung on its pin: shown turned so the sim's gravity points down, the pin lifted so the piece
        // hangs clear of the bed
        let g = asm.hang_gravity();
        let pivot = asm.anchors[0].pivot;
        let reach = asm.bodies.iter().flat_map(|b| b.hulls.iter().flatten()).map(|p| makepad_csg_mech::geom::len(makepad_csg_mech::geom::sub(*p, pivot))).fold(0.0, f64::max);
        let r = rotation_between(g, scale(up, -1.0));
        let lift = makepad_csg_mech::geom::madd(pivot, up, reach + 5.0);
        let turn: M4 = [[r[0][0], r[0][1], r[0][2], 0.0], [r[1][0], r[1][1], r[1][2], 0.0], [r[2][0], r[2][1], r[2][2], 0.0], [0.0, 0.0, 0.0, 1.0]];
        let to_pivot: M4 = [[1.0, 0.0, 0.0, -pivot[0]], [0.0, 1.0, 0.0, -pivot[1]], [0.0, 0.0, 1.0, -pivot[2]], [0.0, 0.0, 0.0, 1.0]];
        let from_pivot: M4 = [[1.0, 0.0, 0.0, lift[0]], [0.0, 1.0, 0.0, lift[1]], [0.0, 0.0, 1.0, lift[2]], [0.0, 0.0, 0.0, 1.0]];
        (scale(g, GRAVITY), None, mul(&from_pivot, &mul(&turn, &to_pivot)))
    };
    // welds hold parts as one: no joint to the eye
    let n = asm.joints.iter().filter(|j| !j.kind.holds()).count();
    let verdict = format!("{n} joint{} · not checked", if n == 1 { "" } else { "s" });
    let joints_ok = vec![true; asm.joints.len()];
    let riders = crate::mech_flex::riders(mech, refs);
    let spine = |i: usize| riders.as_ref().is_some_and(|r| r.spines.iter().any(|s| refs.get(i).is_some_and(|(o, _)| o == s)));
    let bodies = (0..asm.bodies.len())
        .map(|i| match refs.get(i).cloned() {
            // a flexible part's body has no element: its pieces show it
            Some((_, t)) if spine(i) => (String::new(), t, false),
            Some((n, t)) => (n, t, false),
            None => (String::new(), IDENTITY, false),
        })
        .collect();
    // parts on strings are shown where the sim has them: all in the frame of the first one on the plate (the
    // plate may have set their objects apart)
    let frame = if asm.strings.is_empty() { None } else { refs.iter().find(|(n, _)| !n.is_empty()).map(|(_, t)| *t) };
    Ok(Built { hold, asm, gravity, floor, bodies, verdict, joints_ok, scene: None, ball: None, riders, frame, fields: Vec::new() })
}

#[derive(Default)]
pub struct MechSim {
    runner: Option<Runner>,
    /// What a person asked of the sim (the action bar, a motor's row) while the plate was being
    /// built again or its sim was: it is for the sim of the plate that comes, given to that sim
    /// once it runs, in order, with the parts selected when it was asked. Never given to a sim
    /// about to go, and never lost.
    held: Vec<(MechAction, Vec<String>)>,
    speed: Option<f64>,
    /// The contact setting was read from this machine's settings and put in force.
    contact_loaded: bool,
    /// The sim as built has parts that meet others through a grid (or checks pieces against
    /// one): a contact setting changed builds it again.
    grids: bool,
    /// SceneState owns the selection revision; remember it after our own pose updates.
    seen_selection: u64,
    /// The scene the bodies found their elements in (`AppState::scene_revision`).
    seen_scene: u64,
    /// What the headset's frames showed since the desktop's last frame: the desktop draws it too.
    unshown: Shown,
    /// What the action bar said when a sim frame last drew it.
    bar: Option<Bar>,
    /// The evaluation the sim is of: the program's hash and the print profile that sized its joints and links.
    of: Option<([u8; 32], makepad_csg_mech::Profile)>,
    /// Masses in board coordinates for the committed design, shared with each free build by Arc.
    board_masses: Arc<BTreeMap<String, BoardMass>>,
    /// Unit names of visible boards, collected once when the current free build starts.
    free_board_names: Vec<String>,
    job: Option<Receiver<Result<Built, String>>>,
    bodies: Vec<Body>,
    /// The joint rows: the sim's joint (index), its name, kind, range and motor (welds have none).
    joints: Vec<(usize, String, String, f64, f64, Option<makepad_csg_mech::sim::Motor>)>,
    /// Per sim joint.
    joints_ok: Vec<bool>,
    /// A flexible part's pieces (scene object, element once seen, how it moves), the flexes they
    /// bend on and the body whose frame they are in.
    riders: Vec<(String, Option<ElementId>, crate::mech_flex::Ride)>,
    flexes: Vec<makepad_csg_mech::Flex>,
    rider_frame: usize,
    strings: Vec<StringDraw>,
    frame: NextFrame,
    shaking: bool,
    grabbed: Option<usize>,
    /// The engine resets of this sim already recorded.
    resets_seen: usize,
    /// Events for the headset (hits, limits) since its last frame.
    pub haptics: Vec<SimEvent>,
    /// The view shows this sim's own scene; the app puts its model scene back when this is set.
    own_scene: bool,
    pub restore_scene: bool,
    /// The snapshot frame the view shows (a paused sim publishes no new one: nothing to redraw).
    shown: Option<u64>,
    /// While a board's bench leads the sim: the lead's pose record and the step of it the views
    /// draw ([`MechSim::show_instant`]), the poses last found there (kept while the sim thread
    /// records) and the step they are of as the view shows them.
    instant: Option<(Arc<PoseRecord>, u64)>,
    instant_poses: Option<(u64, Vec<Pose>)>,
    shown_step: Option<u64>,
    /// The bodies a hand carries: a Supports scene's parts.
    carried: Vec<usize>,
    /// A Supports scene's check, and its build's hand-over (with the sim's own job).
    loose: Option<crate::mech_loose::LooseRun>,
    loose_built: Option<Receiver<crate::mech_loose::Loose>>,
    loose_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// A Supports scene, or a sim of field parts, waits for the plate's slice of this input (its plate
    /// and settings ids), since this time: the slice the Slice view shows, never one of its own.
    slice_of: Option<((u64, u64), std::time::Instant)>,
    /// The Print editor's settings a Supports scene's check is of ([`crate::live_slice::print_setup_id`]),
    /// or a free or chain sim's slice of field parts.
    setup_of: Option<u64>,
    /// The sim is of the plate's slice (its field parts' pieces): other print settings start it again.
    sliced: bool,
    /// The plate `AppState`'s trapped supports were found on.
    trapped_on: Option<[u8; 32]>,
    /// The jointed piece the sim is of, and per body its plate part's object and model -> plate
    /// frame (what a joint check is of).
    mech: Option<(Arc<fab_model::mech::Mech>, Vec<(String, [[f64; 4]; 3])>)>,
    /// The sim's joints as declared (a check's measured ranges replace their limits).
    kinds: Vec<JointKind>,
    /// The last joint check, run when asked, kept across sims of the piece.
    check: Option<crate::mech_check::Check>,
    /// The platform said the app is hidden: the sim's parallel work runs at Background ([`SimPool`]).
    /// A sim runs only in the Mechanics workspace, which shows it.
    hidden: Arc<AtomicBool>,
    /// How many sims this view started: the running one's number ([`MechSim::runner`]).
    started: u64,
}

impl MechSim {
    /// Compare maps once per arrival. A changed entry needs a rebuild only for a board in this
    /// free build, checked with two map lookups per placed board.
    pub fn set_board_masses(&mut self, cx: &mut Cx, masses: Arc<BTreeMap<String, BoardMass>>, state: &mut AppState) {
        if self.board_masses == masses {
            return;
        }
        if self.of.is_some() && self.free_board_names.iter().any(|name| self.board_masses.get(name) != masses.get(name)) {
            self.of = None;
            self.job = None;
            if state.ui.in_mode(PlateMode::Mechanics) {
                self.frame = cx.new_next_frame();
            }
        }
        self.board_masses = masses;
    }

    fn speed_pref(&mut self) -> f64 {
        *self.speed.get_or_insert_with(|| fab::view_prefs::load_mech_speed().unwrap_or(1.0))
    }

    /// Keep the sim in step with the workspace and the evaluation; call on every event.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event, state: &mut AppState, plate: Option<&Arc<Evaluated>>, building: bool) {
        match event {
            Event::Background => self.hidden.store(true, Ordering::Relaxed),
            Event::Foreground => self.hidden.store(false, Ordering::Relaxed),
            _ => {}
        }
        if !self.contact_loaded {
            self.contact_loaded = true;
            fab_loose::grid::set_contact(contact_of(fab::view_prefs::load_mech_contact()));
        }
        state.mech.contact = contact_view(fab_loose::grid::contact());
        if self.job.is_some() {
            // an exact grid's build says how far it is
            if let Some(status) = GRID_PROGRESS.lock().unwrap().take() {
                if state.mech.status != status {
                    state.mech.status = status;
                    cx.redraw_all();
                }
            }
        }
        if self.trapped_on.is_some() && (plate.is_none_or(|ev| self.trapped_on != Some(ev.content)) || self.setup_of != crate::live_slice::print_setup_id()) {
            self.supports_stale(state);
        }
        let in_mech = state.ui.in_mode(PlateMode::Mechanics);
        if !in_mech {
            // leaving Mechanics stops a joint check (what it found so far stays)
            if let Some(c) = &mut self.check {
                c.stop();
            }
            self.held.clear();
            if self.runner.is_some() || self.job.is_some() || self.slice_of.is_some() || state.scene_state.moves.is_some() {
                self.runner = None;
                self.job = None;
                self.of = None;
                self.stop_loose();
                state.scene_state.set_moves(None);
                state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, ..Default::default() };
                self.restore_scene |= std::mem::take(&mut self.own_scene);
                cx.redraw_all();
            }
            return;
        }
        self.follow_scene(state);
        let Some(ev) = plate else {
            state.mech.status = "Nothing to simulate yet".into();
            return;
        };
        // Other print settings slice the plate again: the check is of that slice.
        if state.mech.scene == fab::api::MechScene::Supports && self.of.is_some() && self.setup_of != crate::live_slice::print_setup_id() {
            self.supports_stale(state);
        }
        if self.of.as_ref().is_none_or(|(content, profile)| *content != ev.content || *profile != ev.profile) && self.job.is_none() {
            // A flexing piece's sim waits for its bends, measured after the build; a sim shown
            // meanwhile stays, out of date.
            match self.bends_pending(ev, state) {
                Some(status) if state.mech.status != status => {
                    if self.runner.is_none() {
                        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, ..Default::default() };
                    }
                    state.mech.status = status;
                    cx.redraw_all();
                }
                Some(_) => {}
                None => self.start(cx, ev, state),
            }
        }
        // Other print settings slice the plate again: a sim built from its slice starts again (one
        // waiting for its slice waits for the new one).
        if self.sliced && self.slice_of.is_none() && self.setup_of != crate::live_slice::print_setup_id() && self.job.is_none() {
            self.of = None;
        }
        if let Some((key, _)) = &mut self.slice_of {
            // a whole slice is of the plate as it is now: its parts' filaments can change under the
            // same shapes, and the slice waited for with them
            if let Event::Actions(actions) = event {
                if actions.iter().any(|a| matches!(a.downcast_ref::<crate::live_slice::LiveAction>(), Some(crate::live_slice::LiveAction::Toolpaths { .. }))) {
                    *key = crate::live_slice::slice_key(ev);
                }
            }
            if state.mech.scene == fab::api::MechScene::Supports {
                self.build_loose(cx, ev, state);
            } else {
                self.start_sliced(cx, ev, state);
            }
        }
        if let Some(rx) = &self.job {
            let result = match rx.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("Building the physics stopped unexpectedly".into())),
            };
            if let Some(r) = result {
                self.job = None;
                match r {
                    Ok(b) => {
                        self.install(cx, ev, state, b);
                        // the check starts with the sim it goes with
                        if let Some(loose) = self.loose_built.take().and_then(|rx| rx.try_recv().ok()) {
                            let run = crate::mech_loose::LooseRun::start(cx, loose);
                            state.mech.pieces = run.rows();
                            self.loose = Some(run);
                        }
                    }
                    Err(e) => {
                        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: e, ..Default::default() };
                        cx.redraw_all();
                    }
                }
            }
        }
        if let Some(run) = &mut self.loose {
            let (news, scene) = run.poll(cx);
            if news {
                state.mech.pieces = run.rows();
                state.print.support_pieces = Some(state.mech.pieces.clone());
                self.trapped_on = Some(ev.content);
                cx.redraw_all();
            }
            if let Some(scene) = scene {
                // The new colours go in with the poses at once: no frame shows the bodies at rest.
                state.update_scene(scene, false);
                self.follow_scene(state);
                cx.redraw_all();
            }
        }
        // what was held goes to the sim of the plate as it now is, once that sim runs
        let current = self.of.as_ref().is_some_and(|(content, profile)| *content == ev.content && *profile == ev.profile);
        if !building && current && self.job.is_none() && self.runner.is_some() {
            for (a, keys) in std::mem::take(&mut self.held) {
                self.act(cx, &a, &keys, state);
            }
        }
        self.poll_check(cx, state);
        if self.runner.is_some() {
            self.resolve(state);
            if self.seen_selection != state.scene_state.revision {
                let keys = self.selected_body_keys(state);
                let elements: Vec<ElementId> = self.bodies.iter().filter(|b| !b.name.is_empty() && keys.contains(b.name.split('~').next().unwrap_or(""))).filter_map(|b| b.element).collect();
                if elements.iter().any(|e| !state.scene_state.selection.set.contains(e)) {
                    let active = state.scene_state.selection.active.filter(|e| elements.contains(e)).or_else(|| elements.first().copied());
                    state.scene_state.select_set(elements);
                    state.scene_state.selection.active = active;
                }
                self.seen_selection = state.scene_state.revision;
                // A paused snapshot is unchanged, but its target indicators must follow selection.
                self.shown = None;
            }
        }
        // a headset's frame draws the newest snapshot, whatever the desktop's frames do
        if matches!(event, Event::XrUpdate(_)) {
            let shown = self.on_frame(state);
            self.unshown = self.unshown.max(shown);
        }
        if self.frame.is_event(event).is_some() {
            // what shows the sim: the panel and the grab targets with each new snapshot, the view
            // and the headset when the parts moved, the action bar when what it says changed
            match self.on_frame(state).max(std::mem::take(&mut self.unshown)) {
                Shown::Same => {}
                Shown::Still => fab::ui::redraw(cx, &[ids!(tab_mechanics), ids!(mech_grab)]),
                Shown::Moved => fab::ui::redraw(cx, &[ids!(tab_mechanics), ids!(mech_grab), ids!(viewport), ids!(xr)]),
            }
            let bar = Some(bar_state(&state.mech));
            if self.bar != bar {
                self.bar = bar;
                fab::ui::redraw(cx, &[ids!(mechanics_ui.actions)]);
            }
        }
        self.seen_selection = state.scene_state.revision;
        // frames while something moves, while the build runs, until the parts are found in the scene and
        // until the sim answers a command. Going idle, the view takes the sim's news: news since it last
        // looked is one more frame, and whatever the sim publishes after wakes it, so nothing the view
        // believes about the sim can leave it asleep over news (a paused sim publishes none)
        let unresolved = self.unresolved();
        let answered = self.runner.as_mut().is_none_or(|r| r.answered());
        let polling = self.job.is_some() || (self.runner.is_some() && (state.mech.running || self.shown.is_none() || unresolved || !answered));
        if polling || self.runner.as_ref().is_some_and(|r| r.take_news()) {
            self.frame = cx.new_next_frame();
        }
    }

    /// What the check found no longer holds: the shape or the print settings changed.
    pub fn supports_stale(&mut self, state: &mut AppState) {
        state.print.support_pieces = None;
        self.trapped_on = None;
        if state.mech.scene == fab::api::MechScene::Supports {
            self.of = None;
        }
    }

    fn stop_loose(&mut self) {
        self.slice_of = None;
        self.loose = None;
        self.loose_built = None;
        if let Some(stop) = self.loose_stop.take() {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// What Mechanics says while the piece it would show waits for its bends (measured after
    /// the build): none when it needs none or has them (a failure shows as the sim's).
    fn bends_pending(&self, ev: &Evaluated, state: &AppState) -> Option<String> {
        if state.mech.scene == fab::api::MechScene::Supports {
            return None;
        }
        let mech = piece(ev, state).filter(|m| m.flexing() && m.flexes().is_none())?;
        crate::mech_flex::bends_status(&mech, self.runner.is_some())
    }

    /// The view's scene changed (its element ids are its own): the sim finds its parts in it and shows
    /// them where it has them before the view draws it, so no frame shows them where they printed.
    fn follow_scene(&mut self, state: &mut AppState) {
        if self.seen_scene == state.scene_revision {
            return;
        }
        self.seen_scene = state.scene_revision;
        if self.runner.is_none() {
            return;
        }
        self.bodies.iter_mut().for_each(|b| b.element = None);
        self.riders.iter_mut().for_each(|r| r.1 = None);
        self.strings.iter_mut().flat_map(|s| s.segments.iter_mut()).for_each(|g| g.1 = None);
        self.shown = None;
        self.on_frame(state);
    }

    fn start(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState) {
        self.runner = None;
        self.grabbed = None;
        self.resets_seen = 0;
        self.haptics.clear();
        self.stop_loose();
        state.scene_state.set_moves(None);
        state.mech.posed_only.clear();
        self.of = Some((ev.content, ev.profile.clone()));
        self.free_board_names.clear();
        self.sliced = false;
        self.restore_scene |= std::mem::take(&mut self.own_scene);
        self.mech = None;
        // a joint check running stops with the sim it went with; what it found is out of date
        // once the piece or its print settings changed
        if let Some(c) = &mut self.check {
            c.stop();
        }
        if state.mech.scene == fab::api::MechScene::Supports {
            return self.start_loose(cx, ev, state);
        }
        // the closed box poses the case itself (`closed_box`): no physics
        if state.mech.scene == fab::api::MechScene::Closed {
            state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, ..Default::default() };
            return;
        }
        let Some(mech) = piece(ev, state) else {
            // a field part moves as what of it prints apart, read off the plate's slice: the sim
            // waits for it, the parts shown as they are meanwhile
            if has_field_parts(ev) {
                if ev.machine.resin_screen().is_some() {
                    state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: "A field part moves as its filament slice prints it, and this printer prints resin".into(), ..Default::default() };
                    return;
                }
                state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: "Waiting for the plate's slice…".into(), ..Default::default() };
                cx.redraw_all();
                self.sliced = true;
                self.slice_of = Some((crate::live_slice::slice_key(ev), std::time::Instant::now()));
                return self.start_sliced(cx, ev, state);
            }
            if !ev.weaves.is_empty() {
                return self.start_weave(cx, ev, state, None);
            }
            return self.start_free(cx, ev, state, None);
        };
        if let Some(c) = self.check.as_mut().filter(|c| c.found.instance == mech.instance) {
            c.stale |= !c.found.is_of(&mech.instance, ev.content, &ev.profile);
        }
        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: "Building the physics…".into(), ..Default::default() };
        let plate_part: Vec<Option<usize>> = mech.names.iter().map(|n| ev.objects.iter().find(|o| o.name == *n).and_then(|o| o.part)).collect();
        let refs: Vec<(String, [[f64; 4]; 3])> = plate_part.iter().map(|p| p.and_then(|p| Some((ev.plate.parts.get(p)?.object.clone(), *ev.model_to_plate.get(p)?))).unwrap_or((String::new(), IDENTITY))).collect();
        // a hung piece shows first where the sim hangs it, turned on its pin: not where it printed
        if mech.hangs() {
            state.mech.posed_only = refs.iter().map(|(object, _)| object.clone()).filter(|o| !o.is_empty()).collect();
        }
        self.mech = Some((mech.clone(), refs.clone()));
        let (tx, rx) = channel();
        let (ev, name) = (ev.clone(), state.ui.document_name.clone());
        // A closed pool (the app ends) drops the job.
        let _ = cx.task_pool().submit_named(Class::Visible, "fab mech", move || {
                let built = jointed(&mech, &refs).map(|mut built| {
                    // a flexible part is shown as its pieces, and strings as their segments, in a scene of the
                    // sim's own
                    let strings = string_objects(&built.asm);
                    if built.riders.is_some() || !strings.is_empty() {
                        let (pieces, spines) = match &built.riders {
                            Some(r) => (crate::mech_flex::scene_pieces(&ev, &r.pieces), r.spines.clone()),
                            None => (Vec::new(), Vec::new()),
                        };
                        built.scene = Some(crate::plate_scene::build_jointed(&ev, &name, &pieces, &spines, &strings));
                    }
                    built
                });
                let _ = tx.send(built);
                SignalToUI::set_ui_signal();
            });
        self.job = Some(rx);
        self.frame = cx.new_next_frame();
    }

    /// Ordinary plate parts share the printer frame (mm), including their laid-out transforms.
    /// Their joints and anchors are empty, so every part can be lifted from the floor and thrown.
    /// `paths`: the plate's slice, which a plate with field parts waits for.
    fn start_free(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState, paths: Option<Arc<PlatePaths>>) {
        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: "Building free parts…".into(), ..Default::default() };
        let masses = self.board_masses.clone();
        self.free_board_names = free_boards(ev).map(|(name, _)| name.to_owned()).collect();
        let (ev, name) = (ev.clone(), state.ui.document_name.clone());
        let (tx, rx) = channel();
        match cx.task_pool().submit_internal(Class::Visible, move || {
            let t = std::time::Instant::now();
            let built = free_parts(&ev, &name, paths.as_deref(), &masses);
            if paths.is_some() {
                log!("fab mech: free parts from the plate's slice in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
            }
            let _ = tx.send(built);
            SignalToUI::set_ui_signal();
        }) {
            Ok(task) => {
                task.detach();
                self.job = Some(rx);
                self.frame = cx.new_next_frame();
            }
            Err(e) => state.mech.status = format!("Could not build the physics scene: {e:?}"),
        }
    }

    /// The sim of the plate's planned supports as loose pieces, once the plate's slice is whole
    /// (the app sends the plate to it while this scene is shown).
    fn start_loose(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState) {
        let status = if ev.plate.parts.is_empty() { "Nothing on the plate prints" } else { "Waiting for the plate's slice…" };
        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: status.into(), ..Default::default() };
        cx.redraw_all();
        self.setup_of = crate::live_slice::print_setup_id();
        if !ev.plate.parts.is_empty() {
            self.slice_of = Some((crate::live_slice::slice_key(ev), std::time::Instant::now()));
            self.build_loose(cx, ev, state);
        }
    }

    /// A free or chain sim of a plate with field parts starts once the plate's slice is whole; a slice
    /// that failed says why.
    fn start_sliced(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState) {
        let Some((key, asked)) = self.slice_of else { return };
        let Some(sliced) = crate::live_slice::toolpaths_of(key) else {
            if let fab::print::SliceStatus::Failed(e) = &state.print.status {
                let status = format!("The plate does not slice: {e}");
                if state.mech.status != status {
                    state.mech.status = status;
                    cx.redraw_all();
                }
            }
            return;
        };
        self.slice_of = None;
        // the settings it is sliced at: before its first slice the app has none
        self.setup_of = crate::live_slice::print_setup_id();
        log!("fab mech: the plate's slice was whole {:.2} s after the sim asked", asked.elapsed().as_secs_f64());
        if !ev.weaves.is_empty() {
            return self.start_weave(cx, ev, state, Some(sliced.toolpaths));
        }
        self.start_free(cx, ev, state, Some(sliced.toolpaths))
    }

    /// The sim waits for the plate's slice: the app slices the plate while it does.
    pub fn waits_for_slice(&self) -> bool {
        self.slice_of.is_some()
    }

    /// The pieces are built from the plate's slice once it is whole; a slice that failed says why.
    fn build_loose(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState) {
        let Some((key, asked)) = self.slice_of else { return };
        let Some(sliced) = crate::live_slice::toolpaths_of(key) else {
            if let fab::print::SliceStatus::Failed(e) = &state.print.status {
                let status = format!("The plate does not slice: {e}");
                if state.mech.status != status {
                    state.mech.status = status;
                    cx.redraw_all();
                }
            }
            return;
        };
        self.slice_of = None;
        log!("fab loose: the plate's slice was whole {:.2} s after the check asked", asked.elapsed().as_secs_f64());
        state.mech.status = "Finding the support pieces…".into();
        cx.redraw_all();
        let (ev, name, paths) = (ev.clone(), state.ui.document_name.clone(), sliced.toolpaths);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.loose_stop = Some(stop.clone());
        let (tx, rx) = channel();
        let (loose_tx, loose_rx) = channel();
        match cx.task_pool().submit_internal(Class::Visible, move || {
            let stopped = move || stop.load(std::sync::atomic::Ordering::Relaxed);
            let result = crate::mech_loose::build(&ev, &paths, &name, &stopped).map(|(built, loose)| {
                let _ = loose_tx.send(loose);
                built
            });
            if !stopped() {
                let _ = tx.send(result);
                SignalToUI::set_ui_signal();
            }
        }) {
            Ok(task) => {
                task.detach();
                self.job = Some(rx);
                self.loose_built = Some(loose_rx);
                self.frame = cx.new_next_frame();
            }
            Err(e) => state.mech.status = format!("Could not find the support pieces: {e:?}"),
        }
    }

    /// The sim of a part made of links (a chain, chainmail): free links that hold by contact, in the scene
    /// the panel picked (dropped onto a floor, draped over a ball, hung by one end).
    /// `paths`: the plate's slice, which a plate with field parts waits for.
    fn start_weave(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState, paths: Option<Arc<PlatePaths>>) {
        use fab::api::MechScene;
        use makepad_csg_mech::weave::{self, WeaveProfile, WeaveScene};
        let which = match state.mech.scene {
            MechScene::Drop | MechScene::Supports | MechScene::Closed => WeaveScene::Drop,
            MechScene::Drape => WeaveScene::Drape,
            MechScene::Hang => WeaveScene::Hang,
        };
        let piece = state.mech.piece.min(ev.weaves.len() - 1);
        let more = if ev.weaves.len() > 1 { format!(" ({} of {} chain parts)", piece + 1, ev.weaves.len()) } else { String::new() };
        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, status: format!("Linking the chain{more}…"), ..Default::default() };
        let ev = ev.clone();
        let name = state.ui.document_name.clone();
        let (tx, rx) = channel();
        // A closed pool (the app ends) drops the job.
        let _ = cx.task_pool().submit_named(Class::Visible, "fab mech", move || {
                let t = std::time::Instant::now();
                let w = &ev.weaves[piece];
                let wp = WeaveProfile::new(&makepad_csg_mech::Profile::default(), Some(w.links.radius));
                let woven = w.weave(&wp);
                let t_weave = t.elapsed().as_secs_f64() * 1e3;
                let mut setup = weave::scene(&woven, fab_model::weave::DENSITY, which);
                let up = norm(setup.asm.up);
                let identity: M4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
                let lift = |d: f64| -> M4 { [[1.0, 0.0, 0.0, up[0] * d], [0.0, 1.0, 0.0, up[1] * d], [0.0, 0.0, 1.0, up[2] * d], [0.0, 0.0, 0.0, 1.0]] };
                let hold = if setup.asm.anchors.is_empty() {
                    if setup.show_lift > 0.0 { lift(setup.show_lift) } else { identity }
                } else {
                    // hung: shown turned so its gravity points down, the held end lifted clear of the bed
                    let n = setup.asm.anchors.len() as f64;
                    let pivot = setup.asm.anchors.iter().fold([0.0; 3], |a, x| [a[0] + x.pivot[0] / n, a[1] + x.pivot[1] / n, a[2] + x.pivot[2] / n]);
                    let reach = setup.asm.bodies.iter().flat_map(|b| b.tubes.iter().flat_map(|t| t.0.iter())).map(|p| makepad_csg_mech::geom::len(makepad_csg_mech::geom::sub(*p, pivot))).fold(0.0, f64::max);
                    let r = rotation_between(setup.gravity, scale(up, -1.0));
                    let to = makepad_csg_mech::geom::madd(pivot, up, reach + 5.0);
                    let turn: M4 = [[r[0][0], r[0][1], r[0][2], 0.0], [r[1][0], r[1][1], r[1][2], 0.0], [r[2][0], r[2][1], r[2][2], 0.0], [0.0, 0.0, 0.0, 1.0]];
                    let to_pivot: M4 = [[1.0, 0.0, 0.0, -pivot[0]], [0.0, 1.0, 0.0, -pivot[1]], [0.0, 0.0, 1.0, -pivot[2]], [0.0, 0.0, 0.0, 1.0]];
                    let from_pivot: M4 = [[1.0, 0.0, 0.0, to[0]], [0.0, 1.0, 0.0, to[1]], [0.0, 0.0, 1.0, to[2]], [0.0, 0.0, 0.0, 1.0]];
                    mul(&from_pivot, &mul(&turn, &to_pivot))
                };
                let ball = setup.ball.map(|(c, r)| (apply(&mul(&m4(&w.to_plate), &hold), c), r));
                let t = std::time::Instant::now();
                let check = weave::check(&woven, &wp);
                let t_check = t.elapsed().as_secs_f64() * 1e3;
                let (pieces, loose) = woven.pieces();
                // a kind of gap the weave does not have (a chain's links are all over and under) is not shown
                let gap = |g: f64, what: &str| if g.is_finite() { format!("{g:.2} mm {what}") } else { format!("none {what}") };
                let verdict = format!(
                    "{} links, {} interlocks · {} piece{}{} · gaps {}, {}",
                    woven.links.len(),
                    woven.pairs.len(),
                    pieces,
                    if pieces == 1 { "" } else { "s" },
                    if loose > 0 { format!(", {loose} loose") } else { String::new() },
                    gap(check.min_gap_xy.0, "in layer"),
                    gap(check.min_gap_z.0, "across")
                );
                let mut bodies: Vec<(String, [[f64; 4]; 3], bool)> = (0..setup.asm.bodies.len()).map(|k| (crate::plate_scene::link_object(&ev.plate.parts[w.part].object, k), w.to_plate, false)).collect();
                // The plate's other parts join the links' world as free parts (grabbed, thrown and hit by the
                // links), built where the plate shows them: the plate frame taken back through the weave's
                // frame and the scene's hold.
                let to_sim = inv(&mul(&m4(&w.to_plate), &hold));
                let down = norm(setup.gravity);
                let mut extra = Vec::new();
                for i in 0..ev.plate.parts.len() {
                    if ev.weaves.iter().any(|x| x.part == i) {
                        continue;
                    }
                    match free_body(&ev, i, paths.as_deref()) {
                        Ok(b) => extra.push((i, moved(b, &to_sim))),
                        Err(e) => log!("fab mech: {e}"),
                    }
                }
                // they stand on the bed: a scene without a floor gets one there, a scene whose floor is above
                // a part lifts that part onto it
                let height = |b: &BodySpec| b.hulls.iter().flatten().map(|p| -dot(*p, down)).fold(f64::INFINITY, f64::min);
                let floor = setup.floor.or_else(|| extra.iter().map(|(_, b)| height(b)).reduce(f64::min).map(|h| h - 0.05));
                for (i, mut b) in extra {
                    if let Some(f) = floor {
                        let lift = f + 0.05 - height(&b);
                        if lift > 0.0 {
                            let up: M4 = [[1.0, 0.0, 0.0, -down[0] * lift], [0.0, 1.0, 0.0, -down[1] * lift], [0.0, 0.0, 1.0, -down[2] * lift], [0.0, 0.0, 0.0, 1.0]];
                            b = moved(b, &up);
                        }
                    }
                    setup.asm.bodies.push(b);
                    bodies.push((ev.plate.parts[i].object.clone(), w.to_plate, true));
                }
                setup.floor = floor;
                let t = std::time::Instant::now();
                let scene = crate::plate_scene::build_mechanics(&ev, &name, ball);
                log!("fab mech: {} links: weave {t_weave:.0} ms, check {t_check:.0} ms, scene {:.0} ms ({} elements)", woven.links.len(), t.elapsed().as_secs_f64() * 1e3, scene.elements.len());
                let _ = tx.send(Ok(Built { hold, asm: setup.asm, gravity: setup.gravity, floor: setup.floor, bodies, verdict, joints_ok: Vec::new(), scene: Some(scene), ball, riders: None, frame: None, fields: Vec::new() }));
                SignalToUI::set_ui_signal();
            });
        self.job = Some(rx);
        self.frame = cx.new_next_frame();
    }

    fn install(&mut self, cx: &mut Cx, ev: &Arc<Evaluated>, state: &mut AppState, b: Built) {
        let _ = ev;
        if let Some(scene) = b.scene.clone() {
            cx.action(fab::ShellAction::SceneUpdated { scene, frame: false });
            self.own_scene = true;
        }
        self.bodies = b
            .bodies
            .iter()
            .enumerate()
            .map(|(i, (name, to_plate, shown))| {
                let s = [[0.001, 0.0, 0.0, 0.0], [0.0, 0.001, 0.0, 0.0], [0.0, 0.0, 0.001, 0.0], [0.0, 0.0, 0.0, 1.0]];
                let printed = mul(&s, &m4(to_plate));
                let to_world = mul(&mul(&s, &m4(b.frame.as_ref().unwrap_or(to_plate))), &b.hold);
                let from_world = inv(&to_world);
                // a body built in the shown frame rests where the plate has it
                let rest_from_world = if *shown { from_world } else { inv(&printed) };
                let hulls = b.asm.bodies.get(i).into_iter().flat_map(|body| body.hulls.iter().flatten());
                let bounds = hulls.fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), p| (std::array::from_fn(|k| lo[k].min(p[k])), std::array::from_fn(|k| hi[k].max(p[k]))));
                Body { name: name.clone(), element: None, from_world, rest_from_world, to_world, bounds }
            })
            .collect();
        (self.riders, self.flexes, self.rider_frame) = match b.riders {
            Some(r) => (r.pieces.into_iter().map(|p| (p.name, None, p.ride)).collect(), r.flexes, r.frame),
            None => (Vec::new(), Vec::new(), 0),
        };
        self.strings = b
            .asm
            .strings
            .iter()
            .enumerate()
            .map(|(s, st)| StringDraw { bodies: st.points().map(|(body, _)| body).collect(), segments: (0..(st.through.len() + 1) * STRING_SEGMENTS).map(|k| (string_object(s, k), None)).collect() })
            .collect();
        self.resolve(state);
        self.joints = b
            .asm
            .joints
            .iter()
            .enumerate()
            .filter_map(|(k, j)| {
                let (kind, lower, upper) = match j.kind {
                    JointKind::Revolute { lower, upper } => ("hinge", lower, upper),
                    JointKind::Spherical { cone, .. } => ("ball", 0.0, cone),
                    JointKind::Prismatic { lower, upper } => ("slider", lower, upper),
                    JointKind::Flex { .. } => ("flex", -std::f64::consts::PI, std::f64::consts::PI),
                    JointKind::Weld => return None,
                };
                Some((k, j.name.clone(), kind.to_string(), lower, upper, j.motor))
            })
            .collect();
        self.joints_ok = b.joints_ok;
        self.kinds = b.asm.joints.iter().map(|j| j.kind.clone()).collect();
        self.carried = if state.mech.scene == fab::api::MechScene::Supports { b.asm.anchors.iter().map(|a| a.body).collect() } else { Vec::new() };
        self.shaking = false;
        self.grabbed = None;
        let speed = self.speed_pref();
        self.grids = b.asm.bodies.iter().any(|body| body.field.is_some());
        // the sim thread spreads the engine's parallel work over the app's workpool, helping with it
        let pool: Arc<dyn makepad_dynamics::thread::TaskPool> = Arc::new(SimPool::new(cx.task_pool(), self.hidden.clone()));
        self.runner = Some(Runner::start_with(b.asm, b.gravity, Options { floor: b.floor, pool: Some(pool), ..Options::default() }, SignalToUI::set_ui_signal));
        self.started += 1;
        self.runner.as_ref().unwrap().send(Command::Speed(speed));
        self.runner.as_ref().unwrap().send(Command::Play);
        self.shown = None;
        state.mech = MechView { tool: state.mech.tool, scene: state.mech.scene, piece: state.mech.piece, ready: true, speed, verdict: b.verdict, ball: b.ball, posed_only: std::mem::take(&mut state.mech.posed_only), fields: b.fields, ..Default::default() };
        // a check of this piece as it is still holds: the sim takes its measured ranges
        self.take_measured(state);
        self.on_frame(state);
        state.mech.speed = speed;
        state.mech.running = true;
        self.frame = cx.new_next_frame();
        cx.redraw_all();
    }

    /// Finds each body's element by its object's name (the scene may arrive after the sim starts).
    fn resolve(&mut self, state: &AppState) {
        if !self.unresolved() {
            return;
        }
        let by_name: HashMap<&str, ElementId> = state.scene.elements.iter().map(|e| (e.name.as_str(), e.id)).collect();
        // A sim with a scene of its own (links, a flexible part's pieces) starts before that scene arrives, and
        // a plate part's name is in the old scene too, under an element id the new one gives another part: such
        // a sim resolves all its bodies at once, in the scene that has them all.
        if self.own_scene && (self.bodies.iter().any(|b| !b.name.is_empty() && !by_name.contains_key(b.name.as_str())) || self.riders.iter().any(|r| !by_name.contains_key(r.0.as_str())) || self.strings.iter().any(|s| s.segments.iter().any(|g| !by_name.contains_key(g.0.as_str())))) {
            return;
        }
        for b in &mut self.bodies {
            if b.element.is_none() {
                b.element = by_name.get(b.name.as_str()).copied();
            }
        }
        for r in &mut self.riders {
            if r.1.is_none() {
                r.1 = by_name.get(r.0.as_str()).copied();
            }
        }
        for g in self.strings.iter_mut().flat_map(|s| s.segments.iter_mut()) {
            if g.1.is_none() {
                g.1 = by_name.get(g.0.as_str()).copied();
            }
        }
    }

    /// A body, a flexible part's piece or a string segment whose element the view has not shown yet.
    fn unresolved(&self) -> bool {
        self.bodies.iter().any(|b| b.element.is_none() && !b.name.is_empty()) || self.riders.iter().any(|r| r.1.is_none()) || self.strings.iter().any(|s| s.segments.iter().any(|g| g.1.is_none()))
    }

    fn selected_body_keys(&self, state: &AppState) -> HashSet<&str> {
        self.bodies.iter().filter(|b| b.element.is_some_and(|e| state.scene_state.selection.set.contains(&e))).map(|b| b.name.split('~').next().unwrap_or("")).collect()
    }

    fn selected_part_keys<'a>(&'a self, state: &'a AppState) -> HashSet<&'a str> {
        let mut keys = self.selected_body_keys(state);
        for &e in &state.scene_state.selection.set {
            if let Some(i) = state.plate.part_of(e) {
                keys.insert(state.plate.parts[i].object.as_str());
            }
        }
        keys
    }

    /// The running sim's runner and its number, new with each sim this view starts (None: no sim
    /// runs; it runs only while the Mechanics workspace shows it).
    pub(crate) fn runner(&self) -> Option<(&Runner, u64)> {
        Some((self.runner.as_ref()?, self.started))
    }

    /// A motion probe on printed plate part `part` of `ev` (the evaluation the running sim is of),
    /// at `at` with `axes` (plate mm, plate directions; an orthonormal frame), in the sim's frame of
    /// the part's body: of the solids a part prints apart, the one whose box is nearest the point.
    /// None without a sim or a body of the part. O(bodies).
    pub(crate) fn probe_on(&self, ev: &Evaluated, part: usize, at: [f64; 3], axes: [[f64; 3]; 3]) -> Option<Probe> {
        self.runner.as_ref()?;
        // a bare board is placed but not printed (an excluded part): Mechanics carries it as a body too
        let object = &(part < ev.plate.parts.len() + ev.plate.excluded.len()).then(|| ev.placed_part(part))?.object;
        let piece = format!("{object}~");
        let world = at.map(|v| v * 0.001);
        let gap = |b: &Body| {
            let p = apply(&b.from_world, world);
            (0..3).map(|k| (b.bounds.0[k] - p[k]).max(p[k] - b.bounds.1[k]).max(0.0).powi(2)).sum::<f64>()
        };
        let (body, b) = self.bodies.iter().enumerate()
            .filter(|(_, b)| b.name == *object || b.name.starts_with(&piece))
            .min_by(|(_, a), (_, b)| gap(a).total_cmp(&gap(b)))?;
        let f = &b.from_world;
        // the frame is rigid with a scale (metres to mm): the axes keep only its turn
        let turn = |v: [f64; 3]| norm(std::array::from_fn(|r| f[r][0] * v[0] + f[r][1] * v[1] + f[r][2] * v[2]));
        Some(Probe { body, at: apply(f, world), axes: axes.map(turn) })
    }

    fn target_bodies(&self, state: &AppState) -> Vec<usize> {
        self.bodies_of(&self.selected_part_keys(state))
    }

    /// The bodies of these parts (every body when none is named).
    fn bodies_of(&self, keys: &HashSet<&str>) -> Vec<usize> {
        self.bodies.iter().enumerate().filter_map(|(i, b)| {
            let targeted = keys.is_empty() || (!b.name.is_empty() && (keys.contains(b.name.as_str()) || b.name.match_indices('~').any(|(end, _)| keys.contains(&b.name[..end]))));
            targeted.then_some(i)
        }).collect()
    }

    /// While a board's bench leads the sim ([`crate::motion_link`]): the lead's pose record and the
    /// step of it the views draw, the one at the instant the bench's views show, so the board's
    /// screen and the body it is on show one instant (about the lead behind the sim's newest
    /// step); None draws the newest again. A change shows on this frame's draw of the sim. O(1);
    /// while set, a frame copies one step's poses and the snapshot it draws them into (O(parts)).
    pub(crate) fn show_instant(&mut self, cx: &mut Cx, instant: Option<(Arc<PoseRecord>, u64)>) {
        let same = |a: &(Arc<PoseRecord>, u64), b: &(Arc<PoseRecord>, u64)| Arc::ptr_eq(&a.0, &b.0) && a.1 == b.1;
        if self.instant.as_ref().zip(instant.as_ref()).is_some_and(|(a, b)| same(a, b)) || (self.instant.is_none() && instant.is_none()) {
            return;
        }
        if self.instant.as_ref().zip(instant.as_ref()).is_none_or(|(a, b)| !Arc::ptr_eq(&a.0, &b.0)) {
            self.instant_poses = None;
        }
        self.instant = instant;
        self.frame = cx.new_next_frame();
    }

    /// The step of the lead's record the view draws, found again when the step asked moves (it
    /// keeps the poses it found while the sim thread holds the record).
    fn instant_step(&mut self) -> Option<u64> {
        let (record, step) = self.instant.as_ref()?;
        if self.instant_poses.as_ref().is_none_or(|(found, _)| found != step) {
            if let Some(found) = record.at(*step) {
                self.instant_poses = Some(found);
            }
        }
        self.instant_poses.as_ref().map(|(found, _)| *found)
    }

    fn on_frame(&mut self, state: &mut AppState) -> Shown {
        let unresolved = self.unresolved();
        self.resolve(state);
        let step = self.instant_step();
        let Some(r) = self.runner.as_mut() else { return Shown::Same };
        let answered = r.answered();
        let snap: Arc<Snapshot> = r.latest();
        if self.shown == Some(snap.frame) && self.shown_step == step && !unresolved {
            return Shown::Same;
        }
        let mut moved = false;
        self.shown = Some(snap.frame);
        self.shown_step = step;
        // a bench leading the sim: the parts where they were at the instant its views show
        let snap = match &self.instant_poses {
            Some((_, poses)) if step.is_some() && poses.len() == snap.poses.len() => Arc::new(Snapshot { poses: poses.clone(), ..(*snap).clone() }),
            _ => snap,
        };
        if let Some(e) = &snap.error {
            state.mech.status = format!("The physics could not be built: {e}");
        }
        // A piece that teleports home and looks fine is the worst way to fail: every reset goes on record.
        if snap.resets.len() < self.resets_seen {
            self.resets_seen = 0;
        }
        for reset in &snap.resets[self.resets_seen..] {
            let part = reset.part.and_then(|i| self.bodies.get(i)).map_or("no part", |b| b.name.as_str());
            let text = format!("{}: the physics blew up at {:.3} s on {part} and put the parts back at the print pose", state.ui.document_name, reset.time);
            log!("fab mechanics: {text}");
            makepad_error_log::flight::mark_ab("fab.mech.reset", &text, reset.part.map_or(u64::MAX, |i| i as u64), (reset.time * 1000.0) as u64);
            state.mech.status = "The physics blew up and went back to the print pose".into();
        }
        self.resets_seen = snap.resets.len();
        self.haptics.extend(r.take_events());
        if self.haptics.len() > 64 {
            let n = self.haptics.len() - 64;
            self.haptics.drain(0..n);
        }
        let mut moves: HashMap<ElementId, [[f32; 4]; 3]> = HashMap::new();
        for (i, body) in self.bodies.iter().enumerate() {
            let (Some(e), true) = (body.element, i < snap.poses.len()) else { continue };
            let a = m4(&snap.affine(i));
            let w = mul(&mul(&body.to_world, &a), &body.rest_from_world);
            moves.insert(e, std::array::from_fn(|k| std::array::from_fn(|c| w[k][c] as f32)));
        }
        // a flexible part's pieces: with the part that holds them, or bent between two
        if let Some(frame) = self.bodies.get(self.rider_frame) {
            let pose = |i: usize| (i < snap.poses.len()).then(|| m4(&snap.affine(i)));
            for (_, e, ride) in &self.riders {
                let m = match *ride {
                    crate::mech_flex::Ride::With(i) => pose(i),
                    crate::mech_flex::Ride::Bend { flex, t } => self.flexes.get(flex).and_then(|f| Some(crate::mech_flex::bend(&pose(f.a)?, &pose(f.b)?, f, t))),
                };
                let (Some(e), Some(m)) = (e, m) else { continue };
                let w = mul(&mul(&frame.to_world, &m), &frame.rest_from_world);
                moves.insert(*e, std::array::from_fn(|k| std::array::from_fn(|c| w[k][c] as f32)));
            }
        }
        // each string along its path as this snapshot has it, each point where the part that holds it is shown
        for (draw, path) in self.strings.iter().zip(&snap.strings) {
            let shown: Option<Vec<[f64; 3]>> = path.points.iter().zip(&draw.bodies).map(|(p, &b)| Some(scale(apply(&self.bodies.get(b)?.to_world, *p), 1000.0))).collect();
            let Some(points) = shown.filter(|p| p.len() == path.points.len()) else { continue };
            for (w, (_, e)) in string_line(&points, path.slack).windows(2).zip(&draw.segments) {
                if let Some(e) = e {
                    moves.insert(*e, segment_move(w[0], w[1]));
                }
            }
        }
        // the joint check's marks ride on the parts they are of, its ghosts turn with them
        let check = match (&self.check, &self.mech) {
            (Some(c), Some((mech, _))) if c.found.instance == mech.instance => {
                let bodies = &self.bodies;
                let place = |i: usize| (i < snap.poses.len()).then(|| Some(mul(&bodies.get(i)?.to_world, &m4(&snap.affine(i))))).flatten();
                let rest = |i: usize| bodies.get(i).map(|b| b.rest_from_world);
                let (view, ghosts) = c.view(&state.scene, &place, &rest);
                for (e, w) in ghosts {
                    if let Some(w) = w {
                        moves.insert(e, std::array::from_fn(|k| std::array::from_fn(|c| w[k][c] as f32)));
                    }
                    if state.scene_state.hidden.contains(&e) != w.is_none() {
                        state.scene_state.set_hidden(e, w.is_none());
                        moved = true;
                    }
                }
                view
            }
            (_, Some((mech, _))) => MechCheckView { joints: mech.plan.joints.len(), ..Default::default() },
            _ => MechCheckView::default(),
        };
        state.mech.check = check;
        // a sim at rest goes on publishing the poses it shows: the view keeps what it drew
        if state.scene_state.moves.as_deref() != Some(&moves) {
            state.scene_state.set_moves(Some(Arc::new(moves)));
            moved = true;
        }
        let targets = self.target_bodies(state);
        let pinned: HashSet<usize> = snap.pinned.iter().copied().collect();
        let frozen: HashSet<usize> = snap.frozen.iter().copied().collect();
        let m = &mut state.mech;
        // until the sim has taken up what it was told, what it last said about that is out of date
        if answered {
            m.running = snap.running;
        }
        m.speed = snap.speed;
        m.time = snap.time;
        m.shaking = self.shaking;
        m.grabbed = snap.grabbed;
        m.grabbed_element = snap.grabbed.and_then(|i| self.bodies.get(i)?.element);
        m.pinned = snap.pinned.iter().filter_map(|&i| self.bodies.get(i)?.element).collect();
        m.carried = self.carried.iter().filter_map(|&i| self.bodies.get(i)?.element).collect();
        // a motor just set shows where it was set until the sim has taken it up
        let before = std::mem::take(&mut m.joints);
        m.joints = self
            .joints
            .iter()
            .enumerate()
            .map(|(i, (k, name, kind, lower, upper, motor))| {
                let set = before.get(i).filter(|j| !answered && j.name == *name).and_then(|j| j.motor).map(|m| m.target);
                let target = set.unwrap_or_else(|| snap.motors.iter().find(|(j, _)| j == k).map_or(0.0, |&(_, t)| t));
                let effort = snap.motors.iter().position(|(j, _)| j == k).and_then(|p| snap.efforts.get(p)).copied().unwrap_or(0.0);
                let motor = motor.map(|m| MechMotorView { speed: m.kind == MotorKind::Speed, target, effort, limit: m.limit / makepad_csg_mech::sim::NEWTON });
                let load = snap.loads.get(*k).copied().flatten();
                MechJointView { name: name.clone(), kind: kind.clone(), angle: snap.angles.get(*k).copied().unwrap_or(0.0), lower: *lower, upper: *upper, ok: self.joints_ok.get(*k).copied().unwrap_or(true), load, motor }
            })
            .collect();
        m.strings = snap.strings.iter().map(|s| s.tension).collect();
        m.targets = targets.len();
        m.targets_pinned = targets.iter().any(|i| pinned.contains(i));
        m.targets_frozen = !targets.is_empty() && targets.iter().all(|i| frozen.contains(i));
        if moved { Shown::Moved } else { Shown::Still }
    }

    /// The joint check of the piece the sim is of, as it is (not out of date).
    fn current_check(&self) -> Option<&crate::mech_check::Check> {
        let (mech, _) = self.mech.as_ref()?;
        self.check.as_ref().filter(|c| c.found.instance == mech.instance && !c.stale)
    }

    /// A finished check of the piece as it is: the sim's joints take its measured ranges, and the
    /// panel's joints say which it found free.
    fn take_measured(&mut self, state: &mut AppState) {
        let Some(c) = self.current_check().filter(|c| !c.found.running) else { return };
        let limits = c.found.limits(&self.kinds);
        let ok: Vec<(usize, bool)> = c.found.checks().map(|j| (j.joint, j.ok())).collect();
        let (n, measured) = (self.kinds.len(), limits.len());
        // a check cancelled before any joint was done measured nothing: the joints stay unchecked
        if measured == 0 {
            self.shown = None;
            return;
        }
        for (j, kind) in &limits {
            if let Some(row) = self.joints.iter_mut().find(|r| r.0 == *j) {
                (row.3, row.4) = match kind {
                    JointKind::Revolute { lower, upper } => (*lower, *upper),
                    JointKind::Spherical { cone, .. } => (0.0, *cone),
                    JointKind::Prismatic { lower, upper } => (*lower, *upper),
                    // a check measures the print-in-place joints only
                    JointKind::Flex { .. } | JointKind::Weld => continue,
                };
            }
        }
        for (j, ok) in ok {
            if let Some(o) = self.joints_ok.get_mut(j) {
                *o = ok;
            }
        }
        if let Some(r) = &self.runner {
            r.send(Command::Limits(limits));
        }
        let s = if n == 1 { "" } else { "s" };
        state.mech.verdict = if measured == n { format!("{n} joint{s} · the ranges the check measured") } else { format!("{n} joint{s} · {measured} with the range the check measured") };
        self.shown = None;
    }

    /// Take what a joint check found since the last event: the panel and the view show it, the
    /// ghosts' scene goes in once built, and a check that ended gives the sim its ranges.
    fn poll_check(&mut self, cx: &mut Cx, state: &mut AppState) {
        let shown = self.current_check().is_some() && self.runner.is_some();
        let Some(c) = &mut self.check else { return };
        let polled = c.poll();
        let mut changed = polled.news;
        if let Some(scene) = polled.scene.filter(|_| shown) {
            // the ghosts' scene has the sim's parts too: the bodies find their elements in it
            state.update_scene(scene, false);
            self.own_scene = true;
            self.follow_scene(state);
            changed = true;
        } else if shown && !c.building() && !c.ghosts_in(&state.scene) {
            // another scene took the ghosts' place (the model's came back): build it again
            c.build_ghosts(cx);
        }
        if polled.ended {
            self.take_measured(state);
        }
        if changed {
            self.shown = None;
            self.on_frame(state);
            cx.redraw_all();
        }
    }

    /// The body an element is, and a printer-frame point (metres) in its model frame (mm).
    fn to_model(&self, element: Option<ElementId>, point: [f64; 3]) -> Option<(usize, [f64; 3])> {
        let body = match element {
            Some(e) => self.bodies.iter().position(|b| b.element == Some(e))?,
            None => self.grabbed?,
        };
        Some((body, apply(&self.bodies[body].from_world, point)))
    }

    pub fn action(&mut self, cx: &mut Cx, a: &MechAction, state: &mut AppState, plate: Option<&Arc<Evaluated>>, building: bool) {
        if let MechAction::Tool(tool) = a {
            state.mech.tool = *tool;
            cx.redraw_all();
            return;
        }
        if let MechAction::Scene(scene) = a {
            // a new scene is a new setup: build the sim again
            self.held.clear();
            state.mech.scene = *scene;
            self.of = None;
            self.job = None;
            self.frame = cx.new_next_frame();
            cx.redraw_all();
            return;
        }
        if let MechAction::Piece(piece) = a {
            // another chain part: build the sim of it
            self.held.clear();
            state.mech.piece = *piece;
            self.of = None;
            self.job = None;
            self.frame = cx.new_next_frame();
            cx.redraw_all();
            return;
        }
        if let MechAction::Contact(c) = a {
            if contact_of(*c) != fab_loose::grid::contact() {
                fab_loose::grid::set_contact(contact_of(*c));
                fab::view_prefs::save_mech_contact(*c);
                state.mech.contact = *c;
                // the parts that meet others through a grid are built again with it
                if self.grids || state.mech.scene == fab::api::MechScene::Supports {
                    self.held.clear();
                    self.of = None;
                    self.job = None;
                }
                self.frame = cx.new_next_frame();
                cx.redraw_all();
            }
            return;
        }
        if let MechAction::Speed(s) = a {
            self.speed = Some(*s);
            fab::view_prefs::save_mech_speed(*s);
            state.mech.speed = *s;
            if let Some(r) = &self.runner {
                r.send(Command::Speed(*s));
            }
            self.frame = cx.new_next_frame();
            cx.redraw_all();
            return;
        }
        if let MechAction::CheckJoints = a {
            // one check at a time: the busy line offers Cancel while one runs
            if self.check.as_ref().is_some_and(|c| c.job().is_some()) {
                return;
            }
            let (Some((mech, refs)), Some(ev)) = (&self.mech, plate) else { return };
            match crate::mech_check::Check::start(cx, mech.clone(), ev.clone(), state.ui.document_name.clone(), refs.clone()) {
                Ok(c) => self.check = Some(c),
                Err(e) => state.mech.status = e,
            }
            self.shown = None;
            self.on_frame(state);
            cx.redraw_all();
            return;
        }
        let mut keys: Vec<String> = self.selected_part_keys(state).into_iter().map(str::to_string).collect();
        keys.sort();
        if matches!(a, MechAction::Reset) {
            self.held.clear();
        }
        // asked while the plate is built again, or its sim is (or is not yet started on the plate as
        // it now is): for the sim of the plate that comes
        let current = plate.is_some_and(|ev| self.of.as_ref().is_some_and(|(content, profile)| *content == ev.content && *profile == ev.profile));
        if (building || self.runner.is_none() || !current) && matches!(a, MechAction::Do(_) | MechAction::Play | MechAction::Take | MechAction::Pause | MechAction::Step | MechAction::Shake(_) | MechAction::Motor { .. }) {
            log!("fab mech: {a:?} held for the sim to come");
            self.held.push((a.clone(), keys));
            return;
        }
        self.act(cx, a, &keys, state);
    }

    /// What the sim does when asked: `keys` are the parts selected when it was.
    fn act(&mut self, cx: &mut Cx, a: &MechAction, keys: &[String], state: &mut AppState) {
        let keys: HashSet<&str> = keys.iter().map(String::as_str).collect();
        let Some(r) = &self.runner else { return };
        match a {
            MechAction::Play => r.send(Command::Play),
            MechAction::Take => {
                r.send(Command::Take);
                r.send(Command::Play);
            }
            MechAction::Pause => r.send(Command::Pause),
            MechAction::Step => r.send(Command::Step),
            MechAction::Do(op) => {
                let bodies = self.bodies_of(&keys);
                log!("fab mech: {op:?} on {} of {} bodies, {} parts selected", bodies.len(), self.bodies.len(), keys.len());
                match op {
                    MechOp::Drop => r.send(Command::Bodies { bodies, action: BodyAction::Drop { lift: 60.0 } }),
                    MechOp::Hang => r.send(Command::Bodies { bodies, action: BodyAction::Hang }),
                    MechOp::Pin => r.send(Command::Bodies { bodies, action: BodyAction::Pin }),
                    MechOp::Unpin => r.send(Command::Bodies { bodies, action: BodyAction::Unpin }),
                    MechOp::Freeze(on) => r.send(Command::Bodies { bodies, action: BodyAction::Freeze(*on) }),
                    MechOp::Throw => {
                        let forward = state.view.camera(state.view.active_plate()).target - state.view.camera(state.view.active_plate()).eye;
                        let horizontal = [forward.x as f64, forward.y as f64, 0.0];
                        let forward = if makepad_csg_mech::geom::len(horizontal) > 1e-9 { norm(horizontal) } else { [0.0, 1.0, 0.0] };
                        let dv = [forward[0] * 1.6, forward[1] * 1.6, 1.6];
                        for body in bodies {
                            // As Poke: from_world converts printer m/s to model mm/s.
                            let f = &self.bodies[body].from_world;
                            let dv = std::array::from_fn(|k| f[k][0] * dv[0] + f[k][1] * dv[1] + f[k][2] * dv[2]);
                            r.send(Command::Bodies { bodies: vec![body], action: BodyAction::Throw(dv) });
                        }
                    }
                    MechOp::Home => {
                        if keys.is_empty() {
                            r.send(Command::Reset);
                            self.shaking = false;
                            self.grabbed = None;
                            state.mech.grabbed = None;
                            state.mech.grabbed_element = None;
                            state.mech.pinned.clear();
                            state.mech.running = true;
                        } else {
                            r.send(Command::Bodies { bodies, action: BodyAction::Home });
                        }
                    }
                }
                if !matches!(op, MechOp::Freeze(_) | MechOp::Pin | MechOp::Unpin) {
                    r.send(Command::Play);
                }
            }
            MechAction::Reset | MechAction::Scene(_) | MechAction::Piece(_) => {
                r.send(Command::Reset);
                self.shaking = false;
                self.grabbed = None;
                state.mech.grabbed = None;
                state.mech.grabbed_element = None;
                state.mech.pinned.clear();
                state.mech.running = true;
            }
            MechAction::Speed(_) => unreachable!("speed selection handled before the runner"),
            MechAction::Contact(_) => unreachable!("the contact setting is handled before the runner"),
            MechAction::Shake(on) => {
                self.shaking = *on;
                r.send(Command::Shake(*on));
                if *on {
                    r.send(Command::Play);
                }
            }
            MechAction::Motor { joint, target } => {
                // a motor set moves the piece, as a grab does: the sim runs
                if let Some(&(k, ..)) = self.joints.get(*joint) {
                    r.send(Command::Motor { joint: k, target: *target });
                    r.send(Command::Play);
                    if let Some(m) = state.mech.joints.get_mut(*joint).and_then(|j| j.motor.as_mut()) {
                        m.target = *target;
                    }
                }
            }
            MechAction::Grab { element, point, hand } => {
                match self.to_model(Some(*element), *point) {
                    Some((body, p)) => {
                        log!("fab mech: grab {} at {p:?}", self.bodies[body].name);
                        self.grabbed = Some(body);
                        let hz = if *hand { makepad_csg_mech::runner::HAND_HZ } else { makepad_csg_mech::runner::POINTER_HZ };
                        r.send(Command::Grab { body, point: p, hz });
                        r.send(Command::Play);
                    }
                    None => log!("fab mech: {element:?} is no body: {:?}", self.bodies.iter().map(|b| (&b.name, b.element)).collect::<Vec<_>>()),
                }
            }
            MechAction::Drag { point } => {
                if let Some((_, p)) = self.to_model(None, *point) {
                    r.send(Command::Drag { point: p });
                }
            }
            MechAction::Pin { element, point } => {
                if let Some((body, point)) = self.to_model(Some(*element), *point) {
                    r.send(Command::Pin { body, point });
                    r.send(Command::Play);
                }
            }
            MechAction::Dangle { element, point } => {
                if let Some((body, point)) = self.to_model(Some(*element), *point) {
                    self.grabbed = Some(body);
                    r.send(Command::Dangle { body, point });
                    r.send(Command::Play);
                }
            }
            MechAction::Poke { element, point, dv } => {
                if let Some((body, point)) = self.to_model(Some(*element), *point) {
                    let f = &self.bodies[body].from_world;
                    let dv = std::array::from_fn(|k| f[k][0] * dv[0] + f[k][1] * dv[1] + f[k][2] * dv[2]);
                    r.send(Command::Poke { body, point, dv });
                    r.send(Command::Play);
                }
            }
            MechAction::Carry { pose } => {
                if let Some(body) = self.carried.first().and_then(|&i| self.bodies.get(i)) {
                    // printer metres -> the bodies' model frame, mm
                    let m = mul(&mul(&body.from_world, &m4(pose)), &body.to_world);
                    let (axis, angle) = axis_angle(&m);
                    r.send(Command::Carry { at: [m[0][3], m[1][3], m[2][3]], axis, angle });
                }
            }
            MechAction::Tool(_) => unreachable!("tool selection handled before the runner"),
            MechAction::CheckJoints => unreachable!("the joint check handled before the runner"),
            // the closed box's board is heard, not a sim's
            MechAction::Listen(_) => {}
            MechAction::Release { velocity } => {
                // printer m/s -> the held body's model frame, mm/s
                let velocity = (*velocity).zip(self.grabbed).map(|(v, b)| {
                    let f = &self.bodies[b].from_world;
                    std::array::from_fn(|k| f[k][0] * v[0] + f[k][1] * v[1] + f[k][2] * v[2])
                });
                self.grabbed = None;
                r.send(Command::Release { velocity });
            }
        }
        state.mech.shaking = self.shaking;
        self.frame = cx.new_next_frame();
        cx.redraw_all();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use makepad_csg_mech::geom::{len, rotate, sub};
    use makepad_csg_mech::runner::{dynamics_world, HAND_HZ, HZ, POINTER_HZ};
    use makepad_csg_mech::sim::AnchorKind;

    /// The jewellery box's evaluation, its piece built as Mechanics builds it, and when that began.
    fn jewellery_box() -> (Arc<Evaluated>, Built, std::time::Instant) {
        let d = crate::demo_part::demo("jewellery_box").expect("the jewellery box demo");
        let model = d.model().unwrap_or_else(|e| panic!("{e}"));
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        let mech = ev.mech.first().expect("the box and lid are jointed");
        let opened = std::time::Instant::now();
        let built = jointed(mech, &[]).unwrap();
        (Arc::new(ev), built, opened)
    }

    /// The action bar asked with no sim yet, while the plate is built again, or while the sim shown is
    /// of a plate gone, acts on the sim of the plate that comes: held, not given to the sim shown
    /// meanwhile, and given in order to the sim of the plate as it is once that runs. (A click before
    /// the sim ran was dropped, and one given to a sim about to go went with it.)
    /// A board (its solid linked `board:<name>`) is one body though a part on it stands apart from
    /// its slab, and Mechanics shows no pieces of it: the board the electronics host draws there stays
    /// the one thing in its place, and a pull moves it whole (11-18: the pieces showed white through
    /// the board, and a pull carried them off without it). The same solid unlinked splits.
    #[test]
    fn a_board_is_one_body_though_its_parts_stand_apart() {
        let evaluate = |object: &str| {
            let source = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet slab = m.box({{size: [40mm 1.6mm 30mm]}})\nlet chip = m.box({{size: [5mm 1mm 5mm] at: [0 3mm 0]}})\nobject({object}, name: \"Board\")\n");
            let model = fab_model::FabModel::with_script("board", "board.cad.splash", &source);
            let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
            let (_, ev) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
            ev
        };
        let empty = BTreeMap::new();
        let mut ev = evaluate(r#"m.link(m.union(slab, chip, {frame: Frame{name: "board:demo" at: [3mm 4mm 7mm] rotate: [0 0 30deg]}}), "board:demo")"#);
        let board = free_parts(&ev, "board", None, &empty).unwrap();
        assert_eq!((board.asm.bodies.len(), board.scene.is_none()), (1, true), "the board is one body, with no pieces shown");
        let placed = &ev.plate.excluded[0];
        let mesh = &placed.part.mesh;
        let mesh_mass = makepad_csg_mech::mass::mesh_mass(&mesh.positions, &mesh.triangles, ev.profile.material.density, 1.0);
        assert_eq!(board.asm.bodies[0].mass, mesh_mass, "a board awaiting its design uses its mesh mass");

        let props = BoardMass { mass: 12.0, volume: 2100.0, com: [2.0, -3.0, 5.0], inertia: [[6.0, 0.0, 0.0], [0.0, 8.0, 0.0], [0.0, 0.0, 10.0]] };
        let masses = BTreeMap::from([("demo".into(), props)]);
        let loaded = free_parts(&ev, "board", None, &masses).unwrap();
        let body = &loaded.asm.bodies[0];
        assert_eq!(body.mass.mass, 12.0, "the placed board weighs its electronics mass in grams");
        assert_eq!(body.mass.volume, props.volume, "placement preserves the electronics volume");
        let (_, frame) = placed.frame.as_ref().expect("the board carries its named frame");
        let model_to_plate = m4(&placed.model_to_plate);
        let expected_com = apply(&model_to_plate, apply(&m4(frame), props.com));
        assert!(len(sub(body.mass.com, expected_com)) < 1e-9, "the centre follows the board frame then the plate placement");
        let rotation = mul(&model_to_plate, &m4(frame));
        for row in 0..3 {
            for col in 0..3 {
                let expected: f64 = (0..3).map(|k| rotation[row][k] * props.inertia[k][k] * rotation[col][k]).sum();
                assert!((body.mass.inertia[row][col] - expected).abs() < 1e-9, "inertia ({row}, {col}) follows the placed principal axes");
            }
        }
        assert_eq!(body.hulls, board.asm.bodies[0].hulls, "electronics mass preserves the mesh collision hulls");
        ev.plate.excluded[0].frame = None;
        let unframed = free_parts(&ev, "board", None, &masses).unwrap();
        assert_eq!(unframed.asm.bodies[0].mass, mesh_mass, "a board without a frame keeps its mesh mass");
        let loose = free_parts(&evaluate("m.union(slab, chip)"), "board", None, &empty).unwrap();
        assert_eq!((loose.asm.bodies.len(), loose.scene.is_some()), (2, true), "the same solid unlinked is its two pieces");
    }

    #[test]
    fn a_click_while_the_plate_is_built_waits_for_its_sim() {
        let model = fab_model::FabModel::with_script("block", "block.cad.splash", "use mod.model.*\nuse mod.fab.*\nobject(mod.model.box({size: [20mm 20mm 20mm]}))\n");
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        let ev = Arc::new(ev);
        let built = free_parts(&ev, "block", None, &BTreeMap::new()).unwrap_or_else(|e| panic!("{e}"));
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_: &mut Cx, _: &Event| {}));
        let mut state = AppState::default();
        state.ui.enter(fab::api::Workspace::Mechanics);
        let mut sim = MechSim { speed: Some(1.0), ..Default::default() };
        // what the sim has taken up, once it has answered every command given
        let taken = |sim: &mut MechSim| {
            let r = sim.runner.as_mut().expect("a sim");
            while !r.answered() {
                std::thread::yield_now();
            }
            let s = r.latest();
            (!s.pinned.is_empty(), !s.frozen.is_empty())
        };
        sim.action(&mut cx, &MechAction::Do(MechOp::Pin), &mut state, Some(&ev), false);
        assert_eq!(sim.held.len(), 1, "a click with no sim is held");
        sim.of = Some((ev.content, ev.profile.clone()));
        sim.install(&mut cx, &ev, &mut state, built);
        sim.handle_event(&mut cx, &Event::Signal, &mut state, Some(&ev), true);
        assert_eq!((sim.held.len(), taken(&mut sim)), (1, (false, false)), "what is held is not given while the plate is built again");
        sim.of = Some(([0; 32], ev.profile.clone()));
        sim.action(&mut cx, &MechAction::Do(MechOp::Freeze(true)), &mut state, Some(&ev), false);
        assert_eq!((sim.held.len(), taken(&mut sim)), (2, (false, false)), "a click on the sim of a plate gone is held, not given to it");
        sim.of = Some((ev.content, ev.profile.clone()));
        sim.handle_event(&mut cx, &Event::Signal, &mut state, Some(&ev), false);
        assert_eq!((sim.held.len(), taken(&mut sim)), (0, (true, true)), "the sim of the plate as it is takes both, once it runs");
    }

    /// Taken by its far edge and pulled up and over, the lid turns on its hinge past upright, and the
    /// box stops it: its hinge has no limits declared, so only contact with the box, away from the
    /// hinge's own region, does.
    macro_rules! the_box_stops_the_lid {
        ($w:ident, $asm:ident) => {{
            let hinge = &$asm.joints[0];
            // no limits declared: a full turn either way (the sim then sets no limit on it)
            let pi = std::f64::consts::PI;
            assert!(matches!(hinge.kind, JointKind::Revolute { lower, upper } if lower <= -pi && upper >= pi), "no limits declared: {:?}", hinge.kind);
            let (lid, axis, pivot) = (hinge.b, norm(hinge.axis), hinge.pivot);
            // the lid's point farthest from the hinge's axis: its far edge
            let off_axis = |p: &[f64; 3]| {
                let r = sub(*p, pivot);
                len(sub(r, scale(axis, dot(r, axis))))
            };
            let far = $asm.bodies[lid].hulls.iter().flatten().copied().max_by(|p, q| off_axis(p).total_cmp(&off_axis(q))).expect("the lid has proxies");
            let up = norm($asm.up);
            // the way round that lifts the far edge off the bed
            let turned = |a: f64| makepad_csg_mech::geom::add(pivot, rotate(sub(far, pivot), axis, a));
            let way = if dot(turned(0.1), up) > dot(far, up) { 1.0 } else { -1.0 };
            let dt = 1.0 / HZ;
            $w.grab(lid, far, POINTER_HZ);
            // the hand goes on round to 200° in two seconds, well past where the box closes it
            let steps = (2.5 * HZ) as usize;
            // the turn unwrapped (the joint's angle wraps at a half turn)
            let (mut prev, mut turn, mut most) = ($w.angles($asm)[0], 0.0f64, 0.0f64);
            let mut angles = Vec::new();
            for s in 0..steps {
                let a = way * (200f64.to_radians() * (s as f64 * dt / 2.0).min(1.0));
                $w.drag(turned(a), dt);
                $w.step(dt);
                let ang = $w.angles($asm)[0];
                turn += (ang - prev + pi).rem_euclid(2.0 * pi) - pi;
                prev = ang;
                most = most.max(turn.abs());
                if s % 60 == 0 {
                    angles.push(format!("{:.0}", turn.abs().to_degrees()));
                }
            }
            println!("the hinge's angle, every quarter second: {}°; at most {:.1}°", angles.join(" "), most.to_degrees());
            assert!(most > 120f64.to_radians(), "the lid turns on its hinge, up and over: at most {:.0}°", most.to_degrees());
            assert!(most < 182f64.to_radians(), "the box stops the lid where it closes (a half turn), not the hand at 200°: {:.1}°", most.to_degrees());
        }};
    }

    /// Opening Mechanics on the jewellery box builds its sim at once, on the dynamics engine, from the
    /// joints as declared: no joint check runs first (it once did, minutes before the lid could be
    /// taken; it runs only when asked), not when Mechanics opens and not once its sim runs. The lid is
    /// taken from the sim's first step, and the box stops it (the engine's own filter, which drops
    /// every contact between a body and its parent, would let it through).
    #[test]
    fn the_jewellery_box_lid_swings_at_once_and_stops_at_the_box() {
        let (ev, built, opened) = jewellery_box();
        let asm = &built.asm;
        assert!(makepad_csg_mech::runner::on_dynamics(asm), "a jointed piece runs on the dynamics engine");
        let mut w = dynamics_world(asm, built.gravity, built.floor).unwrap_or_else(|e| panic!("{e}"));
        // (told, not asserted: on a busy box the clock says how busy it is)
        println!("the sim is up {:.3} s after Mechanics opens ({} bodies, {} joints)", opened.elapsed().as_secs_f64(), asm.bodies.len(), asm.joints.len());
        // as declared: a check's measured range would have set the hinge's limits
        assert_eq!(built.verdict, "1 joint · not checked");
        let _cx = crate::cx_shared();
        let mut cx = Cx::new(Box::new(|_: &mut Cx, _: &Event| {}));
        let mut state = AppState::default();
        state.ui.enter(fab::api::Workspace::Mechanics);
        let mut sim = MechSim { speed: Some(1.0), ..Default::default() };
        sim.handle_event(&mut cx, &Event::Signal, &mut state, Some(&ev), false);
        assert!(sim.check.is_none() && (sim.job.is_some() || sim.runner.is_some()), "Mechanics opened builds the sim and starts no joint check: {}", state.mech.status);
        while sim.runner.is_none() {
            assert!(sim.job.is_some(), "the build ended with no sim: {}", state.mech.status);
            std::thread::yield_now();
            sim.handle_event(&mut cx, &Event::Signal, &mut state, Some(&ev), false);
        }
        assert!(sim.check.is_none() && state.mech.ready, "the sim runs with no joint check: {}", state.mech.status);
        assert_eq!(state.mech.verdict, "1 joint · not checked");
        the_box_stops_the_lid!(w, asm);
        assert!(w.resets().is_empty(), "the engine reset it: {:?}", w.resets());
    }

    /// The marionette (its build test runs this on its build's jointed piece) loads as it printed, the jester on its
    /// back beside the control and every string slack.
    /// Hang lifts the control, and the jester comes up on its strings and hangs upright, its lowest point at
    /// least 5 mm over the bed (the lift stops 10 mm clear) and its back string taut. A pointer then carries the
    /// control 100 mm across and 30 mm up in half a second: on every step the held point is under the
    /// pointer (sub-mm), and the back string leaves the control where that snapshot has it, as the view draws
    /// it. Let go, the control stays where it was let go, and the engine never reset.
    pub(crate) fn the_marionette_hangs_on_its_strings_and_follows_the_hand(mech: &fab_model::mech::Mech) {
        let built = jointed(mech, &[]).unwrap();
        let asm = &built.asm;
        let part = |name: &str| asm.bodies.iter().position(|b| b.name.ends_with(name)).unwrap_or_else(|| panic!("no part {name}"));
        let (control, leg_bar, torso, head) = (part("/control"), part("/leg_bar"), part("/torso_shape"), part("/head_shape"));
        assert_eq!((asm.strings.len(), &asm.anchors[0].kind, asm.anchors.len()), (7, &AnchorKind::Held, 1), "seven strings, the control held by hand");
        assert!(built.floor.is_some() && built.hold == m4(&IDENTITY), "it loads on the bed as it printed");
        let floor = built.floor.unwrap();
        let up = norm(asm.up);
        let jester: Vec<usize> = (0..asm.bodies.len()).filter(|i| ![control, leg_bar].contains(i)).collect();
        let lowest = |s: &Snapshot| jester.iter().flat_map(|&i| asm.bodies[i].hulls.iter().flatten().map(move |p| dot(s.poses[i].apply(s.rest[i], *p), up))).fold(f64::INFINITY, f64::min);
        let mut r = Runner::start_with(asm.clone(), built.gravity, Options { floor: built.floor, pool: None, ..Options::default() }, || {});
        let mut s = step_once(&mut r, None);
        assert!(s.error.is_none(), "{:?}", s.error);
        assert!(s.strings.iter().all(|p| p.slack > 1.0), "every string slack as it printed: {:?}", s.strings.iter().map(|p| p.slack).collect::<Vec<_>>());
        r.send(Command::Bodies { bodies: Vec::new(), action: BodyAction::Hang });
        let (mut k, mut lifted) = (0, None);
        while k < (12.0 * HZ) as usize {
            s = step_once(&mut r, None);
            k += 1;
            if lifted.is_none() && lowest(&s) >= floor + makepad_csg_mech::runner::HANG_CLEAR {
                lifted = Some(k as f64 / HZ);
                // and two seconds to settle
                k = (12.0 * HZ) as usize - (2.0 * HZ) as usize;
            }
        }
        let lifted = lifted.unwrap_or_else(|| panic!("Hang did not lift the jester 10 mm clear of the bed in 12 s: lowest point {:.1} mm over it", lowest(&s) - floor));
        let rise = dot(sub(s.poses[control].com, s.rest[control]), up);
        println!("the jester hangs {lifted:.2} s after Hang, the control {rise:.0} mm up; its lowest point {:.1} mm over the bed", lowest(&s) - floor);
        assert!(lowest(&s) - floor > 5.0, "hung, the jester stays clear of the bed: its lowest point {:.1} mm over it", lowest(&s) - floor);
        assert!(dot(sub(s.poses[head].com, s.poses[torso].com), up) > 25.0, "the jester hangs upright, its head over its torso");
        assert!(s.strings[0].slack < 1.0, "the back string holds the jester: slack {:.2} mm", s.strings[0].slack);
        // a pointer takes the control by its anchor and carries it
        let hold = s.poses[control].apply(s.rest[control], asm.anchors[0].pivot);
        r.send(Command::Grab { body: control, point: hold, hz: makepad_csg_mech::runner::POINTER_HZ });
        let (mut off, mut lag) = (0.0_f64, 0.0_f64);
        let steps = (0.5 * HZ) as usize;
        let across = makepad_csg_mech::geom::basis(up).0;
        for k in 1..=steps {
            let t = k as f64 / steps as f64;
            let hand = madd(madd(hold, across, 100.0 * t), up, 30.0 * t);
            s = step_once(&mut r, Some(hand));
            off = off.max(len(sub(s.poses[control].apply(s.rest[control], asm.anchors[0].pivot), hand)));
            let bar = s.poses[control].apply(s.rest[control], asm.strings[0].from);
            let drawn = string_line(&s.strings[0].points, s.strings[0].slack);
            lag = lag.max(len(sub(drawn[0], bar)));
        }
        assert_eq!(s.grabbed, Some(control), "the pointer holds the control");
        assert!(off < 1e-3, "the control stays under the pointer: {off} mm off");
        assert!(lag < 1e-3, "the back string is drawn from the control where the same snapshot has it: {lag} mm behind");
        r.send(Command::Release { velocity: None });
        let let_go = step_once(&mut r, None).poses[control];
        for _ in 0..HZ as usize {
            s = step_once(&mut r, None);
        }
        assert!(len(sub(s.poses[control].com, let_go.com)) < 1e-6 && s.poses[control].rot == let_go.rot, "the control stays where it was let go");
        assert!(s.resets.is_empty(), "the engine reset it: {:?}", s.resets);
    }

    /// The marionette's arms (its build test runs this on its build's jointed piece) print with their hulls sunk
    /// 2.2 mm into the torso's, and turning an arm one way sinks
    /// it a little deeper before it comes clear. With the jester upright as it hangs on its strings (head up, clear of
    /// the bed) and its torso held still, a hand walks an arm's far end round the shoulder at 40 degrees a second, and
    /// the arm goes past a right angle each way.
    pub(crate) fn the_marionettes_arms_turn_out_of_their_print_overlap(mech: &fab_model::mech::Mech) {
        let built = jointed(mech, &[]).unwrap();
        let asm = &built.asm;
        let part = |name: &str| asm.bodies.iter().position(|b| b.name.ends_with(name)).unwrap_or_else(|| panic!("no part {name}"));
        let torso = part("/torso_shape");
        for arm in [part("/upper_arms[3]"), part("/upper_arms[7]")] {
            let j = asm.joints.iter().find(|j| (j.a == torso && j.b == arm) || (j.a == arm && j.b == torso)).expect("a shoulder");
            for sign in [1.0, -1.0] {
                let mut w = dynamics_world(asm, [0.0, 0.0, -GRAVITY], None).unwrap_or_else(|e| panic!("{e}"));
                let rest: Vec<[f64; 3]> = (0..asm.bodies.len()).map(|i| w.rest_com(i)).collect();
                w.apply(&[torso], &makepad_csg_mech::sim::BodyAction::Freeze(true));
                for _ in 0..(0.25 * HZ) as usize {
                    w.step(1.0 / HZ);
                }
                let far = asm.bodies[arm].hulls.iter().flatten().copied().max_by(|p, q| len(sub(*p, j.pivot)).total_cmp(&len(sub(*q, j.pivot)))).unwrap();
                let poses = w.poses();
                let at = poses[arm].apply(rest[arm], far);
                let pivot = poses[torso].apply(rest[torso], j.pivot);
                w.grab(arm, at, HAND_HZ);
                let mut most = 0.0_f64;
                for k in 1..=(3.0 * HZ) as usize {
                    w.drag(makepad_csg_mech::geom::add(pivot, rotate(sub(at, pivot), norm(j.axis), sign * 40f64.to_radians() * k as f64 / HZ)), 1.0 / HZ);
                    w.step(1.0 / HZ);
                    let poses = w.poses();
                    let (p, q) = (poses[torso].rot, poses[arm].rot);
                    most = most.max(2.0 * (p[0] * q[0] + p[1] * q[1] + p[2] * q[2] + p[3] * q[3]).abs().min(1.0).acos());
                }
                assert!(most > 90f64.to_radians(), "{}, sign {sign}: the hand turned it {:.1} degrees", asm.bodies[arm].name, most.to_degrees());
            }
        }
    }

    /// A piece held fixed (a car by its chassis) or by hand shows as it printed: gravity straight down onto a
    /// floor under it, not turned to dangle from the held part. One hung on a pin hangs clear.
    #[test]
    fn a_piece_held_fixed_shows_on_the_bed() {
        let built = |anchor: &str| {
            let src = format!("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet body = m.box({{size: [30mm 8mm 12mm] at: [0, 4mm, 0]}})\nlet flap = m.box({{size: [12mm 8mm 12mm] at: [21mm, 4mm, 0]}})\nlet h = m.hinge(body, flap, {{at: [15mm, 0, 0]}})\nm.anchor(body, {{at: [-10mm, 4mm, 0] {anchor}}})\n");
            let model = fab_model::FabModel::with_script("held", "held.cad.splash", &src);
            let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
            let (_, ev) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
            jointed(ev.mech.first().expect("the body and flap are jointed"), &[]).unwrap()
        };
        let fixed = built("fixed: true");
        let up = norm(fixed.asm.up);
        assert_eq!(fixed.asm.anchors[0].kind, makepad_csg_mech::sim::AnchorKind::Fixed);
        assert!(dot(norm(fixed.gravity), up) < -0.999, "held fixed, gravity is straight down: {:?}", fixed.gravity);
        assert!(fixed.floor.is_some() && fixed.hold == [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]], "as it printed, on a floor");
        let by_hand = built("held: true");
        assert_eq!(by_hand.asm.anchors[0].kind, makepad_csg_mech::sim::AnchorKind::Held);
        assert!(by_hand.floor.is_some() && by_hand.hold == fixed.hold && by_hand.gravity == fixed.gravity, "held by hand, as it printed, on a floor");
        let pinned = built("hole: 2mm");
        assert!(pinned.floor.is_none() && dot(norm(pinned.gravity), up) > -0.5, "hung on a pin, it swings down from it, clear of the bed: {:?}", pinned.gravity);
    }

    /// The roller bearing prints as one part of loose solids (its rings and the rollers between them), and
    /// Mechanics moves each as a body of its own, an object of its own in place of the part. Its script's
    /// hand holds it by the outer ring a little over the bed; turned by a second hand, the inner ring rolls
    /// the rollers round between the rings (rolling at their waists they go round about 0.3 as fast as the
    /// ring; on the bed, which carries them, they skid round at 0.06). The rollers meet the rings through
    /// their distance grids (the rings as hulls would fill the rollers' room).
    #[test]
    fn the_roller_bearing_held_by_its_outer_ring_rolls_its_rollers() {
        let d = crate::demo_part::demo("roller_bearing").expect("the roller bearing demo");
        let model = d.model().unwrap_or_else(|e| panic!("{e}"));
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        let t = std::time::Instant::now();
        let built = free_parts(&ev, "bearing", None, &BTreeMap::new()).unwrap_or_else(|e| panic!("{e}"));
        let (part, asm) = (&ev.plate.parts[0].object, &built.asm);
        println!("the bearing's {} solids built in {:.0} ms ({})", asm.bodies.len(), t.elapsed().as_secs_f64() * 1e3, built.verdict);
        assert!(asm.bodies.len() >= 4, "two rings and the rollers: {} bodies", asm.bodies.len());
        assert!(built.bodies.iter().enumerate().all(|(k, (o, _, _))| *o == crate::plate_scene::link_object(part, k)), "each solid is the part's object ~k");
        let scene = built.scene.as_ref().expect("a scene of the solids");
        let names: HashSet<&str> = scene.elements.iter().map(|e| e.name.as_str()).collect();
        assert!(built.bodies.iter().all(|(o, _, _)| names.contains(o.as_str())) && !names.contains(part.as_str()), "the scene shows the solids in place of the part");
        // the outer ring in hand, over the bed
        let span = |i: usize| {
            let xs = || asm.bodies[i].hulls.iter().flatten().map(|p| p[0]);
            xs().fold(f64::MIN, f64::max) - xs().fold(f64::MAX, f64::min)
        };
        let by_width = |a: &usize, b: &usize| span(*a).total_cmp(&span(*b));
        let outer = (0..asm.bodies.len()).max_by(by_width).unwrap();
        assert!(asm.anchors.len() == 1 && asm.anchors[0].body == outer && asm.anchors[0].kind == AnchorKind::Held, "a hand holds the outer ring: {:?}", asm.anchors);
        let lowest = asm.bodies.iter().flat_map(|b| b.hulls.iter().flatten()).map(|p| p[2]).fold(f64::MAX, f64::min);
        assert!(lowest > HELD_OVER_BED - 0.01 && built.verdict.contains("in hand"), "held {lowest:.2} mm over the bed: {}", built.verdict);
        assert!(makepad_csg_mech::runner::on_dynamics(asm), "solids that meet through their fields run on the dynamics engine");
        let inner = (0..asm.bodies.len()).filter(|&i| i != outer).max_by(by_width).unwrap();
        let rollers: Vec<usize> = (0..asm.bodies.len()).filter(|&i| i != outer && i != inner).collect();
        let mut w = dynamics_world(asm, built.gravity, built.floor).unwrap_or_else(|e| panic!("{e}"));
        // the bearing's axis: through the middle of the outer ring
        let rim = asm.bodies[outer].hulls.iter().flatten().fold(([f64::MAX; 3], [f64::MIN; 3]), |(lo, hi), p| (std::array::from_fn(|k| lo[k].min(p[k])), std::array::from_fn(|k| hi[k].max(p[k]))));
        let axis: [f64; 3] = std::array::from_fn(|k| 0.5 * (rim.0[k] + rim.1[k]));
        let around = |p: [f64; 3]| (p[1] - axis[1]).atan2(p[0] - axis[0]);
        let unwrap = |d: f64| d - (d / std::f64::consts::TAU).round() * std::f64::consts::TAU;
        let rests: Vec<[f64; 3]> = (0..asm.bodies.len()).map(|i| w.rest_com(i)).collect();
        let turn = |s: &makepad_csg_mech::sim::Pose, i: usize| {
            let rest = rests[i];
            let p = s.apply(rest, [rest[0] + 1.0, rest[1], rest[2]]);
            (p[1] - s.com[1]).atan2(p[0] - s.com[0])
        };
        for _ in 0..(HZ / 4.0) as usize {
            w.step(1.0 / HZ);
        }
        let start = w.poses();
        // the hand takes the inner ring between its bore and its track and turns it half a turn a second
        let r = 0.3 * span(inner);
        let z = start[inner].com[2];
        w.grab(inner, [axis[0] + r, axis[1], z], HAND_HZ);
        let (mut last, mut ring, mut orbit) = (start.clone(), 0.0, vec![0.0; rollers.len()]);
        for step in 1..=(0.75 * HZ) as usize {
            let a = std::f64::consts::PI * step as f64 / HZ;
            w.drag([axis[0] + r * a.cos(), axis[1] + r * a.sin(), z], 1.0 / HZ);
            w.step(1.0 / HZ);
            let now = w.poses();
            ring += unwrap(turn(&now[inner], inner) - turn(&last[inner], inner));
            for (k, &i) in rollers.iter().enumerate() {
                orbit[k] += unwrap(around(now[i].com) - around(last[i].com));
            }
            last = now;
        }
        let rolled = orbit.iter().sum::<f64>() / orbit.len() as f64 / ring;
        println!("the inner ring turned {:.0}°, the rollers went round {:.2} as fast", ring.to_degrees(), rolled);
        assert!(ring.to_degrees() > 90.0, "the hand turned the inner ring only {:.0}°", ring.to_degrees());
        assert!(rolled > 0.2, "the rollers skid: they went round {rolled:.2} as fast as the inner ring");
        let held = len(sub(last[outer].com, start[outer].com));
        assert!(held < 0.1, "the hand let the outer ring move {held:.3} mm");
        assert!(w.resets().is_empty(), "the engine reset it: {:?}", w.resets());
    }

    /// A held piece moves only as its joints let it: the way the first joint out from a part (through what is
    /// welded to it) carries its point, never down into the bed.
    fn joint_way(asm: &makepad_csg_mech::sim::Assembly, part: usize, point: [f64; 3], down: [f64; 3]) -> Option<[f64; 3]> {
        let mut welded = vec![false; asm.bodies.len()];
        welded[part] = true;
        for _ in 0..asm.bodies.len() {
            for j in &asm.joints {
                if j.kind.holds() && (welded[j.a] || welded[j.b]) {
                    welded[j.a] = true;
                    welded[j.b] = true;
                }
            }
        }
        let j = asm.joints.iter().find(|j| !j.kind.holds() && welded[j.a] != welded[j.b])?;
        let t = if matches!(j.kind, makepad_csg_mech::sim::JointKind::Prismatic { .. }) {
            j.axis
        } else {
            makepad_csg_mech::geom::cross(j.axis, sub(point, j.pivot))
        };
        if len(t) < 1e-6 {
            return None;
        }
        Some(if dot(t, down) > 0.0 { scale(norm(t), -1.0) } else { norm(t) })
    }

    /// One step of a paused live sim, its hand moved to `hand` first, once the sim has answered.
    fn step_once(r: &mut Runner, hand: Option<[f64; 3]>) -> Arc<Snapshot> {
        if let Some(point) = hand {
            r.send(Command::Drag { point });
        }
        r.send(Command::Step);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !r.answered() {
            assert!(std::time::Instant::now() < until, "the sim did not answer a step");
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        r.latest()
    }

    /// A demo's jointed piece (its build test runs this on its build: [`JOINTED_DEMOS`]), as the demo builds on
    /// Fab's default printer, opens in Mechanics, live as
    /// Mechanics runs it (one step at a time here), on the dynamics engine (a flexing one too). Left alone on the
    /// bed it stays where it printed, 1 mm at most. A hand
    /// takes its part farthest out by its farthest point, pulls it on out across gravity at 300 mm/s for half
    /// a second, holds it half a second and lets go: within three seconds a piece on the bed is at rest (no part
    /// faster than 1 mm/s for a quarter second), the engine never reset, a piece it put back at its print pose
    /// looks at rest, and no joint ever opened (its pivot as each of its parts carries it, 0.01 mm apart at
    /// most).
    pub(crate) fn grabbed_released_comes_to_rest(key: &str, mech: &fab_model::mech::Mech) {
        let built = jointed(mech, &[]).unwrap_or_else(|e| panic!("{key}: {e}"));
        let asm = built.asm.clone();
        assert!(makepad_csg_mech::runner::on_dynamics(&asm), "{key}: a jointed piece runs on the dynamics engine, a flexing one too");
        let mut r = Runner::start_with(asm.clone(), built.gravity, Options { floor: built.floor, pool: None, ..Options::default() }, || {});
        let opened = |s: &Snapshot| {
            asm.joints.iter().filter(|j| !j.kind.holds()).map(|j| len(sub(s.poses[j.a].apply(s.rest[j.a], j.pivot), s.poses[j.b].apply(s.rest[j.b], j.pivot)))).fold(0.0, f64::max)
        };
        let mut s = step_once(&mut r, None);
        assert!(s.error.is_none(), "{key}: {:?}", s.error);
        for _ in 1..(0.25 * HZ) as usize {
            s = step_once(&mut r, None);
        }
        // a piece that prints and rests stays where it printed; what hangs settles on its anchor
        if built.floor.is_some() {
            let moved = s.poses.iter().zip(&s.rest).map(|(p, r)| len(sub(p.com, *r))).fold(0.0, f64::max);
            assert!(moved <= 1.0, "{key}: {moved:.2} mm from where it printed, with no hand on it");
        }
        let mut most_open = opened(&s);
        // the part farthest out that a hand can swing (joined to the first anchor, not itself fixed), by its
        // point farthest out, pulled on out across gravity, or for a held piece the way its joint lets it go
        let centroid = asm.centroid();
        let mut joined = vec![asm.anchors.is_empty(); asm.bodies.len()];
        if let Some(a) = asm.anchors.first() {
            joined[a.body] = true;
        }
        for _ in 0..asm.bodies.len() {
            for j in &asm.joints {
                if joined[j.a] || joined[j.b] {
                    joined[j.a] = true;
                    joined[j.b] = true;
                }
            }
        }
        let fixed = |i: usize| asm.anchors.iter().any(|a| a.body == i && a.kind == makepad_csg_mech::sim::AnchorKind::Fixed);
        let body = (0..asm.bodies.len()).filter(|&i| joined[i] && !fixed(i)).max_by(|a, b| len(sub(s.rest[*a], centroid)).total_cmp(&len(sub(s.rest[*b], centroid)))).unwrap_or_else(|| panic!("{key}: no parts"));
        let far = asm.bodies[body].hulls.iter().flatten().copied().max_by(|p, q| len(sub(*p, centroid)).total_cmp(&len(sub(*q, centroid)))).unwrap_or_else(|| panic!("{key}: part {body} has no proxies"));
        let at = s.poses[body].apply(s.rest[body], far);
        let down = norm(built.gravity);
        let out = sub(far, centroid);
        let across = sub(out, scale(down, dot(out, down)));
        let free = if len(across) > 1e-9 { norm(across) } else { makepad_csg_mech::geom::basis(down).0 };
        let way = if asm.anchors.is_empty() { free } else { joint_way(&asm, body, far, down).unwrap_or(free) };
        r.send(Command::Grab { body, point: at, hz: HAND_HZ });
        let pull = (0.5 * HZ) as usize;
        for k in 1..=2 * pull {
            s = step_once(&mut r, Some(makepad_csg_mech::geom::madd(at, way, 300.0 * (k.min(pull) as f64 / HZ))));
            most_open = most_open.max(opened(&s));
        }
        assert_eq!(s.grabbed, Some(body), "{key}: the hand holds part {body}");
        let carried = len(sub(s.poses[body].apply(s.rest[body], far), at));
        assert!(carried > 5.0, "{key}: the hand carried part {body} only {carried:.2} mm");
        r.send(Command::Release { velocity: None });
        let (mut still, mut rested) = (0, None);
        let mut before = s.poses.clone();
        for k in 1..=(3.0 * HZ) as usize {
            s = step_once(&mut r, None);
            most_open = most_open.max(opened(&s));
            let fastest = before.iter().zip(&s.poses).map(|(a, b)| len(sub(b.com, a.com)) * HZ).fold(0.0, f64::max);
            before = s.poses.clone();
            still = if fastest < 1.0 { still + 1 } else { 0 };
            if still >= (0.25 * HZ) as usize {
                rested = Some((k - still) as f64 / HZ);
                break;
            }
        }
        println!("{key}: {} parts, {} joints; part {body} pulled 150 mm and let go: at rest {}; joints open at most {most_open:.2e} mm",
            asm.bodies.len(), asm.joints.len(), rested.map_or("not within 3 s".into(), |t| format!("{t:.2} s after")));
        // a piece the engine put back at its print pose looks at rest: any reset fails it
        assert!(s.resets.is_empty(), "{key}: the engine reset it: {:?}", s.resets);
        // what hangs swings on: the census judges how its energy decays against the other engine
        assert!(rested.is_some() || built.floor.is_none(), "{key}: still moving 3 s after the hand let go");
        assert!(most_open < 0.01, "{key}: a joint opened {most_open:.4} mm");
    }

    /// The demos with a jointed piece: each one's build test checks it ([`grabbed_released_comes_to_rest`]) on its
    /// build, and checks that the list names every demo with one.
    pub(crate) const JOINTED_DEMOS: &[&str] = &["jewellery_box", "articulated_dragon", "poseable_robot", "little_car", "servo_arm", "rally_car", "marionette", "dune_buggy", "roadster", "concept_car", "lizard_puppet", "turing_lizard", "flex_dragon", "flex_snake", "flex_acrobat", "poseable_athlete", "flex_jellyfish", "flex_beetle", "flex_flower"];
}
