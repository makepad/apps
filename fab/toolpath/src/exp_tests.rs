//! Replanning and windowed planning with experimental modes: every emitted
//! model layer is final, and separate supports reconstruct the whole plan.

use crate::*;
use makepad_fab_slicer::sdf::SdfSource;

/// An 8 x 8 pillar (z 0..10, 34 layers) carrying a 20 x 20 cap from z 7; `notch` cuts a
/// radius-1.5 hole through the pillar between z 2 and 4 (a mid-height edit).
fn mushroom(notch: bool) -> impl Fn(f64, f64, f64) -> f64 + Send + Sync {
    move |x: f64, y: f64, z: f64| {
        let (x, y) = (x - 100.0, y - 100.0);
        let pillar = (x.abs() - 4.0).max(y.abs() - 4.0).max(-z).max(z - 10.0);
        let cap = (x.abs() - 10.0).max(y.abs() - 10.0).max(7.0 - z).max(z - 10.0);
        let solid = pillar.min(cap);
        if notch {
            let hole = (x * x + y * y).sqrt() - 1.5;
            let band = (2.0 - z).max(z - 4.0);
            solid.max(-hole.max(band))
        } else {
            solid
        }
    }
}
const BOUNDS: ([f64; 3], [f64; 3]) = ([90.0, 90.0, 0.0], [110.0, 110.0, 10.0]);

fn job(modes: &[(&str, &str)]) -> Job {
    let mut cfg = makepad_fab_settings::config::Config::new();
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        cfg.set_str(key, "zig-zag");
    }
    for (key, value) in [("nozzle_diameter", "0.4"), ("layer_height", "0.3"), ("initial_layer_print_height", "0.3"), ("wall_loops", "2"), ("top_shell_layers", "3"), ("bottom_shell_layers", "3"), ("sparse_infill_density", "15%")] {
        cfg.set_str(key, value);
    }
    for (k, v) in modes {
        cfg.set_str(k, *v);
    }
    cfg.set_vec("filament_map", vec!["1".into()]);
    // On the app's default printer's bed.
    cfg.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
    Job::new(cfg)
}

fn input(notch: bool) -> ObjectInput {
    ObjectInput { overrides: Vec::new(), name: "mushroom".into(), source: Box::new(SdfSource::new(Arc::new(mushroom(notch)), BOUNDS)), filament: 0 }
}

fn same(a: &ObjectPlan, b: &ObjectPlan) {
    assert_eq!(a.layers.len(), b.layers.len());
    for (la, lb) in a.layers.iter().zip(&b.layers) {
        assert_eq!(format!("{:?}", la), format!("{:?}", lb), "layer at z {}", la.0);
    }
}

/// A 6 x 6 pillar (z 0..5); `bulge` adds a double cone (radius 4.5 at z 2,
/// back to the pillar at z 0.5 and 3.5): sloped faces mid-height, so adaptive
/// layers there thin out and every layer above it moves.
fn pillar(bulge: bool) -> impl Fn(f64, f64, f64) -> f64 + Send + Sync {
    move |x: f64, y: f64, z: f64| {
        let (x, y) = (x - 100.0, y - 100.0);
        let p = (x.abs() - 3.0).max(y.abs() - 3.0).max(-z).max(z - 5.0);
        if bulge {
            let (s, c) = 45f64.to_radians().sin_cos();
            let cone = c * (x * x + y * y).sqrt() + s * (z - 2.0).abs() - c * 4.5;
            p.min(cone.max(0.5 - z).max(z - 3.5))
        } else {
            p
        }
    }
}

/// Adaptive layers: an edit that changes the slopes mid-height shifts the
/// heights of every layer above it; the assembled windows still equal a
/// fresh plan, and the heights did move.
#[test]
fn a_replan_with_shifted_adaptive_heights_equals_a_fresh_plan() {
    let job = job(&[("fab_adaptive_layers", "1"), ("layer_height", "0.2"), ("initial_layer_print_height", "0.2")]);
    let mk = |bulge: bool| ObjectInput { overrides: Vec::new(), name: "pillar".into(), source: Box::new(SdfSource::new(Arc::new(pillar(bulge)), ([94.0, 94.0, 0.0], [106.0, 106.0, 5.0]))), filament: 0 };
    let before: Vec<f64> = plan_heights(&mk(false), &job).iter().map(|h| h.0).collect();
    let fresh = check_windows_in(&mk(true), &job, Probe::Few);
    let after: Vec<f64> = fresh.layers.iter().map(|l| l.0).collect();
    assert!(before != after, "the bulge changes the adaptive layer heights");
    let above = |v: &[f64]| v.iter().copied().filter(|z| *z > 3.8).collect::<Vec<f64>>();
    assert!(above(&before) != above(&after), "the layers above the edit moved");
}

