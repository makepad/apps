//! A script's `m.settings` reaches what prints, through the model, the part's paint and the
//! slice: inside its volume the part prints with the region's infill density, pattern and wall
//! count, outside it with its own, and what the region does not set stays as it was.

use fab_model::eval::Evaluated;
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{slice_frames, Frame, PartField};
use makepad_fab_slice::SliceResult;
use makepad_fab_toolpath::role::Role;
use std::collections::BTreeMap;

/// A 20 mm cube standing on the bed, in model millimetres (y up).
const CUBE: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet cube = m.box({size: [20mm 20mm 20mm] at: [0 10mm 0]})\nobject(cube)\n";
/// The core: an 8 mm box in the cube's middle (model y 6..14 mm).
const CORE: &str = "m.box({size: [8mm 8mm 8mm] at: [0 10mm 0]})";
const HALF: f64 = 4.0;

fn build(source: &str) -> Evaluated {
    let m = FabModel::with_script_files("part", "part.cad.splash", source, BTreeMap::new(), BTreeMap::new());
    let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

/// 15% gyroid at 0.2 mm layers, two walls, of `generator`.
fn setup(generator: &str) -> JobSetup {
    let mut process = Preset::default();
    for (k, v) in [("layer_height", "0.2"), ("sparse_infill_density", "15%"), ("sparse_infill_pattern", "gyroid"), ("wall_loops", "2"), ("wall_generator", generator)] {
        process.values.set_str(k, v);
    }
    let mut machine = Preset::default();
    machine.values.set_vec("printable_area", Machine::dual_nozzle_350().bed.corners());
    // A filament with a density (g/cm3), so the estimates weigh what they count.
    let mut filament = Preset::default();
    filament.values.set_str("filament_density", "1.24");
    JobSetup { machine, process, filaments: vec![filament], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] }
}

fn slice(ev: &Evaluated) -> SliceResult {
    slice_with(ev, "classic")
}

fn slice_with(ev: &Evaluated, generator: &str) -> SliceResult {
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
    slice_frames(&setup(generator), &ev.plate, &frames, &[], None).expect("slices")
}

/// One printed path: its role, centre line (printer mm) and width.
#[derive(Clone, Debug, PartialEq)]
struct Path {
    role: Role,
    pts: Vec<(f64, f64)>,
    width: f64,
}

/// Each model layer's print z and paths, walls and fills.
fn layers(res: &SliceResult) -> Vec<(f64, Vec<Path>)> {
    res.layers()
        .iter()
        .map(|l| {
            let paths = l.objects.iter().filter(|o| !o.support_only).flat_map(|o| o.islands.iter()).flat_map(|i| i.walls.iter().chain(&i.fills)).map(|p| Path { role: p.role, pts: p.pts.iter().map(|q| (q.x_mm(), q.y_mm())).collect(), width: p.width }).collect();
            (l.print_z - 0.5 * l.height, paths)
        })
        .collect()
}

fn is_fill(r: Role) -> bool {
    matches!(r, Role::SparseInfill | Role::InternalSolidInfill | Role::InternalBridge | Role::Bridge | Role::TopSurface | Role::BottomSurface | Role::FloatingVerticalShell | Role::GapFill)
}

fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l = dx * dx + dy * dy;
    let t = if l > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l).clamp(0.0, 1.0) } else { 0.0 };
    (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
}

/// The distance from `p` to the nearest centre line of the paths `pick` takes.
fn nearest(paths: &[Path], p: (f64, f64), pick: impl Fn(&Path) -> bool) -> f64 {
    paths.iter().filter(|q| pick(q)).flat_map(|q| q.pts.windows(2).map(|w| segment_distance(p, w[0], w[1])).chain(q.pts.first().map(|&a| segment_distance(p, a, a)))).fold(f64::INFINITY, f64::min)
}

/// Whether a fill bead covers `p`.
fn covered(paths: &[Path], p: (f64, f64)) -> bool {
    paths.iter().filter(|q| is_fill(q.role)).any(|q| q.pts.windows(2).any(|w| segment_distance(p, w[0], w[1]) <= 0.5 * q.width))
}

