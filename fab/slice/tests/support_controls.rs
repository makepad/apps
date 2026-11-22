//! The support panel's rows for trees are exactly what R14 is told: each one the panel shows
//! while Type is Tree changes what the job tells R14, and each one it hides there changes nothing
//! of it. And what the first layer gap promises holds.

use fab_plate::{support_reads, Part, PartMesh, Plate, PrintSetting, SettingKind, PRINT_SETTINGS, SUPPORT_TYPE};
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{plan_frames_outlines, plan_frames_supports, Frame};
use makepad_fab_slicer::geom::{dist_point_segment, ExPolygon, Pt, SCALE};
use makepad_fab_toolpath::par::Ctl;
use makepad_fab_toolpath::ObjectLayer;
use std::collections::HashSet;
use std::sync::Arc;

const TREE: &str = "tree(auto)";

fn setup(values: &[(&str, &str)]) -> JobSetup {
    let mut setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    for &(k, v) in [("enable_support", "1"), (SUPPORT_TYPE, TREE)].iter().chain(values) {
        setup.process.values.set_str(k, v);
    }
    setup
}

fn job(values: &[(&str, &str)]) -> makepad_fab_settings::machine::Job {
    makepad_fab_slice::prepare_job(&setup(values)).unwrap_or_else(|e| panic!("{e}"))
}

/// What the job tells R14, as text.
fn told(values: &[(&str, &str)]) -> String {
    format!("{:?}", makepad_fab_toolpath::tree_told(&job(values)).expect("the job asks for trees"))
}

/// A value of `s` other than `now`, within its range.
fn other(s: &PrintSetting, now: &str) -> String {
    match s.kind {
        SettingKind::Bool => (if matches!(now.trim(), "1" | "true") { "0" } else { "1" }).into(),
        SettingKind::Number { min, max, step, .. } => {
            let v = fab_plate::setting_number(now).unwrap_or(min);
            let up = v + 3.0 * step;
            format!("{}", if up <= max { up } else { v - 3.0 * step })
        }
        SettingKind::Choice(choices) => choices.iter().map(|(v, _)| *v).find(|v| *v != now).unwrap_or(now).into(),
    }
}

#[test]
fn every_tree_control_changes_what_r14_is_told() {
    let base = job(&[]);
    let told_base = told(&[]);
    let mut wrong = Vec::new();
    for s in PRINT_SETTINGS.iter().filter(|s| s.needs == SUPPORT_TYPE) {
        let now = base.cfg.str(s.key);
        let now = if now.is_empty() { s.default } else { now };
        let to = other(s, now);
        let changes = told(&[(s.key, &to)]) != told_base;
        if changes != support_reads(TREE, s.key) {
            wrong.push(format!("{} ({now} to {to}): {}", s.key, if changes { "changes R14's input, but the panel hides it for trees" } else { "shown for trees, but changes nothing R14 is told" }));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// One closed box.
fn cube(min: [f64; 3], size: [f64; 3], mesh: &mut PartMesh) {
    let b = mesh.positions.len() as u32;
    mesh.positions.extend((0..8u32).map(|i| [min[0] + size[0] * (i & 1) as f64, min[1] + size[1] * ((i >> 1) & 1) as f64, min[2] + size[2] * ((i >> 2) & 1) as f64]));
    let triangles = [[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
    mesh.triangles.extend(triangles.map(|t: [u32; 3]| t.map(|v| v + b)));
}

/// The hollow of the box under a lid (x, y, mm): 1.8 mm wide, room for a tree until the gap fills
/// it from both sides.
const POCKET: [f64; 4] = [144.0, 140.0, 145.8, 148.0];

/// The shelf's underside beside its block, 0.6 mm over the bed (x, y, mm): its roofs reach the bed.
const SHELF: [f64; 4] = [50.0, 100.0, 60.0, 110.0];

/// All a tree puts on the first layer, in one part: a cap on a stem, whose feet stand beside the
/// stem; a shelf just over the bed; and a closed box with the pocket in it.
fn gap_part() -> Plate {
    let mut mesh = PartMesh { positions: Vec::new(), triangles: Vec::new() };
    cube([96.0, 96.0, 0.0], [8.0, 8.0, 8.0], &mut mesh);
    cube([88.0, 88.0, 8.0], [24.0, 24.0, 3.0], &mut mesh);
    cube([60.0, 100.0, 0.0], [10.0, 10.0, 6.0], &mut mesh);
    cube([50.0, 100.0, 0.6], [11.0, 10.0, 5.4], &mut mesh);
    cube([141.0, 137.0, 0.0], [3.0, 14.0, 8.0], &mut mesh);
    cube([145.8, 137.0, 0.0], [3.0, 14.0, 8.0], &mut mesh);
    cube([143.0, 137.0, 0.0], [3.8, 3.0, 8.0], &mut mesh);
    cube([143.0, 148.0, 0.0], [3.8, 3.0, 8.0], &mut mesh);
    cube([141.0, 137.0, 8.0], [7.8, 14.0, 3.0], &mut mesh);
    let part = Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: "part".into(), name: "part".into(), filament: 1, color: [1.0; 4], nozzle: None, mesh, field_bounds: None, field_floor: None, ..Default::default() };
    Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part] }
}

/// Points along what `paths` print, at most 0.05 mm apart, each with its line's half width (mm).
fn printed<'a>(paths: impl Iterator<Item = &'a makepad_fab_toolpath::ExtrusionPath>) -> Vec<(Pt, f64)> {
    let mut out = Vec::new();
    for path in paths {
        for (i, s) in path.pts.windows(2).enumerate() {
            if !path.dry.get(i).copied().unwrap_or(false) {
                let steps = (s[0].dist(s[1]) / (0.05 * SCALE)).ceil().max(1.0) as i64;
                out.extend((0..=steps).map(|k| (Pt::new(s[0].x + (s[1].x - s[0].x) * k / steps, s[0].y + (s[1].y - s[0].y) * k / steps), path.width / 2.0)));
            }
        }
    }
    out
}

fn paths_of(layers: &[&ObjectLayer]) -> Vec<(Pt, f64)> {
    printed(layers.iter().flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)))
}