/// Supports are an object's own: only the mushroom that asks for them gets any.
#[test]
fn an_objects_supports_stay_with_it() {
    let job = job(&[("support_threshold_angle", "30"), ("support_type", "tree(auto)"), ("support_style", "tree_organic"), ("support_top_z_distance", "0.2"), ("support_bottom_z_distance", "0.2"), ("support_line_width", "0.4"), ("support_base_pattern", "rectilinear"), ("support_interface_pattern", "rectilinear")]);
    let with = |overrides: Vec<(String, String)>| ObjectInput { overrides, name: "mushroom".into(), source: Box::new(SdfSource::new(Arc::new(mushroom(false)), BOUNDS)), filament: 0 };
    let on = plan_one(par::Ctl::default(), 0, &with(vec![("enable_support".into(), "1".into())]), &job).unwrap();
    let off = plan_one(par::Ctl::default(), 0, &with(Vec::new()), &job).unwrap();
    let supports = |p: &ObjectPlan| p.layers.iter().flat_map(|l| l.2.islands.iter()).flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| matches!(p.role, Role::Support | Role::SupportInterface)).count();
    assert!(supports(&on) > 0, "the object asked for supports");
    assert_eq!(supports(&off), 0, "its neighbour did not");
}

/// A part's fills are its core and its inner walls their lining (`ExtrusionPath::core`, what the
/// preview leaves out while the part shows whole) only where its walls and skins close them in on
/// every side: one wall loop, or no skin above or below, leaves them shown; nothing else is ever
/// inside.
#[test]
fn only_fills_closed_in_are_core() {
    // (fills inside, fills shown, inner walls inside, inner walls shown, any other path inside)
    let count = |settings: &[(&str, &str)]| {
        let plan = plan_one(par::Ctl::default(), 0, &input(false), &job(settings)).unwrap();
        let mut n = [0usize; 5];
        for p in plan.layers.iter().flat_map(|l| l.2.islands.iter()).flat_map(|i| i.walls.iter().chain(&i.fills)) {
            let at = if p.role.fills_inside() { 0 } else if p.role == Role::InnerWall { 2 } else if p.core { 4 } else { continue };
            n[at + (at < 4 && !p.core) as usize] += 1;
        }
        n
    };
    let closed = count(&[]);
    assert!(closed[0] > 0 && closed[1] == 0 && closed[2] > 0 && closed[3] == 0 && closed[4] == 0, "two walls, skins above and below: every fill is core, every inner wall lining, nothing else: {closed:?}");
    let opens: [&[(&str, &str)]; 3] = [&[("wall_loops", "1")], &[("top_shell_layers", "0"), ("top_shell_thickness", "0")], &[("bottom_shell_layers", "0"), ("bottom_shell_thickness", "0")]];
    for open in opens {
        let n = count(open);
        assert!(n[0] == 0 && n[1] > 0 && n[2] == 0 && n[4] == 0, "{open:?}: nothing is inside: {n:?}");
    }
}

/// Two copies of one mesh with different settings are planned apart.
#[test]
fn copies_with_their_own_settings_are_not_one_plan() {
    let mesh = |x: f64| {
        let s = 10.0;
        let verts: Vec<[f64; 3]> = (0..8u32).map(|i| [x + s * (i & 1) as f64, 100.0 + s * ((i >> 1) & 1) as f64, s * ((i >> 2) & 1) as f64]).collect();
        let tris = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        makepad_fab_slicer::mesh::Mesh { verts, tris, paint: Vec::new() }
    };
    let object = |x: f64, overrides: Vec<(String, String)>| ObjectInput { overrides, name: "cube".into(), source: Box::new(makepad_fab_slicer::source::MeshSource::new(mesh(x))), filament: 0 };
    let same = instances(&[object(100.0, Vec::new()), object(150.0, Vec::new())]);
    assert_eq!(same[1].0, 0, "plain copies share a plan");
    let own = instances(&[object(100.0, Vec::new()), object(150.0, vec![("wall_loops".into(), "5".into())])]);
    assert_eq!(own[1].0, 1, "a copy with its own settings plans alone");
}

