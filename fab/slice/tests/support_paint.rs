//! A script's `m.support_block` and `m.support_enforce` reach what prints, through the model, the
//! part's paint and the slice, in both planners.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use fab_model::{Change, PlaceArg};
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_toolpath::role::Role;
use makepad_csg_sdf::strokes::{StrokeOp, Strokes, Swatch as StrokeSwatch};
use std::collections::BTreeMap;
use std::sync::Arc;

/// A cap on a stem: an overhang all round at 6 mm, in model millimetres (y up).
const CAP: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet stem = m.box({size: [4mm 6mm 4mm] at: [0 3mm 0]})\nlet cap = m.box({size: [16mm 1mm 16mm] at: [0 6.5mm 0]})\nlet part = m.union(stem, cap)\nobject(part)\n";
/// A wedge: its +x face 30 degrees off vertical, which the angle leaves alone.
const WEDGE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet part = m.sdf({bounds: [[-6mm, 0, -6mm], [20mm, 9mm, 6mm]] cell: 0.5mm field: fn(p) { return sdf_intersection(sdf_box(p - vec3(7mm, 4.5mm, 0), vec3(12mm, 4.5mm, 5mm)), p.x * 0.866 - p.y * 0.5 - 6.06mm) }})\nobject(part)\n";

fn build_with(source: &str, strokes: BTreeMap<String, Arc<Strokes>>) -> Evaluated {
    build_placed(source, strokes, &[])
}