/// The grid points (0.25 mm apart) of the square of half side `half` round `c`, at least
/// `inset` inside it (negative: outside, within `-inset` of it).
fn square_points(c: (f64, f64), half: f64, from: f64, to: f64) -> Vec<(f64, f64)> {
    let n = ((half + to.max(0.0)) / 0.25).ceil() as i64;
    let mut out = Vec::new();
    for i in -n..=n {
        for j in -n..=n {
            let p = (c.0 + i as f64 * 0.25, c.1 + j as f64 * 0.25);
            // signed distance into the square
            let d = half - (p.0 - c.0).abs().max((p.1 - c.1).abs());
            if d >= from && d <= to {
                out.push(p);
            }
        }
    }
    out
}

/// The part's model point in printer millimetres.
fn printer(ev: &Evaluated, p: [f64; 3]) -> [f64; 3] {
    let m = ev.paint[0].as_ref().map(|paint| paint.to_printer).expect("the script paints");
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// A solid core through the cube's middle: inside it every layer prints solid, with no gap where
/// the sparse infill meets it, and below, above and away from it the print is the same as with
/// no core; the walls and skins are the same on every layer.
#[test]
fn a_solid_core_prints_solid_inside_and_leaves_the_rest_as_it_was() {
    let (base_run, ev) = (slice(&build(CUBE)), build(&format!("{CUBE}m.settings({CORE}, {{sparse_infill_density: \"100%\"}})\n")));
    let core_run = slice(&ev);
    // the estimate prints the core: about its volume less the 15% it had, more filament
    let added = core_run.stats.filament_mm3[0] - base_run.stats.filament_mm3[0];
    let expected = 0.85 * (2.0 * HALF).powi(3);
    assert!((added - expected).abs() < 0.25 * expected, "the core adds {added:.0} mm3 of filament, not about {expected:.0}");
    assert!(core_run.stats.total_time > base_run.stats.total_time, "the core takes time to print");
    let (base, core) = (layers(&base_run), layers(&core_run));
    let c = printer(&ev, [0.0, 10.0, 0.0]);
    let (centre, lo, hi) = ((c[0], c[1]), c[2] - HALF, c[2] + HALF);
    assert_eq!(base.len(), core.len(), "the same layers");
    let mut solid_layers = 0;
    let mut bonds = 0;
    for ((z, a), (_, b)) in base.iter().zip(&core) {
        let walls_and_skins = |paths: &[Path]| paths.iter().filter(|p| matches!(p.role, Role::OuterWall | Role::InnerWall | Role::TopSurface | Role::BottomSurface)).cloned().collect::<Vec<_>>();
        assert_eq!(walls_and_skins(a), walls_and_skins(b), "z {z:.2}: the walls and skins are the part's");
        if *z < lo - 0.6 || *z > hi + 0.6 {
            assert_eq!(a, b, "z {z:.2}: a layer the core does not reach prints as before");
            continue;
        }
        if *z < lo + 0.3 || *z > hi - 0.3 {
            continue;
        }
        solid_layers += 1;
        // inside: solid, to its edge
        let inside = square_points(centre, HALF, 0.0, HALF);
        let filled = inside.iter().filter(|&&p| covered(b, p)).count();
        assert!(filled as f64 >= 0.97 * inside.len() as f64, "z {z:.2}: {filled} of {} points inside the core covered", inside.len());
        let rim = square_points(centre, HALF, 0.2, 0.3);
        let rim_filled = rim.iter().filter(|&&p| covered(b, p)).count();
        assert!(rim_filled as f64 >= 0.95 * rim.len() as f64, "z {z:.2}: the core's edge band: {rim_filled} of {} covered", rim.len());
        // how far a point is outside the core (negative: inside)
        let out = |q: (f64, f64)| (q.0 - centre.0).abs().max((q.1 - centre.1).abs()) - HALF;
        assert!(b.iter().filter(|p| p.role == Role::SparseInfill).all(|p| p.pts.iter().all(|&q| out(q) > -0.05)), "z {z:.2}: no sparse line inside the core");
        // the sparse lines that reach the core meet its beads as they meet a wall (along its
        // sides: a corner's beads round it off, as a wall's do)
        let corner = |q: (f64, f64)| (q.0 - centre.0).abs().min((q.1 - centre.1).abs()) > HALF - 0.5;
        for p in b.iter().filter(|p| p.role == Role::SparseInfill) {
            for &q in &p.pts {
                if (0.0..0.2).contains(&out(q)) && !corner(q) {
                    let d = nearest(b, q, |s| matches!(s.role, Role::InternalSolidInfill | Role::InternalBridge | Role::Bridge));
                    assert!(d <= 0.5, "z {z:.2}: a sparse line at ({:.2}, {:.2}) off the core's middle, {:.2} mm out, ends {d:.2} mm from the core's beads", q.0 - centre.0, q.1 - centre.1, out(q));
                    bonds += 1;
                }
            }
        }
        // away from it: as before
        let away = square_points(centre, HALF, -10.0, -2.0);
        let same = away.iter().filter(|&&p| covered(a, p) == covered(b, p)).count();
        assert!(same as f64 >= 0.98 * away.len() as f64, "z {z:.2}: {same} of {} points 2 mm or more off the core as before", away.len());
    }
    assert!(solid_layers >= 30, "{solid_layers} layers through the core");
    assert!(bonds > 0, "the sparse infill reaches the core");
}

/// A tower with an arm: the arm's a feature, standing out of the tower's side at mid height.
const TOWER: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet tower = m.box({size: [20mm 40mm 20mm] at: [0 20mm 0]})\nlet arm = m.box({size: [20mm 8mm 8mm] at: [18mm 20mm 0]})\nobject(m.union(tower, arm), name: \"figure\")\n";
/// The arm's reach in model Y (mm) and how far past the tower its volume starts, grown by 1 mm.
const ARM_Y: (f64, f64) = (16.0, 24.0);
const GROW: f64 = 1.0;

/// A volume that follows a feature: solid inside the feature grown by its Grow, and everything
/// that's further off is the print as it was, a layer it doesn't reach byte for byte.
#[test]
fn a_volume_that_follows_a_feature_changes_infill_only_inside_it() {
    let base_ev = build(TOWER);
    let ev = build(&format!("{TOWER}let arm_settings = m.offset(arm, {GROW}mm)\nm.settings(arm_settings, {{sparse_infill_density: \"100%\"}})\n"));
    let (base_run, run) = (slice(&base_ev), slice(&ev));
    let (a, b) = (layers(&base_run), layers(&run));
    assert_eq!(a.len(), b.len(), "the same layers");
    assert!(run.stats.filament_mm3[0] > base_run.stats.filament_mm3[0], "the arm prints solid");
    // the arm grown, in printer mm
    let lo = printer(&ev, [8.0 - GROW, ARM_Y.0 - GROW, -4.0 - GROW]);
    let hi = printer(&ev, [28.0 + GROW, ARM_Y.1 + GROW, 4.0 + GROW]);
    let (min, max): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| lo[k].min(hi[k])), std::array::from_fn(|k| lo[k].max(hi[k])));
    // how far a point is off it, across the plate: grown, its corners are round
    let off = |q: (f64, f64)| {
        let (dx, dy) = ((min[0] + GROW - q.0).max(q.0 - max[0] + GROW), (min[1] + GROW - q.1).max(q.1 - max[1] + GROW));
        dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - GROW
    };
    let (mut reached, mut changed_above) = (0, 0);
    for ((z, before), (_, after)) in a.iter().zip(&b) {
        if *z < min[2] - 0.6 || *z > max[2] + 0.6 {
            // the skins over and under a solid piece stay as they were
            assert_eq!(before, after, "z {z:.2}: a layer the volume doesn't reach prints as before");
            continue;
        }
        // grown past the arm, the volume holds air there: the arm's own layers are what's read
        if *z < min[2] + GROW + 0.6 || *z > max[2] - GROW - 0.6 {
            changed_above += 1;
            continue;
        }
        reached += 1;
        let outer = |paths: &[Path]| paths.iter().filter(|p| p.role == Role::OuterWall).cloned().collect::<Vec<_>>();
        assert_eq!(outer(before), outer(after), "z {z:.2}: the outer walls are the part's");
        // inside the arm, past the tower: solid
        let c = printer(&ev, [18.0, 20.0, 0.0]);
        assert!(covered(after, (c[0], c[1])) || covered(after, (c[0] + 0.3, c[1] + 0.3)), "z {z:.2}: the arm's middle prints solid");
        assert!(after.iter().filter(|p| p.role == Role::SparseInfill).all(|p| p.pts.iter().all(|&q| off(q) > -0.05)), "z {z:.2}: no sparse line inside the volume");
        // 2 mm or more off it, the same fill lines
        let away: Vec<(f64, f64)> = (0..80).flat_map(|i| (0..80).map(move |j| (min[0] - 40.0 + i as f64 * 0.5, min[1] - 20.0 + j as f64 * 0.5))).filter(|q| off(*q) >= 2.0).collect();
        let same = away.iter().filter(|&&p| covered(before, p) == covered(after, p)).count();
        assert!(same as f64 >= 0.98 * away.len() as f64, "z {z:.2}: {same} of {} points off the volume as before", away.len());
    }
    assert!(reached >= 30, "{reached} layers through the arm");
    assert!(changed_above <= 24, "{changed_above} layers at its edge");
}