/// Compare all path fields and their order, including the loop, flow and Z
/// data; Debug prints floats losslessly and also covers each layer's slices.
fn same_layers(a: &[(f64, f64, ObjectLayer)], b: &[(f64, f64, ObjectLayer)]) {
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(b) {
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "layer at z {}", a.0);
    }
}

fn model_layers(plan: &ObjectPlan) -> Vec<(f64, f64, ObjectLayer)> {
    plan.layers.iter().filter(|l| !l.2.support_only).cloned().map(|(z, h, mut ol)| {
        ol.islands.retain(|island| {
            let paths: Vec<_> = island.walls.iter().chain(&island.fills).collect();
            paths.is_empty() || !paths.iter().all(|p| matches!(p.role, Role::Support | Role::SupportInterface | Role::SupportTransition))
        });
        (z, h, ol)
    }).collect()
}

fn next_random(seed: &mut u64) -> usize {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*seed >> 32) as usize
}

/// The windows tried besides the random partition.
#[derive(Clone, Copy)]
enum Probe {
    /// One middle layer (for a part whose every window costs its slope profile).
    Few,
    /// Singleton bottom, top and middle layers, empty windows, one running past the top.
    Generic,
    /// The aux block boundaries (cross-layer infill).
    CrossLayer,
    /// One aux block boundary.
    Boundary,
}

/// The probe windows; then a randomly ordered partition into windows of 1..64 layers
/// covering the part. Every returned layer must also be emitted once with the absolute
/// index. Returns the whole plan.
fn check_windows(o: &ObjectInput, job: &Job) -> ObjectPlan {
    check_windows_in(o, job, Probe::Generic)
}

