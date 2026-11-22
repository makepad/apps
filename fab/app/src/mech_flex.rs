//! A flexible part a row of parts is bonded to (`m.bond` to a TPU spine), as Mechanics shows it
//! bending with them: the sim has it ride the first of them (it weighs next to nothing there), so
//! the view cuts its mesh into pieces instead. A piece the part holds rides that part; a piece in
//! a free run between two parts is a slab bent between them as the sim bends them, on a curve
//! tangent to both ends of the run, its turn shared out along the run.

use crate::mechanics::{apply, M4};
use fab_loose::mesh::PieceMesh;
use fab_model::eval::Evaluated;
use fab_model::mech::Mech;
use makepad_csg_mech::geom::{dot, len, madd, norm, rotate, scale, sub, P3};
use makepad_csg_mech::Flex;
use makepad_widgets::makepad_platform::thread::Class;
use makepad_widgets::Cx;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Slabs per free run.
const SLABS: usize = 12;

/// How a piece moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Ride {
    /// With the body that holds it.
    With(usize),
    /// Bent between the two bodies of flex `flex` (of [`Mech::plan`]'s flexes), `t` of the way
    /// along its run from its `a`.
    Bend { flex: usize, t: f64 },
}

/// One piece: its scene object's name, the plate part it is of (its object), its mesh where the
/// plate has it (plate mm), how it moves.
pub(crate) struct FlexPiece {
    pub(crate) name: String,
    pub(crate) object: String,
    pub(crate) mesh: PieceMesh,
    pub(crate) ride: Ride,
}

/// What the sim's view needs to show the flexible parts bent: the pieces, the flexes they bend
/// on, the body whose frame they are in (a part of the piece: all print as one group), and the
/// flexible parts' plate objects, which the scene leaves out.
pub(crate) struct Riders {
    pub(crate) pieces: Vec<FlexPiece>,
    pub(crate) flexes: Vec<Flex>,
    pub(crate) frame: usize,
    pub(crate) spines: Vec<String>,
}

/// Measure the bends of the plate's flexing pieces right after its build (each piece once), on
/// the workpool below the shown plate's slice: [`crate::model_host::HostAction::BendsMeasured`]
/// says when they are in.
pub(crate) fn measure_bends(cx: &Cx, ev: &Arc<Evaluated>) {
    let todo: Vec<Arc<Mech>> = ev.mech.iter().filter(|m| m.ask_measure()).cloned().collect();
    if todo.is_empty() {
        return;
    }
    let content = ev.content;
    let task = cx.task_pool().submit_named(Class::Background, "fab bends", move || {
        for m in &todo {
            let _ = m.measure_flexes();
        }
        Cx::post_action(crate::model_host::HostAction::BendsMeasured(content));
    });
    // a closed pool (the app ends) measures nothing
    if let Ok(task) = task {
        task.detach();
    }
}

/// What Mechanics says of a flexing piece's bends: still measuring (after an edit, while the
/// sim `shown` shows the piece as it was: that sim is out of date), or why they can't be.
/// None once they are measured.
pub(crate) fn bends_status(mech: &Mech, shown: bool) -> Option<String> {
    match mech.flexes() {
        Some(Ok(_)) => None,
        Some(Err(f)) => Some(format!("The bends can't be measured: {f}")),
        None if shown => Some("Out of date: measuring the edited piece's bends…".into()),
        None => Some("Measuring how the bonded parts bend…".into()),
    }
}

/// The riders of `mech`, if it has a flexible part and its bends are measured (it waits for
/// them).
pub(crate) fn riders(mech: &Mech, refs: &[(String, [[f64; 4]; 3])]) -> Option<Riders> {
    let flexes = mech.measure_flexes().as_ref().ok()?;
    let (pieces, spines) = pieces(mech, flexes, refs);
    let frame = *spines.first()?;
    Some(Riders { pieces, flexes: flexes.clone(), frame, spines: spines.iter().filter_map(|s| Some(refs.get(*s)?.0.clone())).collect() })
}

/// The pieces for a scene: each in its plate part's colour.
pub(crate) fn scene_pieces<'a>(ev: &Evaluated, pieces: &'a [FlexPiece]) -> Vec<(String, &'a PieceMesh, [f32; 4])> {
    pieces.iter().map(|p| (p.name.clone(), &p.mesh, ev.plate.parts.iter().find(|q| q.object == p.object).map_or([0.7, 0.7, 0.7, 1.0], |q| q.color))).collect()
}