/// The core's added material and time in the estimates match what its paths print.
#[test]
fn a_solid_core_adds_to_the_estimates_what_its_lines_print() {
    // Model extrusion volume (mm3) and time at planned speed (s), without dry moves.
    fn path_sums(res: &SliceResult) -> (f64, f64) {
        let (mut volume_mm3, mut time_s) = (0.0, 0.0);
        for p in res.layers().iter().flat_map(|l| &l.objects).filter(|o| !o.support_only).flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)) {
            for (i, w) in p.pts.windows(2).enumerate() {
                if p.dry.get(i).copied().unwrap_or(false) {
                    continue;
                }
                let dz_mm = p.z.get(i + 1).copied().unwrap_or(0.0) - p.z.get(i).copied().unwrap_or(0.0);
                let length_mm = (w[1].x_mm() - w[0].x_mm()).hypot(w[1].y_mm() - w[0].y_mm()).hypot(dz_mm);
                volume_mm3 += length_mm * p.area * p.flow.get(i).copied().unwrap_or(1.0);
                time_s += length_mm / p.speed;
            }
        }
        (volume_mm3, time_s)
    }
    let base = slice(&build(CUBE));
    let core = slice(&build(&format!("{CUBE}m.settings({CORE}, {{sparse_infill_density: \"100%\"}})\n")));
    let (base_mm3, base_s) = path_sums(&base);
    let (core_mm3, core_s) = path_sums(&core);
    let path_mm3 = core_mm3 - base_mm3;
    let path_s = core_s - base_s;
    let added_mm3 = core.stats.filament_mm3.iter().sum::<f64>() - base.stats.filament_mm3.iter().sum::<f64>();
    let added_g = core.stats.filament_g.iter().sum::<f64>() - base.stats.filament_g.iter().sum::<f64>();
    let added_s = core.stats.total_time - base.stats.total_time;
    // The filament's density is g/cm3; the paths and estimates use mm3.
    let density_g_per_mm3 = base.job.fil("filament_density", 0) / 1000.0;
    assert!(density_g_per_mm3 > 0.0, "the filament weighs {density_g_per_mm3} g per mm3, more than nothing");
    let expected_g = added_mm3 * density_g_per_mm3;
    println!("solid core adds: estimate {added_mm3:.6} mm3, paths {path_mm3:.6} mm3, estimate {added_g:.6} g, from density {expected_g:.6} g, estimate {added_s:.6} s, at speed {path_s:.6} s");
    assert!(added_mm3 > 0.0, "the core adds {added_mm3:.6} mm3 of estimated filament, more than zero");
    assert!((added_mm3 - path_mm3).abs() <= 0.03 * path_mm3.abs(), "the core adds {added_mm3:.6} mm3 in the estimate and {path_mm3:.6} mm3 in its paths, within 3%");
    assert!((added_g - expected_g).abs() <= 0.01 * expected_g.abs(), "the core adds {added_g:.6} g in the estimate and {expected_g:.6} g from its volume and filament density, within 1%");
    assert!(added_s > 0.0, "the core adds {added_s:.6} s of estimated print time, more than zero");
    assert!(added_s >= 0.95 * path_s, "the core adds {added_s:.6} s in the estimate, at least 0.95 times its paths' {path_s:.6} s at speed");
    assert!(added_s <= 2.0 * path_s, "the core adds {added_s:.6} s in the estimate, at most twice its paths' {path_s:.6} s at speed");
}