fn check_windows_in(o: &ObjectInput, job: &Job, probe: Probe) -> ObjectPlan {
    let whole = plan_one(par::Ctl::default(), 7, o, job).unwrap();
    let model = model_layers(&whole);
    let own = job.with_overrides(&o.overrides);
    let p = Params::from_job(&own, o.filament);
    let heights = plan_heights(o, job);
    let n = heights.len();
    assert!(n > if matches!(probe, Probe::CrossLayer | Probe::Boundary) { 32 } else { 8 }, "{n} layers");
    let mut windows = match probe {
        Probe::Few => vec![(n / 2, n / 2 + 1)],
        Probe::Generic => vec![(0, 1), (n - 1, n), (n / 2, n / 2 + 1), (n, n + 1), (5, 5), (n - 2, n + 50)],
        Probe::CrossLayer => vec![(15, 16), (16, 17), (31, 33)],
        Probe::Boundary => vec![(16, 17)],
    };
    let mut seed = 0xd35a_3b9f_72a1_409c;
    let mut partition = Vec::new();
    let mut lo = 0;
    while lo < n {
        let hi = (lo + 1 + next_random(&mut seed) % 64).min(n);
        partition.push((lo, hi));
        lo = hi;
    }
    for i in (1..partition.len()).rev() {
        let j = next_random(&mut seed) % (i + 1);
        partition.swap(i, j);
    }
    let extras = windows.len();
    windows.extend(partition);
    let mut union = vec![None; n];
    for (k, (lo, hi)) in windows.into_iter().enumerate() {
        let emitted = std::sync::Mutex::new(Vec::new());
        let counts = std::sync::Mutex::new(Vec::new());
        let emit = |e: Emit| match e {
            Emit::Regions { count } => counts.lock().unwrap().push(count),
            Emit::Supports | Emit::Tree { .. } => {}
            Emit::Layer { region, index, z, h, layer } => emitted.lock().unwrap().push((region, index, z, h, layer.clone())),
        };
        let ctl = par::Ctl { emit: Some(&emit), focus: Some(lo), ..Default::default() };
        let got = plan_window(ctl, 7, o, job, &heights, lo, hi).unwrap();
        let wanted: Vec<_> = model.iter().filter(|l| heights[lo.min(n)..hi.min(n)].iter().any(|h| h.0 == l.0)).cloned().collect();
        same_layers(&got, &wanted);
        let mut emitted = emitted.into_inner().unwrap();
        emitted.sort_by_key(|e| (e.1, e.0));
        assert_eq!(emitted.len(), got.len());
        for (e, l) in emitted.iter().zip(&got) {
            assert!((lo..hi.min(n)).contains(&e.1));
            assert_eq!((e.2, e.3), heights[e.1]);
            same_layers(&[(e.2, e.3, e.4.clone())], std::slice::from_ref(l));
        }
        if lo < hi.min(n) {
            assert_eq!(counts.into_inner().unwrap(), vec![model.iter().filter(|l| l.0 == heights[0].0).count()]);
        }
        if k >= extras {
            for i in lo..hi {
                assert!(union[i].is_none(), "partition layer {i} repeated");
                union[i] = Some(got.iter().filter(|l| l.0 == heights[i].0).cloned().collect::<Vec<_>>());
            }
        }
    }
    let combined: Vec<_> = union.into_iter().flat_map(|l| l.expect("partition covers the part")).collect();
    same_layers(&combined, &model);
    // No emit callback is required to return the requested paths.
    same_layers(&plan_window(par::Ctl::default(), 7, o, job, &heights, 0, 1).unwrap(), &model.iter().filter(|l| l.0 == heights[0].0).cloned().collect::<Vec<_>>());
    let zs: Vec<_> = heights.iter().map(|(z, h)| z - h / 2.0).collect();
    let outline = o.source.contours(&zs);
    let supports = plan_supports(par::Ctl::default(), 7, o, job, &outline, &heights, &mut Default::default()).unwrap();
    if p.support.is_some() || p.normal_support.is_some() {
        assert!(!supports.additions.is_empty(), "the overhang needs support");
    }
    for (index, _, _, ol) in &supports.additions {
        assert_eq!(ol.filament, o.filament);
        assert_eq!(ol.support_only, *index == usize::MAX);
    }
    let combined = finish_plan(o, job, &heights, &outline, combined, supports);
    same(&combined, &whole);
    assert_eq!(combined.brim_keep_out, whole.brim_keep_out);
    whole
}

/// Windows, and a plan assembled from its windows and supports, equal the whole plan through
/// tops, bottoms and bridges, with the elephant-foot compensation, a brim, scarf seams and arc
/// overhangs on at once, and again with wall texture and ironing (a path has one mode, so the
/// texture needs a run without the scarf): each of those modes prints.
#[test]
fn windows_equal_whole_layers_through_tops_bottoms_bridges_and_every_experimental_mode() {
    let mode_paths = |plan: &ObjectPlan, mode: u8| plan.layers.iter().flat_map(|l| l.2.islands.iter()).flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| p.mode == mode).count();
    let whole = check_windows(&input(true), &job(&[("elefant_foot_compensation", "0.15"), ("brim_type", "outer_only"), ("brim_width", "2"), ("seam_slope_type", "external"), ("fab_arc_overhangs", "1")]));
    assert!(mode_paths(&whole, exp::mode::SCARF) > 0, "no scarf seam paths");
    assert!(mode_paths(&whole, exp::mode::ARC_FRONT) > 0, "no arc overhang paths");
    let whole = check_windows(&input(false), &job(&[("fab_wall_texture", "knurl"), ("ironing_type", "top")]));
    assert!(mode_paths(&whole, exp::mode::TEXTURE) > 0, "no textured wall paths");
    assert!(mode_paths(&whole, exp::mode::IRONING) > 0, "no ironing paths");
}

#[test]
fn windows_and_separate_tree_supports_equal_the_whole_plan() {
    let job = job(&[("enable_support", "1"), ("support_threshold_angle", "30"), ("support_type", "tree(auto)"), ("support_style", "tree_organic"), ("support_top_z_distance", "0.2"), ("support_bottom_z_distance", "0.2"), ("support_line_width", "0.4"), ("support_base_pattern", "rectilinear"), ("support_interface_pattern", "rectilinear")]);
    check_windows(&input(false), &job);
}