/// The pieces of the piece's flexible parts (each flexible bond's `to`), and those parts' plan
/// indices: a scene shows the pieces in their place. `flexes`: its measured bends; `refs`: per
/// plan part, its plate part's object and model -> plate frame.
pub(crate) fn pieces(mech: &Mech, flexes: &[Flex], refs: &[(String, [[f64; 4]; 3])]) -> (Vec<FlexPiece>, Vec<usize>) {
    let mut out = Vec::new();
    let mut spines = Vec::new();
    for bond in mech.plan.bonds.iter().filter(|b| b.flexible) {
        let (Some(m), Some((object, to_plate))) = (mech.finals.get(bond.to), refs.get(bond.to)) else { continue };
        spines.push(bond.to);
        let mine: Vec<usize> = (0..flexes.len()).filter(|&k| bond.parts.contains(&flexes[k].a)).collect();
        // by a triangle's centre: the nearest free run's slab when it is in that run, else the
        // part on its side of it
        let ride = |c: P3| -> (usize, Ride) {
            let Some(k) = mine.iter().copied().min_by(|&x, &y| len(sub(c, flexes[x].pivot)).total_cmp(&len(sub(c, flexes[y].pivot)))) else {
                return (bond.parts[0], Ride::With(bond.parts[0]));
            };
            let f = &flexes[k];
            let s = dot(sub(c, f.pivot), f.axis) / f.length + 0.5;
            if s <= 0.0 {
                (f.a, Ride::With(f.a))
            } else if s >= 1.0 {
                (f.b, Ride::With(f.b))
            } else {
                let i = ((s * SLABS as f64) as usize).min(SLABS - 1);
                (refs.len() + k * SLABS + i, Ride::Bend { flex: k, t: (i as f64 + 0.5) / SLABS as f64 })
            }
        };
        let mut groups: BTreeMap<usize, (Ride, Vec<usize>)> = BTreeMap::new();
        for (t, tri) in m.triangles.iter().enumerate() {
            let p = tri.map(|i| m.vertices[i as usize]);
            let c = [(p[0].x + p[1].x + p[2].x) / 3.0, (p[0].y + p[1].y + p[2].y) / 3.0, (p[0].z + p[1].z + p[2].z) / 3.0];
            let (key, r) = ride(c);
            groups.entry(key).or_insert((r, Vec::new())).1.push(t);
        }
        for (key, (ride, tris)) in groups {
            let name = format!("{}mech-flex-{object}-{key}", fab::api::CONTEXT_PREFIX);
            out.push(FlexPiece { name, object: object.clone(), mesh: crate::mech_check::piece_mesh(&m.vertices, &m.triangles, &m.corner_normals, &mut tris.into_iter(), to_plate), ride });
        }
    }
    (out, spines)
}

fn rotation(m: &M4) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| m[i][j]))
}

fn mul3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// The move of a slab `t` of the way along flex `f`'s run (model mm), from the moves of its two
/// bodies: its rest point on the run goes onto the curve from `a`'s end of the run to `b`'s,
/// tangent to both (through where the two take the middle), turned `t` of the way from `a`'s turn
/// to `b`'s. At 0 it moves as `a`, at 1 as `b`.
pub(crate) fn bend(a: &M4, b: &M4, f: &Flex, t: f64) -> M4 {
    let h = 0.5 * f.length;
    let (p0, p2) = (apply(a, madd(f.pivot, f.axis, -h)), apply(b, madd(f.pivot, f.axis, h)));
    let (ma, mb) = (apply(a, f.pivot), apply(b, f.pivot));
    let p1 = scale([ma[0] + mb[0], ma[1] + mb[1], ma[2] + mb[2]], 0.5);
    let (u, v, w) = ((1.0 - t) * (1.0 - t), 2.0 * t * (1.0 - t), t * t);
    let at: P3 = std::array::from_fn(|k| u * p0[k] + v * p1[k] + w * p2[k]);
    // a's turn, then t of the turn from a's to b's
    let (ra, rb) = (rotation(a), rotation(b));
    let rel: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| ra[k][i] * rb[k][j]).sum()));
    let rel4: M4 = [[rel[0][0], rel[0][1], rel[0][2], 0.0], [rel[1][0], rel[1][1], rel[1][2], 0.0], [rel[2][0], rel[2][1], rel[2][2], 0.0], [0.0, 0.0, 0.0, 1.0]];
    let (axis, angle) = crate::mechanics::axis_angle(&rel4);
    let part: [[f64; 3]; 3] = {
        let cols = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|e| rotate(e, norm(axis), t * angle));
        std::array::from_fn(|i| std::array::from_fn(|j| cols[j][i]))
    };
    let r = mul3(&ra, &part);
    // x -> at + r (x - c), c the slab's rest point on the run
    let c = madd(f.pivot, f.axis, (2.0 * t - 1.0) * h);
    let rc: P3 = std::array::from_fn(|i| (0..3).map(|k| r[i][k] * c[k]).sum());
    [[r[0][0], r[0][1], r[0][2], at[0] - rc[0]], [r[1][0], r[1][1], r[1][2], at[1] - rc[1]], [r[2][0], r[2][1], r[2][2], at[2] - rc[2]], [0.0, 0.0, 0.0, 1.0]]
}