/// The share of the segment length of `segs` that runs within two degrees of its commonest
/// direction (mod 180 degrees, in whole degrees).
fn straight_share(segs: &[((f64, f64), (f64, f64))]) -> f64 {
    let mut by = [0.0f64; 180];
    let mut total = 0.0;
    for &(a, b) in segs {
        let l = (b.0 - a.0).hypot(b.1 - a.1);
        let deg = (b.1 - a.1).atan2(b.0 - a.0).to_degrees().rem_euclid(180.0);
        by[(deg.round() as usize) % 180] += l;
        total += l;
    }
    let best = (0..180).max_by(|&i, &j| by[i].total_cmp(&by[j])).unwrap_or(0);
    (0..5).map(|k| by[(best + 178 + k) % 180]).sum::<f64>() / total.max(1e-9)
}

/// A lines region inside the cube's gyroid: its sparse infill runs straight inside it, the
/// gyroid's curves round it, and the density is the part's.
#[test]
fn a_pattern_override_prints_its_pattern_inside_alone() {
    let base = layers(&slice(&build(CUBE)));
    let ev = build(&format!("{CUBE}m.settings({CORE}, {{sparse_infill_pattern: \"line\"}})\n"));
    let lines = layers(&slice(&ev));
    let c = printer(&ev, [0.0, 10.0, 0.0]);
    let (centre, lo, hi) = ((c[0], c[1]), c[2] - HALF, c[2] + HALF);
    let mut seen = 0;
    for ((z, a), (_, b)) in base.iter().zip(&lines) {
        if *z < lo + 0.3 || *z > hi - 0.3 {
            continue;
        }
        let out = |q: (f64, f64)| (q.0 - centre.0).abs().max((q.1 - centre.1).abs()) - HALF;
        // the sparse lines in pieces of at most 0.2 mm, those `keep` takes
        let segs = |paths: &[Path], keep: &dyn Fn(f64) -> bool| -> Vec<((f64, f64), (f64, f64))> {
            paths.iter().filter(|p| p.role == Role::SparseInfill).flat_map(|p| p.pts.windows(2).map(|w| (w[0], w[1]))).flat_map(|(a, b)| {
                let n = ((b.0 - a.0).hypot(b.1 - a.1) / 0.2).ceil().max(1.0) as usize;
                let at = move |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                (0..n).map(move |i| (at(i as f64 / n as f64), at((i + 1) as f64 / n as f64)))
            }).filter(|(p, q)| keep(out(*p)) && keep(out(*q))).collect()
        };
        let inside = segs(b, &|d| d < -0.8);
        let length = |s: &[((f64, f64), (f64, f64))]| s.iter().map(|(p, q)| (q.0 - p.0).hypot(q.1 - p.1)).sum::<f64>();
        assert!(length(&inside) > 10.0, "z {z:.2}: {:.1} mm of sparse line inside", length(&inside));
        assert!(straight_share(&inside) > 0.95, "z {z:.2}: the lines run straight inside ({:.2})", straight_share(&inside));
        let outside = segs(b, &|d| d > 0.8);
        assert!(straight_share(&outside) < 0.5, "z {z:.2}: the gyroid curves outside ({:.2})", straight_share(&outside));
        // the part's density inside: about the gyroid's line length per area, not solid
        let was = length(&segs(a, &|d| d < -0.8));
        assert!((length(&inside) / was - 1.0).abs() < 0.35, "z {z:.2}: {:.1} mm of lines against the gyroid's {was:.1} mm", length(&inside));
        seen += 1;
    }
    assert!(seen >= 30, "{seen} layers through the region");
}