#[test]
fn windows_and_separate_normal_supports_equal_the_whole_plan() {
    let job = job(&[("enable_support", "1"), ("support_threshold_angle", "30"), ("support_type", "normal(auto)"), ("support_style", "snug"), ("support_top_z_distance", "0.2"), ("support_bottom_z_distance", "0.2"), ("support_line_width", "0.4"), ("support_base_pattern", "rectilinear"), ("support_interface_pattern", "rectilinear"), ("independent_support_layer_height", "1"), ("raft_first_layer_density", "90%")]);
    check_windows(&input(false), &job);
}

/// Cross-layer infill blocks (lightning, adaptive and support cubic on the mushroom, gyroid on
/// the quarter-scale bracket with its external bridges, shells and sloping walls) are the same
/// in windows as in the whole plan.
#[test]
fn cross_layer_blocks_equal_whole_layers() {
    for pattern in ["lightning", "adaptivecubic", "supportcubic"] {
        check_windows_in(&input(true), &job(&[("sparse_infill_pattern", pattern)]), Probe::CrossLayer);
    }
    let source = SdfSource::new(Arc::new(|x, y, z| bench_bracket::bracket_field([4.0 * x, 4.0 * y, 4.0 * z]) / 4.0), ([-11.5, -7.75, 0.0], [11.5, 7.75, 12.25]));
    let o = ObjectInput { overrides: Vec::new(), name: "bench bracket".into(), source: Box::new(source), filament: 0 };
    check_windows_in(&o, &job(&[("sparse_infill_pattern", "gyroid")]), Probe::Boundary);
}

/// Rotating islands and an empty band exercise moving seam targets and
/// the centre fallback. The source bounds deliberately exceed every slice.
struct TurningPart {
    painted: bool,
    changed_bottom: bool,
    cuts: std::sync::Mutex<Vec<f64>>,
}

impl ContourSource for TurningPart {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) { ([-40.0, -40.0, 0.0], [40.0, 40.0, 16.0]) }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.cuts.lock().unwrap().extend(zs);
        zs.iter().map(|&z| {
            if (6.0..7.0).contains(&z) { return Vec::new(); }
            let angle = z * 0.29 + if self.changed_bottom && z < 2.0 { 1.1 } else { 0.0 };
            let (sin, cos) = angle.sin_cos();
            [-13.0, 13.0].into_iter().map(|x| {
                let pts = [(-6.0, -5.0), (6.0, -5.0), (5.0, 6.0), (-4.0, 5.0)].map(|(u, v)| Pt::from_mm(x + u * cos - v * sin, u * sin + v * cos));
                ExPolygon::new(Polygon::new(pts.to_vec()))
            }).collect()
        }).collect()
    }
    fn painted(&self) -> bool { self.painted }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], _p: &makepad_fab_slicer::paint::PaintParams) -> Vec<makepad_fab_slicer::paint::LayerPaint> {
        zs.iter().zip(slices).map(|(z, s)| {
            if *z < 10.0 || s.is_empty() { vec![(0, s.clone())] }
            else { vec![(0, s[..1].to_vec()), (2, s[1..].to_vec())] }
        }).collect()
    }
}

fn turning(painted: bool, changed_bottom: bool) -> ObjectInput {
    ObjectInput { overrides: Vec::new(), name: "turning islands".into(), filament: 0, source: Box::new(TurningPart { painted, changed_bottom, cuts: Default::default() }) }
}

#[test]
fn painted_windows_keep_every_region_even_where_it_is_empty() {
    let mut job = job(&[("sparse_infill_pattern", "lightning"), ("seam_slope_type", "external"), ("enable_support", "1"), ("support_type", "normal(auto)"), ("support_style", "snug"), ("support_threshold_angle", "30"), ("support_line_width", "0.4"), ("support_base_pattern", "rectilinear"), ("support_interface_pattern", "rectilinear"), ("support_top_z_distance", "0.2")]);
    job.cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
    let job = Job::new(job.cfg);
    check_windows(&turning(true, false), &job);
}