/// How far (mm) `p` is from `part`, none inside it.
fn from_part(p: Pt, part: &[ExPolygon]) -> f64 {
    if part.iter().any(|e| e.contains(p)) {
        return 0.0;
    }
    part.iter().flat_map(|e| e.rings()).flat_map(|r| (0..r.pts.len()).map(move |i| dist_point_segment(p, r.pts[i], r.pts[(i + 1) % r.pts.len()]))).fold(f64::MAX, f64::min) / SCALE
}

fn within(p: Pt, r: [f64; 4]) -> bool {
    let (x, y) = (p.x_mm(), p.y_mm());
    r[0] < x && x < r[2] && r[1] < y && y < r[3]
}

/// Nothing the trees print on the first layer stands nearer the part than the gap, the pad, the
/// roofs reaching the bed and all, and the gap is what stops them. What stands on the second layer
/// stands on what the first prints. A pocket the gap closes from both sides has a tree while there
/// is room and none after: nothing prints there, and its overhang is told as left unsupported.
#[test]
fn nothing_on_the_first_layer_stands_nearer_the_part_than_the_gap() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    // The lines and outlines are simplified, within this (mm).
    const TOLERANCE: f64 = 0.03;
    let plate = gap_part();
    let frames = [Frame { local: Arc::new(plate.parts[0].mesh.clone()), offset: [0.0; 3], field: None }];
    for (gap, roomy) in [("0.2", true), ("1", false)] {
        let mut setup = setup(&[("support_object_first_layer_gap", gap), ("fab_support_inside", "1"), ("support_remove_small_overhang", "0")]);
        setup.machine.values.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
        let supports = plan_frames_supports(Ctl::default(), &setup, &plate, &frames, &[], None).unwrap_or_else(|f| panic!("gap {gap}: {f}")).remove(0);
        let (outline, _, _) = plan_frames_outlines(Ctl::default(), &setup, &plate, &frames, &[]).unwrap_or_else(|f| panic!("gap {gap}: {f}")).remove(0);
        let layer = |l: usize| -> Vec<&ObjectLayer> { supports.additions.iter().filter(|a| a.0 == l).map(|a| &a.3).collect() };
        let first = paths_of(&layer(0));
        assert!(!first.is_empty(), "gap {gap}: the first layer prints");
        // The part as the first layer keeps off it: its own and the layer within the top gap.
        let part: Vec<ExPolygon> = outline[..2].concat();
        let want: f64 = gap.parse().unwrap();
        let nearest = first.iter().map(|&(p, half)| from_part(p, &part) - half).fold(f64::MAX, f64::min);
        eprintln!("gap {gap}: the first layer's lines come within {nearest:.3} mm of the part");
        assert!(nearest >= want - TOLERANCE, "gap {gap}: the first layer's lines come within {nearest:.3} mm of the part");
        assert!(nearest <= want + 0.1, "gap {gap}: the gap is what stops the pad, yet its lines stay {nearest:.3} mm off the part");
        assert!(first.iter().any(|&(p, _)| within(p, SHELF)), "gap {gap}: the roofs under the shelf reach the bed");
        // What the second layer's lines stand on, on a grid of half a millimetre.
        let cell = |p: Pt| (p.x_mm().div_euclid(0.5) as i64, p.y_mm().div_euclid(0.5) as i64);
        let under: HashSet<(i64, i64)> = first.iter().map(|&(p, _)| cell(p)).collect();
        for path in layer(1).iter().flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)) {
            let stands = printed(std::iter::once(path)).iter().any(|&(p, _)| {
                let (x, y) = cell(p);
                (-1..=1).any(|i| (-1..=1).any(|j| under.contains(&(x + i, y + j))))
            });
            assert!(stands, "gap {gap}: a line of the second layer at {:?} stands on nothing the first prints", path.pts.first().map(|p| [p.x_mm(), p.y_mm()]));
        }
        // The pocket.
        let in_pocket = (0..outline.len()).any(|l| paths_of(&layer(l)).iter().any(|&(p, _)| within(p, POCKET)));
        let sk = &supports.skeleton;
        let told: f64 = sk.given_up.iter().chain(&sk.inside).filter(|g| within(Pt::from_mm(g.at[0], g.at[1]), POCKET)).map(|g| g.area).sum();
        eprintln!("gap {gap}: tree in the pocket {in_pocket}, {told:.1} mm2 of its overhang told");
        assert_eq!(in_pocket, roomy, "gap {gap}: the tree in the pocket");
        assert_eq!(told > 1.0, !roomy, "gap {gap}: {told:.1} mm2 of the pocket's overhang told as unsupported");
    }
}