/// Two overlapping volumes: where they overlap the later statement decides each key it sets,
/// and a key it leaves out stays the earlier one's.
#[test]
fn where_regions_overlap_the_later_one_wins_key_by_key() {
    let left = "m.box({size: [8mm 8mm 8mm] at: [-2mm 10mm 0]})";
    let right = "m.box({size: [8mm 8mm 8mm] at: [2mm 10mm 0]})";
    let solid = |r: &str| format!("m.settings({r}, {{sparse_infill_density: 100}})\n");
    let empty = |r: &str| format!("m.settings({r}, {{sparse_infill_density: \"0%\"}})\n");
    let walls = |r: &str| format!("m.settings({r}, {{wall_loops: 3 sparse_infill_pattern: \"line\"}})\n");
    // the statements; whether the overlap prints solid, and the right one's own part empty
    let cases = [(solid(left) + &empty(right), false, true), (empty(right) + &solid(left), true, true), (solid(left) + &walls(right), true, false)];
    for (statements, overlap_solid, right_empty) in cases {
        let ev = build(&format!("{CUBE}{statements}"));
        let res = layers(&slice(&ev));
        let at = |x: f64| { let p = printer(&ev, [x, 10.0, 0.0]); (p[0], p[1], p[2]) };
        let (overlap, only_left, only_right) = (at(0.0), at(-4.5), at(4.5));
        let (_, mid) = res.iter().min_by(|a, b| (a.0 - overlap.2).abs().total_cmp(&(b.0 - overlap.2).abs())).unwrap();
        let near = |p: (f64, f64, f64)| square_points((p.0, p.1), 1.0, 0.0, 1.0);
        let share = |p: (f64, f64, f64)| { let pts = near(p); pts.iter().filter(|&&q| covered(mid, q)).count() as f64 / pts.len() as f64 };
        assert!(share(only_left) > 0.97, "{statements}: the solid one's own part is solid ({:.2})", share(only_left));
        if right_empty {
            assert_eq!(share(only_right), 0.0, "{statements}: the empty one's own part is empty");
        } else {
            assert!(share(only_right) > 0.0 && share(only_right) < 0.5, "{statements}: the later one's own part is sparse ({:.2})", share(only_right));
        }
        if overlap_solid {
            assert!(share(overlap) > 0.97, "{statements}: the overlap is solid ({:.2})", share(overlap));
        } else {
            assert_eq!(share(overlap), 0.0, "{statements}: the later, empty, wins");
        }
    }
}