/// A band planned on its own goes into a whole plan as finished layers
/// ([`stages::PartPlan::put_finished`]), a region-painted part's before its streams are known:
/// the plate plans and emits only the other layers, and the part equals the whole plan. Its
/// supports are there from their [`Emit::Supports`] on.
#[test]
fn a_band_put_as_finished_layers_is_not_planned_again() {
    use stages::{PartPlan, Plate, PlateCtl, PlatePart, PlateRun, Stage, Task};
    let mut job = job(&[("enable_support", "1"), ("support_type", "normal(auto)"), ("support_threshold_angle", "30")]);
    job.cfg.set_vec("filament_map", vec!["1".into(), "1".into()]);
    let job = Job::new(job.cfg);
    for o in [input(false), turning(true, false)] {
        let whole = plan_one(par::Ctl::default(), 0, &o, &job).unwrap();
        let heights = plan_heights(&o, &job);
        let n = heights.len();
        let (lo, hi) = (n * 5 / 8, n * 5 / 8 + 4);
        let band = plan_window(par::Ctl::default(), 0, &o, &job, &heights, lo, hi).unwrap();
        let regions = band.len() / (hi - lo);
        let plan = PartPlan::new(0, &o, &job, heights.clone());
        // Height-ordered, in stream order at each height.
        for (j, (_, _, layer)) in band.into_iter().enumerate() {
            plan.put_finished(j % regions, lo + j / regions, vec![layer]);
        }
        let p = Params::from_job(&job, o.filament);
        let slices = o.source.contours(&heights.iter().map(|&(z, h)| cut_z(&p, z, h)).collect::<Vec<_>>());
        let outline = plan_outline(par::Ctl::default(), &o, &job, &heights, slices).unwrap();
        let supports = plan_supports(par::Ctl::default(), 0, &o, &job, &outline, &heights, &mut Default::default()).unwrap();
        assert!(!supports.additions.is_empty(), "{}: the overhang needs support", o.name);
        let emitted = std::sync::Mutex::new(Vec::new());
        let seen = std::sync::Mutex::new(Vec::new());
        let emit = |_: usize, e: Emit| match e {
            Emit::Layer { index, .. } => emitted.lock().unwrap().push(index),
            Emit::Supports => seen.lock().unwrap().push(format!("{:?}", plan.supports().additions)),
            Emit::Regions { .. } | Emit::Tree { .. } => {}
        };
        let plate = Plate::new(vec![PlatePart { plan: &plan, targets: vec![Task::part(Stage::Finish)], urgency: 0, focus: None }]);
        assert_eq!(plate.run(PlateCtl { emit: Some(&emit), ..Default::default() }), PlateRun::Finished);
        drop(plate);
        let emitted = emitted.into_inner().unwrap();
        assert!(emitted.iter().all(|k| !(lo..hi).contains(k)), "{}: a band layer planned again", o.name);
        assert_eq!(emitted.len(), (n - (hi - lo)) * regions, "{}: every other layer emitted once per region", o.name);
        assert_eq!(seen.into_inner().unwrap(), vec![format!("{:?}", supports.additions)], "{}: the supports at their event", o.name);
        same(&plan.take_finished(), &whole);
    }
}

#[test]
fn seam_chains_forget_layers_below_their_window() {
    let job = job(&[("top_shell_layers", "0"), ("bottom_shell_layers", "0"), ("sparse_infill_density", "0%")]);
    let a = plan_one(par::Ctl::default(), 0, &turning(false, false), &job).unwrap();
    let b = plan_one(par::Ctl::default(), 0, &turning(false, true), &job).unwrap();
    assert_ne!(format!("{:?}", a.layers[0]), format!("{:?}", b.layers[0]));
    same_layers(&a.layers[20..], &b.layers[20..]);
    check_windows(&turning(false, false), &job);
}