#[cfg(test)]
mod tests {
    /// Mechanics on a flexing piece: measuring its bends until they are in; after an edit, the
    /// sim it still shows (of the piece as it was) is out of date until the new piece's bends
    /// are in; a bend that can't be measured says why.
    #[test]
    fn an_edit_leaves_the_bends_shown_out_of_date_until_measured() {
        let build = |pitch: &str| {
            let script = String::from("use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet spine = m.cylinder({radius: 1.5mm height: 50mm at: [25mm 2.5mm 0] rotate: [0, 0, 90°]})\nlet segs = for i in 0..3 { m.box({size: [10mm 5mm 8mm] at: [i * ") + pitch + " + 9mm 2.5mm 0]}) }\nlet back = m.bond(spine, segs)\nm.fill(spine, Material{albedo: [0.1 0.1 0.12] name: \"Spine\"})\n";
            let project = "use mod.fab.*\nscript(\"row.cad.splash\")\ncolor(\"Spine\", rgb: #x1a1a20, material: \"TPU\")\n";
            let model = fab_model::FabModel::load("row", project, [("row.cad.splash".to_string(), script)].into_iter().collect()).unwrap();
            let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
            model.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1.mech[0].clone()
        };
        let first = build("16mm");
        assert!(super::bends_status(&first, false).is_some_and(|s| s.starts_with("Measuring")));
        assert!(first.measure_flexes().is_ok());
        assert_eq!(super::bends_status(&first, true), None, "measured: the sim goes on them");
        // an edit: the sim shown is of the old piece
        let edited = build("14mm");
        assert!(super::bends_status(&edited, true).is_some_and(|s| s.starts_with("Out of date")), "{:?}", super::bends_status(&edited, true));
        assert!(edited.measure_flexes().is_ok());
        assert_eq!(super::bends_status(&edited, true), None);
        let bad = build("8mm");
        let _ = bad.measure_flexes();
        assert!(super::bends_status(&bad, false).is_some_and(|s| s.starts_with("The bends can't be measured") && s.contains("meet round")));
    }

    use super::*;
    use makepad_csg_mech::flex::Section;

    /// A slab moves as `a` at the run's start and as `b` at its end; bent a quarter turn about
    /// the run's middle, the slab halfway along sits on the curve, off the straight line between
    /// the ends, turned an eighth.
    #[test]
    fn a_slab_bends_from_one_end_to_the_other() {
        let section = Section { centroid: [0.0; 3], area: 4.0, moments: (1.0, 1.0), soft_axis: [0.0, 0.0, 1.0] };
        let f = Flex { a: 0, b: 1, pivot: [10.0, 0.0, 0.0], axis: [1.0, 0.0, 0.0], length: 4.0, section };
        let id: M4 = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        // b turned a quarter about Z through the pivot
        let b: M4 = [[0.0, -1.0, 0.0, 10.0], [1.0, 0.0, 0.0, -10.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let close = |m: &M4, n: &M4| (0..3).all(|i| (0..4).all(|j| (m[i][j] - n[i][j]).abs() < 1e-9));
        assert!(close(&bend(&id, &b, &f, 0.0), &id) && close(&bend(&id, &b, &f, 1.0), &b));
        let mid = bend(&id, &b, &f, 0.5);
        let at = apply(&mid, [10.0, 0.0, 0.0]);
        // the ends at (8, 0) and (10, 2), the curve through the corner at (10, 0): halfway, (9.5, 0.5)
        assert!(len(sub(at, [9.5, 0.5, 0.0])) < 1e-9, "{at:?}");
        let (_, angle) = crate::mechanics::axis_angle(&mid);
        assert!((angle - std::f64::consts::FRAC_PI_4).abs() < 1e-9, "{angle}");
    }
}