/// A sphere by script: on every layer through it the solid core is the sphere's circle at that
/// height, its beads inside the circle, the sparse infill outside it, no gap at its edge.
#[test]
fn a_sphere_region_cuts_its_circle_on_every_layer() {
    const R: f64 = 4.0;
    let ev = build(&format!("{CUBE}m.settings(m.sphere({{radius: {R}mm at: [0 10mm 0]}}), {{sparse_infill_density: 100}})\n"));
    let res = layers(&slice(&ev));
    let c = printer(&ev, [0.0, 10.0, 0.0]);
    let mut seen = 0;
    for (z, paths) in &res {
        let dz = z - c[2];
        if dz.abs() > R - 1.0 {
            continue;
        }
        let r = (R * R - dz * dz).sqrt();
        let from = |q: (f64, f64)| (q.0 - c[0]).hypot(q.1 - c[1]);
        for p in paths {
            match p.role {
                Role::InternalSolidInfill | Role::InternalBridge | Role::Bridge => {
                    let far = p.pts.iter().map(|&q| from(q)).fold(0.0, f64::max);
                    assert!(far <= r + 0.02, "z {z:.2}: a core bead reaches {far:.3} mm out, the circle {r:.3} mm");
                }
                Role::SparseInfill => {
                    let near = p.pts.iter().map(|&q| from(q)).fold(f64::INFINITY, f64::min);
                    assert!(near >= r - 0.02, "z {z:.2}: a sparse line comes {near:.3} mm near the axis, inside the circle {r:.3} mm");
                }
                _ => {}
            }
        }
        // solid all through, to a bead of its edge
        let disk: Vec<(f64, f64)> = square_points((c[0], c[1]), r, 0.0, r).into_iter().filter(|&q| from(q) <= r - 0.45).collect();
        let filled = disk.iter().filter(|&&q| covered(paths, q)).count();
        assert!(filled as f64 >= 0.98 * disk.len() as f64, "z {z:.2}: {filled} of {} points of the core covered", disk.len());
        let ring: Vec<(f64, f64)> = (0..72).map(|i| { let a = i as f64 * std::f64::consts::TAU / 72.0; (c[0] + (r - 0.25) * a.cos(), c[1] + (r - 0.25) * a.sin()) }).collect();
        let rim = ring.iter().filter(|&&q| covered(paths, q)).count();
        assert!(rim >= 65, "z {z:.2}: {rim} of 72 points round the core's edge covered");
        seen += 1;
    }
    assert!(seen >= 25, "{seen} layers through the sphere");
}