#[test]
fn vase_windows_return_and_emit_the_whole_finished_ramp() {
    let job = job(&[("spiral_mode", "1")]);
    let o = input(false);
    let whole = plan_one(par::Ctl::default(), 0, &o, &job).unwrap();
    let emitted = std::sync::Mutex::new(Vec::new());
    let emit = |e: Emit| {
        if let Emit::Layer { index, z, h, layer, .. } = e { emitted.lock().unwrap().push((index, z, h, layer.clone())); }
    };
    let got = plan_window(par::Ctl { emit: Some(&emit), ..Default::default() }, 0, &o, &job, &plan_heights(&o, &job), 17, 18).unwrap();
    same_layers(&got, &whole.layers);
    assert!(got.iter().flat_map(|l| &l.2.islands).flat_map(|i| &i.walls).any(|p| p.mode == exp::mode::VASE));
    let mut emitted = emitted.into_inner().unwrap();
    emitted.sort_by_key(|e| e.0);
    same_layers(&emitted.into_iter().map(|(_, z, h, ol)| (z, h, ol)).collect::<Vec<_>>(), &whole.layers);
}

#[test]
fn a_window_only_cuts_its_required_contour_context() {
    let job = job(&[("sparse_infill_pattern", "lightning")]);
    let source = TurningPart { painted: false, changed_bottom: false, cuts: Default::default() };
    // Keep the recorder accessible through an Arc-backed contour source.
    struct Recorded(Arc<TurningPart>);
    impl ContourSource for Recorded {
        fn bounds(&self) -> ([f64; 3], [f64; 3]) { self.0.bounds() }
        fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> { self.0.contours(zs) }
    }
    let source = Arc::new(source);
    let o = ObjectInput { overrides: Vec::new(), name: "recorded".into(), filament: 0, source: Box::new(Recorded(source.clone())) };
    let p = Params::from_job(&job, 0);
    let heights = layers::object_heights(&*o.source, &p);
    let (a, b) = plan_context(&p, 30, 31, heights.len());
    plan_window(par::Ctl::default(), 0, &o, &job, &heights, 30, 31).unwrap();
    let mut cuts = source.cuts.lock().unwrap().clone();
    cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // Each layer the window's tasks read is cut once, and only within its context.
    let context: Vec<f64> = heights[a..b].iter().map(|(z, h)| z - h / 2.0).collect();
    assert!(cuts.windows(2).all(|w| w[0] < w[1]), "a layer cut twice: {cuts:?}");
    assert!(cuts.iter().all(|z| context.contains(z)), "a cut outside {context:?}: {cuts:?}");
    assert!(cuts.contains(&(heights[30].0 - heights[30].1 / 2.0)));
    assert!(a > 0 && b < heights.len());
}