/// The script placed, then `changes` made to the plate (moves, turns, clones, copies).
fn build_placed(source: &str, strokes: BTreeMap<String, Arc<Strokes>>, changes: &[Change]) -> Evaluated {
    let m = FabModel::with_script_files("part", "part.cad.splash", source, BTreeMap::new(), strokes);
    if !changes.is_empty() {
        let mut e = Evaluator::new(Machine::dual_nozzle_350());
        return m.preview(&mut e, changes, None).unwrap_or_else(|f| panic!("{f:?}")).2;
    }
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

fn setup(kind: &str) -> JobSetup {
    let mut process = Preset::default();
    process.values.set_str("layer_height", "0.3");
    process.values.set_str("enable_support", "1");
    process.values.set_str("support_type", kind);
    process.values.set_str("support_threshold_angle", "45");
    // The app's default printer's bed, as the app lists it.
    let mut machine = Preset::default();
    machine.values.set_vec("printable_area", Machine::dual_nozzle_350().bed.corners());
    JobSetup { machine, process, filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

/// What the part's supports print, as (x, y, z, whether a roof) points at most 0.5 mm apart.
fn support_points(source: &str, kind: &str) -> (Evaluated, Vec<(f64, f64, f64, bool)>) {
    support_points_with(source, BTreeMap::new(), kind)
}

fn support_points_with(source: &str, strokes: BTreeMap<String, Arc<Strokes>>, kind: &str) -> (Evaluated, Vec<(f64, f64, f64, bool)>) {
    supports_of(build_with(source, strokes), kind)
}

fn supports_of(ev: Evaluated, kind: &str) -> (Evaluated, Vec<(f64, f64, f64, bool)>) {
    let frames: Vec<Frame> = ev.frames.iter().zip(&ev.fields).map(|(fr, pf)| Frame {
        local: fr.local.clone(),
        offset: fr.offset,
        field: pf.as_ref().map(|pf| {
            let mut to = pf.to_printer;
            for (r, o) in fr.offset.iter().enumerate() {
                to[r][3] -= o;
            }
            PartField::new(pf.shape.clone(), to)
        }),
    }).collect();
    let res = slice_frames(&setup(kind), &ev.plate, &frames, &[], None).expect("slices");
    let mut out = Vec::new();
    for layer in res.layers() {
        for ol in &layer.objects {
            for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                let roof = matches!(p.role, Role::SupportInterface | Role::SupportTransition);
                if p.role != Role::Support && !roof {
                    continue;
                }
                // every 0.5 mm along the path, its straight runs too
                let z = layer.print_z - 0.5 * layer.height;
                let mut last: Option<(f64, f64)> = None;
                for q in &p.pts {
                    let (x, y) = (q.x_mm(), q.y_mm());
                    let (lx, ly) = last.unwrap_or((x, y));
                    let n = ((x - lx).hypot(y - ly) / 0.5).floor() as usize;
                    for k in 1..=n {
                        let t = k as f64 / n as f64;
                        out.push((lx + (x - lx) * t, ly + (y - ly) * t, z, roof));
                    }
                    if last.is_none() {
                        out.push((x, y, z, roof));
                    }
                    last = Some((x, y));
                }
            }
        }
    }
    (ev, out)
}

/// The part's model point in printer millimetres.
fn printer(ev: &Evaluated, p: [f64; 3]) -> [f64; 3] {
    printer_of(ev, 0, p)
}

/// Part `i`'s model point in printer millimetres.
fn printer_of(ev: &Evaluated, i: usize, p: [f64; 3]) -> [f64; 3] {
    let m = ev.paint[i].as_ref().map(|paint| paint.to_printer).unwrap_or_else(|| panic!("the script paints"));
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// The block over half the cap: no contact under that half, tree or normal, and the other half
/// keeps its. Without the statement both halves have it.
#[test]
fn a_scripts_block_takes_that_support_in_both_planners() {
    for kind in ["tree(auto)", "normal(auto)"] {
        let (_, free) = support_points(CAP, kind);
        let block = "m.support_block(m.box({size: [9mm 2mm 20mm] at: [4.5mm 6mm 0]}))\n";
        let (ev, points) = support_points(&format!("{CAP}{block}"), kind);
        // Which printer side the block is on, from the part's own middle: the stem's.
        let (mid, side) = (printer(&ev, [0.0, 0.0, 0.0]), printer(&ev, [4.5, 6.0, 0.0]));
        let over = |p: &(f64, f64, f64, bool)| p.3 && (p.0 - mid[0]) * (side[0] - mid[0]) + (p.1 - mid[1]) * (side[1] - mid[1]) > 1.0;
        let under = |p: &(f64, f64, f64, bool)| p.3 && (p.0 - mid[0]) * (side[0] - mid[0]) + (p.1 - mid[1]) * (side[1] - mid[1]) < -3.0;
        let z_of = |list: &[(f64, f64, f64, bool)], f: &dyn Fn(&(f64, f64, f64, bool)) -> bool| list.iter().filter(|p| f(p)).count();
        assert!(z_of(&free, &over) > 50, "{kind}: without the block that half has a roof: {}", z_of(&free, &over));
        assert_eq!(z_of(&points, &over), 0, "{kind}: a roof under the block");
        assert!(z_of(&points, &under) > 50, "{kind}: the other half keeps its roof: {}", z_of(&points, &under));
    }
}

/// An enforced face the angle leaves alone gets support in both planners; the statement is what adds
/// it, and block on the same spot wins.
#[test]
fn a_scripts_enforce_adds_support_and_block_wins_in_both_planners() {
    for kind in ["tree(auto)", "normal(auto)"] {
        assert_eq!(support_points(WEDGE, kind).1.len(), 0, "{kind}: the angle leaves the wedge alone");
        let region = "m.box({size: [30mm 5mm 8mm] at: [10mm 7mm 0]})";
        let enforced = format!("{WEDGE}m.support_enforce({region})\n");
        assert!(support_points(&enforced, kind).1.len() > 50, "{kind}: enforced, it has support");
        assert_eq!(support_points(&format!("{enforced}m.support_block({region})\n"), kind).1.len(), 0, "{kind}: block after enforce wins");
        assert_eq!(support_points(&format!("{WEDGE}m.support_block({region})\nm.support_enforce({region})\n"), kind).1.len(), 0, "{kind}: block before enforce wins");
    }
}

/// No trees here: a box on the bed under the cap's +x side, half the way up to its underside,
/// typed into the script. No tree stands or grows in it (all of it under the cap's overhang):
/// they lean round it. Without the statement they stand there.
#[test]
fn a_no_trees_box_keeps_every_tree_out_of_it() {
    let area = "m.no_trees(m.box({size: [5mm 3mm 6mm] at: [5mm 1.5mm 0]}))\n";
    let (ev, points) = support_points(&format!("{CAP}{area}"), "tree(auto)");
    let (_, free) = support_points(CAP, "tree(auto)");
    // The box in printer millimetres: its corners' box (the part moves and scales).
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for c in 0..8 {
        let q = printer(&ev, [if c & 1 == 0 { 2.5 } else { 7.5 }, if c & 2 == 0 { 0.0 } else { 3.0 }, if c & 4 == 0 { -3.0 } else { 3.0 }]);
        for k in 0..3 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    // A path's middle in the box, past what its outline is traced to.
    let inside = |p: &&(f64, f64, f64, bool)| p.2 < hi[2] && p.0 > lo[0] + 0.1 && p.0 < hi[0] - 0.1 && p.1 > lo[1] + 0.1 && p.1 < hi[1] - 0.1;
    let (was, now) = (free.iter().filter(inside).count(), points.iter().filter(inside).count());
    let roofs = |list: &[(f64, f64, f64, bool)]| list.iter().filter(|p| p.3).count();
    eprintln!("no trees here: {was} points in the box without it, {now} with it; roofs {} and {}", roofs(&free), roofs(&points));
    assert!(was > 20, "without the statement trees stand in the box: {was}");
    assert_eq!(now, 0, "a tree in the no-trees box");
    assert!(roofs(&points) > 50, "the cap keeps its trees: {} roof points", roofs(&points));
}

/// The brush's block on the cap: a strokes file read by `m.support_block`, one stroke along
/// the underside's +x edge (model frame).
fn brushed() -> (String, BTreeMap<String, Arc<Strokes>>) {
    let mut log = Strokes::new();
    let slot = log.slot_for(StrokeSwatch { rgb: [0.85, 0.08, 0.35], filament: None });
    let points: Vec<[f64; 3]> = (0..12).map(|k| [6.0, 6.0, -7.0 + k as f64 * 14.0 / 11.0]).collect();
    log.push(StrokeOp::Paint { slot }, 3.0, 1.0, &points);
    (format!("{CAP}m.support_block(\"part.block.strokes\")\n"), BTreeMap::from([("part.block.strokes".to_string(), Arc::new(log))]))
}

/// Roofs under part `i`'s brushed band and under its far side: (band, far). `along` is mm from
/// the part's middle toward the stroke, in the part's own frame wherever it stands.
fn sides(ev: &Evaluated, i: usize, points: &[(f64, f64, f64, bool)]) -> (usize, usize) {
    let (mid, side) = (printer_of(ev, i, [0.0, 0.0, 0.0]), printer_of(ev, i, [1.0, 6.0, 0.0]));
    let (dx, dy) = (side[0] - mid[0], side[1] - mid[1]);
    // under the cap: within its 8 mm half-width across the stroke too
    let mine = points.iter().filter(|p| p.3 && ((p.0 - mid[0]) * -dy + (p.1 - mid[1]) * dx).abs() < 8.0 && (p.0 - mid[0]).hypot(p.1 - mid[1]) < 13.0);
    let along: Vec<f64> = mine.map(|p| (p.0 - mid[0]) * dx + (p.1 - mid[1]) * dy).collect();
    (along.iter().filter(|a| (4.0..7.0).contains(*a)).count(), along.iter().filter(|a| **a < -2.5).count())
}

/// The brush's block takes the support under its stroke in both planners and leaves the far
/// side's.
#[test]
fn a_brushed_block_takes_the_support_under_its_stroke_in_both_planners() {
    let (source, files) = brushed();
    for kind in ["tree(auto)", "normal(auto)"] {
        let (_, free) = support_points(CAP, kind);
        let (ev, points) = support_points_with(&source, files.clone(), kind);
        // the same place on the plate: the painted part's frame reads both
        let (band, _) = sides(&ev, 0, &free);
        assert!(band > 10, "{kind}: without the block that band has a roof: {band}");
        let (band, far) = sides(&ev, 0, &points);
        assert_eq!(band, 0, "{kind}: a roof under the brushed block");
        assert!(far > 10, "{kind}: the far side keeps its roof: {far}");
    }
}

/// The overrides go with the model, as its colour paint does: moved, turned on the plate, a
/// linked clone and a copy, each part keeps its block on its own +x edge and its roof opposite.
#[test]
fn a_brushed_block_goes_with_the_part_moved_turned_cloned_and_copied() {
    let (source, files) = brushed();
    let place = |arg| Change::Place { instance: "part".into(), object: "part".into(), arg };
    let cases: [(&str, Vec<Change>, usize); 4] = [
        ("moved", vec![place(PlaceArg::At(Some([80.0, 70.0])))], 1),
        ("turned", vec![place(PlaceArg::At(Some([150.0, 150.0]))), place(PlaceArg::Rotate([0.0, 0.0, 90.0]))], 1),
        ("cloned", vec![place(PlaceArg::At(Some([100.0, 100.0]))), place(PlaceArg::Rotate([0.0, 0.0, 30.0])), Change::Clone { instance: "part".into(), at: Some([180.0, 200.0]) }], 2),
        ("copied", vec![place(PlaceArg::At(Some([100.0, 100.0]))), Change::Copy { instance: "part".into(), at: Some([200.0, 120.0]) }, Change::Place { instance: "part_2".into(), object: "part".into(), arg: PlaceArg::Rotate([0.0, 0.0, -90.0]) }], 2),
    ];
    for kind in ["tree(auto)", "normal(auto)"] {
        for (case, changes, parts) in &cases {
            let (ev, points) = supports_of(build_placed(&source, files.clone(), changes), kind);
            assert_eq!(ev.plate.parts.len(), *parts, "{kind} {case}");
            for i in 0..*parts {
                let (band, far) = sides(&ev, i, &points);
                assert_eq!(band, 0, "{kind} {case}: part {i} has a roof under its block");
                assert!(far > 10, "{kind} {case}: part {i} keeps its roof opposite: {far}");
            }
        }
    }
}