/// Where `p` is from the face a probe starts on (printer XY): along the face, and in from it.
fn face_frame(ev: &Evaluated, face: [f64; 3]) -> impl Fn((f64, f64)) -> (f64, f64) {
    let f = printer(ev, face);
    let along = printer(ev, [face[0], face[1], face[2] + 1.0]);
    let inward = printer(ev, [face[0] + 1.0, face[1], face[2]]);
    let (u, n) = ((along[0] - f[0], along[1] - f[1]), (inward[0] - f[0], inward[1] - f[1]));
    move |q: (f64, f64)| {
        let d = (q.0 - f[0], q.1 - f[1]);
        (d.0 * u.0 + d.1 * u.1, d.0 * n.0 + d.1 * n.1)
    }
}

/// The depths (in from the face) where wall centre lines cross the probe in from the face at `s`
/// along it, to `reach`, in order.
fn wall_depths(paths: &[Path], frame: &dyn Fn((f64, f64)) -> (f64, f64), s: f64, reach: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for p in paths.iter().filter(|p| matches!(p.role, Role::OuterWall | Role::InnerWall)) {
        for w in p.pts.windows(2) {
            let (a, b) = (frame(w[0]), frame(w[1]));
            if (a.0 - s) * (b.0 - s) > 0.0 || a.0 == b.0 {
                continue;
            }
            let t = (s - a.0) / (b.0 - a.0);
            let depth = a.1 + (b.1 - a.1) * t;
            if (0.0..reach).contains(&depth) {
                out.push(depth);
            }
        }
    }
    out.sort_by(f64::total_cmp);
    out
}

/// A box region across the cube's side at five wall loops against the part's two: on the layers
/// through it the walls there count five, two beside it, the extra loops reaching its edge and
/// stopping there, the infill inside the innermost loop on either side; the layers it does not
/// reach print as with no region, and so do the part's own two loops through it.
#[test]
fn a_regions_wall_count_steps_where_its_edge_crosses_the_walls() {
    // model x -14..-6 (the cube's side at -10), y 6..14, z -4..4
    let region = "m.box({size: [8mm 8mm 8mm] at: [-10mm 10mm 0]})";
    for generator in ["classic", "arachne"] {
        let base = layers(&slice_with(&build(CUBE), generator));
        let ev = build(&format!("{CUBE}m.settings({region}, {{wall_loops: 5}})\n"));
        let walled = layers(&slice_with(&ev, generator));
        let frame = face_frame(&ev, [-10.0, 10.0, 0.0]);
        let (lo, hi) = (printer(&ev, [0.0, 6.0, 0.0])[2], printer(&ev, [0.0, 14.0, 0.0])[2]);
        assert_eq!(base.len(), walled.len(), "{generator}: the same layers");
        let (mut seen, mut bonds) = (0, 0);
        for ((z, a), (_, b)) in base.iter().zip(&walled) {
            if *z < lo - 0.6 || *z > hi + 0.6 {
                assert_eq!(a, b, "{generator} z {z:.2}: a layer the region does not reach prints as before");
                continue;
            }
            if *z < lo + 0.3 || *z > hi - 0.3 {
                continue;
            }
            seen += 1;
            if generator == "classic" {
                let walls = |paths: &[Path]| paths.iter().filter(|p| matches!(p.role, Role::OuterWall | Role::InnerWall)).cloned().collect::<Vec<_>>();
                let mine = walls(b);
                assert!(walls(a).iter().all(|w| mine.contains(w)), "{generator} z {z:.2}: the part's own loops run round as before");
            }
            // five inside, two beside
            let inside = wall_depths(b, &frame, 0.0, 4.0);
            assert_eq!(inside.len(), 5, "{generator} z {z:.2}: walls at {inside:.2?} in the region");
            for s in [-7.0, 7.0] {
                let beside = wall_depths(b, &frame, s, 4.0);
                assert_eq!(beside.len(), 2, "{generator} z {z:.2}: walls at {beside:.2?} {s} mm along, beside the region");
            }
            // the extra loops reach the region's edge and stop there
            let wall_at = |q: (f64, f64)| b.iter().filter(|p| matches!(p.role, Role::OuterWall | Role::InnerWall)).any(|p| p.pts.windows(2).any(|w| { let (u, v) = (frame(w[0]), frame(w[1])); segment_distance(q, u, v) <= 0.5 * p.width }));
            for &depth in &inside[2..] {
                for s in [-3.9, 3.9] {
                    assert!(wall_at((s, depth)), "{generator} z {z:.2}: the loop {depth:.2} mm in reaches the region's edge at {s}");
                }
                for s in [-4.4, 4.4] {
                    assert!(!wall_at((s, depth)), "{generator} z {z:.2}: the loop {depth:.2} mm in runs on past the region's edge at {s}");
                }
            }
            // the infill inside the innermost loop on both sides, and meeting the loops' ends
            let innermost = |d: &[f64]| d.last().copied().unwrap_or(0.0);
            let beside = innermost(&wall_depths(b, &frame, 7.0, 4.0));
            for p in b.iter().filter(|p| p.role == Role::SparseInfill) {
                for &q in &p.pts {
                    let (s, depth) = frame(q);
                    if depth > 4.0 {
                        continue;
                    }
                    if s.abs() < 3.5 {
                        assert!(depth > innermost(&inside) + 0.3, "{generator} z {z:.2}: sparse infill {depth:.2} mm in, {s:.2} along, inside the region's innermost loop at {:.2}", innermost(&inside));
                    } else if s.abs() > 4.5 {
                        assert!(depth > beside + 0.3, "{generator} z {z:.2}: sparse infill {depth:.2} mm in, {s:.2} along, inside the innermost loop beside at {beside:.2}");
                    }
                    if (4.0..4.3).contains(&s.abs()) && depth < innermost(&inside) && depth > beside + 0.3 {
                        let d = nearest(b, q, |w| matches!(w.role, Role::OuterWall | Role::InnerWall));
                        assert!(d <= 0.6, "{generator} z {z:.2}: sparse infill at the step ({s:.2}, {depth:.2}) ends {d:.2} mm from the loops' ends");
                        bonds += 1;
                    }
                }
            }
        }
        assert!(seen >= 30, "{generator}: {seen} layers through the region");
        assert!(bonds > 0, "{generator}: the infill beside the region reaches its loops' ends");
    }
}