/// The slice benchmark's bracket field at quarter scale: its shelf and
/// horizontal bores exercise external bridges, shells and sloping walls.
/// Only the field from slice/src/bench.rs is used, without its archive writer.
mod bench_bracket {
    fn sd_box(p: [f64; 3], c: [f64; 3], half: [f64; 3]) -> f64 {
        let q = [0, 1, 2].map(|a| (p[a] - c[a]).abs() - half[a]);
        let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2) + q[2].max(0.0).powi(2)).sqrt();
        out + q[0].max(q[1]).max(q[2]).min(0.0)
    }

    fn sd_round_box(p: [f64; 3], c: [f64; 3], half: [f64; 3], r: f64) -> f64 {
        sd_box(p, c, [half[0] - r, half[1] - r, half[2] - r]) - r
    }

    /// Capped cylinder along `axis` (0 = x, 1 = y, 2 = z).
    fn sd_cyl(p: [f64; 3], c: [f64; 3], axis: usize, r: f64, half_len: f64) -> f64 {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let d_r = ((p[u] - c[u]).powi(2) + (p[v] - c[v]).powi(2)).sqrt() - r;
        let d_a = (p[axis] - c[axis]).abs() - half_len;
        let out = (d_r.max(0.0).powi(2) + d_a.max(0.0).powi(2)).sqrt();
        out + d_r.max(d_a).min(0.0)
    }

    /// Smooth union (fillet radius ~k).
    fn smin(a: f64, b: f64, k: f64) -> f64 {
        let h = (0.5 + 0.5 * (b - a) / k).clamp(0.0, 1.0);
        b + (a - b) * h - k * h * (1.0 - h)
    }

    /// Smooth subtraction of b from a (rounded hole edges).
    fn ssub(a: f64, b: f64, k: f64) -> f64 {
        -smin(-a, b, k)
    }

    pub fn bracket_field(p: [f64; 3]) -> f64 {
        let f = |p: [f64; 3]| {
            let base = sd_round_box(p, [0.0, 0.0, 5.0], [45.0, 30.0, 5.0], 3.0);
            let upright = sd_round_box(p, [0.0, 23.0, 25.0], [40.0, 5.0, 25.0], 2.0);
            let shelf = sd_round_box(p, [0.0, -2.0, 45.0], [32.0, 23.0, 3.5], 2.0);
            let boss = sd_cyl(p, [22.0, -12.0, 12.0], 2, 10.0, 12.0);
            let mut d = smin(base, upright, 5.0);
            d = smin(d, shelf, 4.0);
            d = smin(d, boss, 4.0);
            // Vertical holes through the base, rounded edges.
            for (x, y) in [(-38.0, -22.0), (-38.0, 0.0), (-38.0, 14.0), (38.0, -22.0), (38.0, 0.0), (38.0, 14.0), (-12.0, -22.0), (0.0, -10.0)] {
                d = ssub(d, sd_cyl(p, [x, y, 5.0], 2, 3.0, 8.0), 1.0);
            }
            // Bore through the boss.
            d = ssub(d, sd_cyl(p, [22.0, -12.0, 12.0], 2, 4.0, 14.0), 1.0);
            // Horizontal holes through the upright (overhanging hole roofs).
            for x in [-24.0, 0.0, 24.0] {
                d = ssub(d, sd_cyl(p, [x, 23.0, 26.0], 1, 6.0, 8.0), 1.0);
            }
            // A slot through the shelf.
            d = ssub(d, sd_round_box(p, [-14.0, -8.0, 45.0], [10.0, 4.0, 6.0], 1.0), 1.0);
            d
        };
        f(p)
    }
}

/// A 9 mm bud vase: a 0.8 mm wall around a belly of revolution (radius 2.5 to 4.5 and back),
/// a 0.9 mm floor, open at the top. Every layer is one ring island on a slope.
fn vase(x: f64, y: f64, z: f64) -> f64 {
    let r = ((x - 100.0).powi(2) + (y - 100.0).powi(2)).sqrt();
    let belly = 2.5 + 2.0 * (std::f64::consts::PI * z / 9.0).sin();
    let outer = (r - belly).max(-z).max(z - 9.0);
    let cavity = (r - (belly - 0.8)).max(0.9 - z);
    outer.max(-cavity)
}

/// A 6 mm cube of gyroid sheet (period 3 mm, 0.8 mm thick): many islands per layer.
fn gyroid_sheet(x: f64, y: f64, z: f64) -> f64 {
    let k = 2.0 * std::f64::consts::PI / 3.0;
    let (a, b, c) = (k * (x - 100.0), k * (y - 100.0), k * z);
    let g = a.sin() * b.cos() + b.sin() * c.cos() + c.sin() * a.cos();
    let sheet = g.abs() / (1.5 * k) - 0.4;
    let cube = ((x - 100.0).abs() - 3.0).max((y - 100.0).abs() - 3.0).max(-z).max(z - 6.0);
    sheet.max(cube)
}

/// A viewed layer planned on its own (its window computing only what that layer reads) equals
/// the whole plan's layer: a vase and a gyroid sheet, with four top shells.
#[test]
fn a_viewed_layer_alone_equals_the_whole_plan() {
    let job = job(&[("top_shell_layers", "4"), ("sparse_infill_pattern", "gyroid")]);
    let vase = ObjectInput { overrides: Vec::new(), name: "vase".into(), source: Box::new(SdfSource::new(Arc::new(vase), ([95.0, 95.0, 0.0], [105.0, 105.0, 9.0]))), filament: 0 };
    let gyroid = ObjectInput { overrides: Vec::new(), name: "gyroid".into(), source: Box::new(SdfSource::new(Arc::new(gyroid_sheet), ([97.0, 97.0, 0.0], [103.0, 103.0, 6.0]))), filament: 0 };
    for o in [vase, gyroid] {
        check_windows(&o, &job);
    }
}