/// What `m.settings` takes, and what it says when it is given something else.
#[test]
fn the_settings_statement_says_what_it_takes() {
    let error = |statement: &str| {
        let m = FabModel::with_script_files("part", "part.cad.splash", &format!("{CUBE}{statement}\n"), BTreeMap::new(), BTreeMap::new());
        let mut e = fab_model::eval::Evaluator::new(Machine::dual_nozzle_350());
        m.evaluate(&mut e, None).err().map(|f| f.to_string())
    };
    for (statement, says) in [
        (format!("m.settings({CORE}, {{sparse_infill_density: 150}})"), "goes from 0 to 100"),
        (format!("m.settings({CORE}, {{sparse_infill_pattern: \"wobble\"}})"), "not \"wobble\""),
        (format!("m.settings({CORE}, {{sparse_infill_pattern: \"lightning\"}})"), "the whole part's pattern"),
        (format!("m.settings({CORE}, {{sparse_infill_pattern: \"lockedzag\"}})"), "the whole part's pattern"),
        (format!("m.settings({CORE}, {{}})"), "give it a setting"),
        (format!("m.settings({CORE}, {{wall_loops: 3 top_shell_layers: 2}})"), "a region sets sparse_infill_density, sparse_infill_pattern, wall_loops, not 'top_shell_layers'"),
        (format!("m.settings({CORE}, {{wall_loops: [3]}})"), "text, a number or true or false"),
        (format!("m.settings({CORE})"), "give it the settings"),
        ("m.settings(Flood{seed: [0 20mm 0]}, {wall_loops: 3})".to_string(), "hold in a volume"),
    ] {
        let e = error(&statement).unwrap_or_else(|| panic!("{statement}: no error"));
        assert!(e.contains(says), "{statement}: {e}");
    }
    for statement in [format!("m.settings({CORE}, {{sparse_infill_density: 40 sparse_infill_pattern: \"grid\"}})"), format!("m.settings({CORE}, {{wall_loops: 4}})"), format!("m.settings({CORE}, {{sparse_infill_density: \"0%\"}})")] {
        assert_eq!(error(&statement), None, "{statement}");
    }
}
