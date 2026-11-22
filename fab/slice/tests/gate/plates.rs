//! The gate's model plates and cases (GPU-EXEC §10.6), shared by the CPU gate here and the GPU
//! gate in the app's tests (`#[macro_use] #[path]`): the app's own resources, evaluated by the
//! model as the app evaluates them, their parts as the slicer takes them, and the cases, one
//! `#[test]` each ([`field_cases`]): the smallest part or the fewest layers that prove each
//! class of layer (first, mid, bridge, top skin, strut top, sliver, wall only).
#![allow(dead_code)]
use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::makepad_csg_sdf::{SdfNode, SdfOp, SdfShape};
use makepad_fab_slice::gate::{self, GatePart, Kind};
use makepad_fab_slice::live::{Frame, PartField};
use makepad_fab_slice::stage_cases::{Prop, Stage, StageCase};
use std::collections::BTreeMap;
use std::sync::OnceLock;

pub const PROJECT: &str = include_str!("../../../app/resources/demo.fab.splash");
pub const SCRIPTS: [(&str, &str); 3] = [
    ("bracket.cad.splash", include_str!("../../../app/resources/bracket.cad.splash")),
    ("vase.cad.splash", include_str!("../../../app/resources/vase.cad.splash")),
    ("lattice_cube.cad.splash", include_str!("../../../app/resources/lattice_cube.cad.splash")),
];

/// The lattice cube's size and period on the gate's plates (mm): at the default 24 mm its
/// Splash kernel field costs a third of a second a layer on the CPU.
pub const LATTICE: (f64, f64) = (10.0, 5.0);
/// The wavy cube (live_slice's): the lattice cube at its smallest period.
pub const WAVY: (f64, f64) = (6.0, 3.0);

/// A project evaluated as the app evaluates one.
pub fn evaluate(project: &str, scripts: &[(&str, &str)]) -> Evaluated {
    let scripts: BTreeMap<String, String> = scripts.iter().map(|(f, s)| (f.to_string(), s.to_string())).collect();
    let m = FabModel::load("gate", project, scripts).unwrap_or_else(|f| panic!("the project loads: {f}"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("the plate builds: {f}")).1
}

fn lattice_script(size: f64, period: f64) -> String {
    format!("script(\"lattice_cube.cad.splash\", params: {{lattice_size: {size}mm lattice_period: {period}mm}})")
}

/// The plate Fab opens with: the shelf bracket (a mesh part), the bud vase and the lattice cube
/// (both fields), the lattice at [`LATTICE`].
pub fn demo() -> Evaluated {
    let project = PROJECT.replace("script(\"lattice_cube.cad.splash\")", &lattice_script(LATTICE.0, LATTICE.1));
    assert_ne!(project, PROJECT, "the demo project scripts the lattice cube");
    evaluate(&project, &SCRIPTS)
}

/// One of the demo's objects alone on the plate (`object`: the script's object, when it has several).
pub fn alone(script: &str, object: Option<&str>, params: &str) -> Evaluated {
    let file = format!("{script}.cad.splash");
    let source = SCRIPTS.iter().find(|(f, _)| *f == file).unwrap_or_else(|| panic!("no script {file}")).1;
    let params = if params.is_empty() { String::new() } else { format!(", params: {{{params}}}") };
    let object = object.map_or(String::new(), |o| format!(", object: \"{o}\""));
    let project = format!("use mod.fab.*\nscript(\"{file}\"{params})\nplace(\"{script}\"{object}, at: [150mm 165mm], name: \"{script}\")\n");
    evaluate(&project, &[(file.as_str(), source)])
}

pub fn bracket() -> Evaluated {
    alone("bracket", None, "")
}

pub fn vase() -> Evaluated {
    alone("vase", None, "")
}

pub fn lattice() -> Evaluated {
    alone("lattice_cube", Some("lattice"), &format!("lattice_size: {}mm lattice_period: {}mm", LATTICE.0, LATTICE.1))
}

pub fn wavy() -> Evaluated {
    alone("lattice_cube", Some("lattice"), &format!("lattice_size: {}mm lattice_period: {}mm", WAVY.0, WAVY.1))
}

/// The slicer's frames of an evaluated plate, as the app makes them (`live_slice::input_for`).
pub fn frames(ev: &Evaluated) -> Vec<Frame> {
    ev.frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
            Frame { local: f.local.clone(), offset: f.offset, field }
        })
        .collect()
}

/// A job setup: 0.4 mm nozzles, 0.2 mm layers, three walls of `generator` (classic or
/// arachne), zig-zag fills, `filaments` filaments mapped to the two nozzles in turn.
pub fn setup(filaments: usize, generator: &str) -> JobSetup {
    let mut machine = Preset::default();
    machine.values.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
    let mut process = Preset::default();
    for (key, value) in [("layer_height", "0.2"), ("initial_layer_print_height", "0.2"), ("wall_loops", "3"), ("wall_generator", generator)] {
        process.values.set_str(key, value);
    }
    for key in ["sparse_infill_pattern", "top_surface_pattern", "bottom_surface_pattern", "internal_solid_infill_pattern"] {
        process.values.set_str(key, "zig-zag");
    }
    JobSetup {
        machine,
        process,
        filaments: vec![Preset::default(); filaments],
        filament_map: (0..filaments).map(|i| (i % 2 + 1) as u32).collect(),
        nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None; filaments],
    }
}

/// An evaluated plate's parts for the gate under `generator`.
pub fn parts(ev: &Evaluated, generator: &str) -> Vec<GatePart> {
    let filaments = ev.plate.parts.iter().map(|p| p.filament as usize).max().unwrap_or(1).max(1);
    gate::frame_parts(&setup(filaments, generator), &ev.plate.printable(), &frames(ev)).unwrap_or_else(|e| panic!("the plate's parts: {e}"))
}

/// The demo plate's representative layers, per part (layers of 0.2 mm), each of the [`Kind`] it
/// names on the CPU: the gate fails `case` when the plate drifts away from it.
pub const BRACKET_LAYERS: &[(usize, Kind)] = &[(0, Kind::First), (100, Kind::Mid), (152, Kind::Bridge), (154, Kind::TopSkin)];
pub const VASE_LAYERS: &[(usize, Kind)] = &[(99, Kind::WallOnly), (132, Kind::Sliver)];
pub const LATTICE_LAYERS: &[(usize, Kind)] = &[(7, Kind::StrutTop), (24, Kind::Mid)];
/// The single plates' (arachne): the wavy cube's sliver (an island too thin for a wall),
/// mid and bridge layers, the box's first, bridge and top skin, the thin tube's first and a
/// layer of walls only.
pub const WAVY_LAYERS: &[(usize, Kind)] = &[(11, Kind::Sliver), (15, Kind::Mid), (23, Kind::Bridge)];
pub const BOX_LAYERS: &[(usize, Kind)] = &[(0, Kind::First), (36, Kind::Bridge), (39, Kind::TopSkin)];
pub const TUBE_LAYERS: &[(usize, Kind)] = &[(0, Kind::First), (10, Kind::WallOnly)];

/// One gate case: a part, and the layers it is held at (`None`: [`gate::sample`]d over its height).
pub struct Case {
    pub part: &'static GatePart,
    pub at: Option<&'static [(usize, Kind)]>,
}

/// A part for the life of the test process: a case's part is built once and read by its runner.
fn keep(part: GatePart) -> &'static GatePart {
    Box::leak(Box::new(part))
}

fn job(generator: &str) -> makepad_fab_settings::machine::Job {
    makepad_fab_slice::prepare_job(&setup(1, generator)).expect("the gate's job")
}

/// An analytic shape ([`gate::analytic`]) under `generator`, at every layer.
pub fn analytic_case(shape: &str, generator: &str) -> Case {
    let (name, shape) = gate::analytic().into_iter().find(|(n, _)| *n == shape).unwrap_or_else(|| panic!("no analytic shape {shape}"));
    let part = gate::analytic_part(&format!("{name} ({generator})"), shape, gate::ANALYTIC_HEIGHT, &job(generator)).expect("a field part");
    Case { part: keep(part), at: None }
}

/// A part of the demo plate (`name`: the plate's part name) under `generator`, at its
/// representative layers. The plate is evaluated once per process.
pub fn demo_case(name: &str, generator: &str) -> Case {
    static DEMO: OnceLock<Vec<(String, GatePart)>> = OnceLock::new();
    let parts = DEMO.get_or_init(|| {
        let ev = demo();
        ["classic", "arachne"].into_iter().flat_map(|g| parts(&ev, g).into_iter().map(move |mut p| { p.name = format!("{} ({g})", p.name); (g.to_string(), p) })).collect()
    });
    let part = parts.iter().find(|(g, p)| g == generator && p.name.starts_with(name)).map(|(_, p)| p).unwrap_or_else(|| panic!("the demo plate has no part {name}"));
    let at = match name {
        "Shelf bracket" => BRACKET_LAYERS,
        "Bud vase" => VASE_LAYERS,
        "Lattice cube" => LATTICE_LAYERS,
        _ => panic!("no representative layers for {name}"),
    };
    Case { part, at: Some(at) }
}

/// The wavy cube (arachne) at its representative layers.
pub fn wavy_case() -> Case {
    let mut part = parts(&wavy(), "arachne").into_iter().next().expect("the wavy cube's part");
    part.name = "wavy cube (arachne)".into();
    Case { part: keep(part), at: Some(WAVY_LAYERS) }
}

/// A box or the thin tube ([`gate::solids`], arachne) at its representative layers.
pub fn solid_case(name: &str) -> Case {
    let (_, shape, height) = gate::solids().into_iter().find(|(n, ..)| *n == name).unwrap_or_else(|| panic!("no solid {name}"));
    let part = gate::analytic_part(&format!("{name} (arachne)"), shape, height, &job("arachne")).expect("a field part");
    Case { part: keep(part), at: Some(if name == "box" { BOX_LAYERS } else { TUBE_LAYERS }) }
}

/// `#[test]`s, each running `$run` on its case (in a stage's module: `trace::square_classic`).
#[allow(unused_macros)]
macro_rules! cases {
    ($run:ident { $($test:ident: $case:expr;)* }) => {
        $(
            #[test]
            fn $test() {
                $run($case)
            }
        )*
    };
}

/// The field cases whose CPU contour lies on the zero set, which both the CPU and the GPU gate
/// run: the analytic shapes but the wedge under both wall generators, the demo plate's vase at
/// its representative layers, a box and a thin tube.
#[allow(unused_macros)]
macro_rules! on_zero_set_cases {
    ($run:ident) => {
        cases!($run {
            square_classic: crate::plates::analytic_case("square", "classic");
            square_arachne: crate::plates::analytic_case("square", "arachne");
            disk_classic: crate::plates::analytic_case("disk", "classic");
            disk_arachne: crate::plates::analytic_case("disk", "arachne");
            l_classic: crate::plates::analytic_case("L", "classic");
            l_arachne: crate::plates::analytic_case("L", "arachne");
            thin_slab_classic: crate::plates::analytic_case("thin-slab", "classic");
            thin_slab_arachne: crate::plates::analytic_case("thin-slab", "arachne");
            annulus_classic: crate::plates::analytic_case("annulus", "classic");
            annulus_arachne: crate::plates::analytic_case("annulus", "arachne");
            box_hole_classic: crate::plates::analytic_case("box-hole", "classic");
            box_hole_arachne: crate::plates::analytic_case("box-hole", "arachne");
            split_classic: crate::plates::analytic_case("split", "classic");
            split_arachne: crate::plates::analytic_case("split", "arachne");
            neck_classic: crate::plates::analytic_case("neck", "classic");
            neck_arachne: crate::plates::analytic_case("neck", "arachne");
            demo_vase_classic: crate::plates::demo_case("Bud vase", "classic");
            demo_vase_arachne: crate::plates::demo_case("Bud vase", "arachne");
            plate_box: crate::plates::solid_case("box");
            plate_thin_tube: crate::plates::solid_case("thin tube");
        });
    };
}

/// The cases on field parts, which the GPU gate runs: [`on_zero_set_cases`], and those whose
/// convex corners the CPU's 0.1 mm grid chords across (the wedge's tip, the demo lattice cube's
/// beams, the wavy cube's edges), so its contour is no reference there.
#[allow(unused_macros)]
macro_rules! field_cases {
    ($run:ident) => {
        on_zero_set_cases!($run);
        cases!($run {
            wedge_classic: crate::plates::analytic_case("wedge", "classic");
            wedge_arachne: crate::plates::analytic_case("wedge", "arachne");
            demo_lattice_classic: crate::plates::demo_case("Lattice cube", "classic");
            demo_lattice_arachne: crate::plates::demo_case("Lattice cube", "arachne");
            plate_wavy_cube: crate::plates::wavy_case();
        });
    };
}

/// A Y-up shape `height` tall standing on the bed, under `generator` with `overrides`, its box the
/// shape's own grown by half a millimetre.
fn standing_part(name: &str, root: SdfNode, half: [f64; 3], height: f64, generator: &str, overrides: &[(&str, &str)]) -> GatePart {
    let shape = SdfShape { root, min: half.map(|v| -v - 0.5), max: half.map(|v| v + 0.5), materials: Vec::new() };
    let overrides = overrides.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    GatePart::of_field(&format!("{name} ({generator})"), gate::standing(shape, height), 0, overrides, job(generator)).expect("a field part")
}

/// The stage cases (stage_cases.rs), each on the smallest part that shows its properties.
/// Wider fixtures (the corner corpus, the vases, the 42 mm lattice) are backfill, not the gate's.
pub fn stage_case(name: &str) -> StageCase {
    let h = gate::ANALYTIC_HEIGHT;
    let block = |x: f64, y: f64, z: f64| SdfNode::Box { size: [x, y, z], radius: 0.0 };
    let at = |of: SdfNode, x: f64, y: f64, z: f64| SdfNode::Transform { at: [x, y, z], rotate: [0.0; 3], scale: 1.0, of: Box::new(of) };
    let disk = |r: f64, height: f64| SdfNode::Cylinder { radius: r, top_radius: r, height };
    let union = |of| SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of };
    let minus = |of| SdfNode::Combine { op: SdfOp::Difference, blend: 0.0, of };
    let analytic = |shape: &str, generator: &str, overrides: &[(&str, &str)]| {
        let (_, s) = gate::analytic().into_iter().find(|(n, _)| *n == shape).unwrap_or_else(|| panic!("no analytic shape {shape}"));
        let overrides = overrides.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        GatePart::of_field(&format!("{shape} ({generator})"), gate::standing(s, h), 0, overrides, job(generator)).expect("a field part")
    };
    // the levels of layer 1 (0.2 mm, no foot), for the analytic corners
    let levels = |part: &GatePart| gate::levels(&part.params(), 1, 0.2).0.into_iter().map(|l| l.inset).collect::<Vec<f64>>();
    let mid: Vec<(usize, Kind)> = vec![(1, Kind::Mid)];
    let case = |stage, part, at: Vec<(usize, Kind)>, props| StageCase { stage, part, at, props };
    match name {
        // chain and rings: two islands, kept apart at their pinches
        "islands_split" => case(Stage::Rings, analytic("split", "classic", &[]), mid, vec![Prop::Islands(2), Prop::Pinches]),
        // the count rules: one wall on the first layer, one under open sky, all under a step
        "count_rules" => {
            let shape = union(vec![at(block(8.0, 4.0, 8.0), 0.0, -2.0, 0.0), at(block(4.0, 4.0, 4.0), 0.0, 2.0, 0.0)]);
            let part = standing_part("step", shape, [4.0; 3], 8.0, "classic", &[("only_one_wall_first_layer", "1"), ("top_one_wall_type", "all top")]);
            case(Stage::Rings, part, vec![(0, Kind::First), (10, Kind::Mid), (19, Kind::TopSkin), (39, Kind::TopSkin)], vec![Prop::Walls(vec![1, 3, 3, 1])])
        }
        // walls at each adaptive height's own levels
        "adaptive_levels" => {
            let mut part = standing_part("adaptive box", block(8.0, h, 8.0), [4.0, 0.5 * h, 4.0], h, "classic", &[]);
            part.heights = vec![(0.2, 0.2), (0.32, 0.12), (0.6, 0.28)];
            case(Stage::Walls, part, vec![(0, Kind::First), (1, Kind::Mid), (2, Kind::TopSkin)], vec![Prop::Exact(0.001), Prop::FillLevel(0.001)])
        }
        // analytic corners and radii, both joins
        "box_corners_classic" | "box_corners_arachne" => {
            let generator = if name.ends_with("classic") { "classic" } else { "arachne" };
            let part = analytic("square", generator, &[]);
            let l = levels(&part);
            let corners = |k: usize, tol| Prop::Corners { wall: k, at: [[1.0, 1.0], [1.0, -1.0], [-1.0, 1.0], [-1.0, -1.0]].iter().map(|s| [s[0] * (5.0 - l[k]), s[1] * (5.0 - l[k])]).collect(), tol };
            let props = if generator == "classic" { vec![Prop::Exact(0.001), Prop::FillLevel(0.001), corners(0, 0.001), corners(l.len() - 1, 0.001)] } else { vec![corners(0, 0.01), corners(l.len() - 1, 0.01)] };
            case(Stage::Walls, part, mid, props)
        }
        "disk_radius_classic" | "disk_radius_arachne" => {
            let classic = name.ends_with("classic");
            let part = analytic("disk", if classic { "classic" } else { "arachne" }, &[]);
            let n = levels(&part).len();
            let tol = if classic { 0.002 } else { 0.01 };
            let mut props = vec![Prop::Radius { wall: 0, r: 5.0, tol }, Prop::Radius { wall: n - 1, r: 5.0, tol }];
            if classic {
                props.push(Prop::Exact(0.001));
            }
            case(Stage::Walls, part, mid, props)
        }
        // the L's reflex corner at (-1, -1): a sharp miter, a round arc
        "l_corner_classic" => {
            let part = analytic("L", "classic", &[]);
            let l = levels(&part);
            let props = (0..l.len()).map(|k| Prop::Corners { wall: k, at: vec![[-1.0 - l[k], -1.0 - l[k]]], tol: 0.001 }).collect();
            case(Stage::Walls, part, mid, props)
        }
        "l_corner_arachne" => {
            let part = analytic("L", "arachne", &[]);
            let l = levels(&part)[0];
            let s = l / 2f64.sqrt();
            // the arc as the beads' polyline, its chords up to 0.02 mm in from the true arc
            case(Stage::Walls, part, mid, vec![Prop::Rounded { wall: 0, arc: [-1.0 - s, -1.0 - s], corner: [-1.0 - l, -1.0 - l], far: 0.1 * l, tol: 0.02 }])
        }
        // the elephant foot splits the first layer's base at a 0.15 mm web, not the island
        "foot_split" => {
            let shape = union(vec![at(block(3.0, h, 3.0), -2.5, 0.0, 0.0), at(block(3.0, h, 3.0), 2.5, 0.0, 0.0), block(3.0, h, 0.15)]);
            let part = standing_part("web", shape, [4.0, 0.5 * h, 1.5], h, "classic", &[("elefant_foot_compensation", "0.1")]);
            case(Stage::Walls, part, vec![(0, Kind::First)], vec![Prop::Islands(1), Prop::FootSplit(2)])
        }
        // XY compensation of contours and holes, each sign kept
        "compensation_grow" | "compensation_mixed" => {
            let (contour, hole) = if name.ends_with("grow") { (0.12, 0.08) } else { (0.12, -0.08) };
            let part = analytic("box-hole", "classic", &[("xy_contour_compensation", &contour.to_string()), ("xy_hole_compensation", &hole.to_string())]);
            case(Stage::Walls, part, mid, vec![Prop::Compensated { contour, hole }])
        }
        // a 20 degree tip: the fill at its level to it (the walls' miters cut the tip), the same
        // output twice
        "wedge_turns_classic" => case(Stage::Walls, analytic("wedge", "classic", &[]), mid, vec![Prop::FillLevel(0.001), Prop::Deterministic]),
        "wedge_turns_arachne" => case(Stage::Walls, analytic("wedge", "arachne", &[]), mid, vec![Prop::Deterministic]),
        // thick islands' beads at the nominal spacing: a cube, a disk, a holed cube, two slabs
        "arachne_beads" | "arachne_beads_precise" => {
            let shape = union(vec![
                at(block(8.0, h, 8.0), -20.0, 0.0, 0.0),
                at(disk(3.0, h), -10.0, 0.0, 0.0),
                at(minus(vec![block(8.0, h, 8.0), disk(1.0, h + 2.0)]), 0.0, 0.0, 0.0),
                at(block(6.0, h, 1.98), 10.0, 0.0, 0.0),
                at(block(6.0, h, 2.5), 20.0, 0.0, 0.0),
            ]);
            let precise = if name.ends_with("precise") { "1" } else { "0" };
            let overrides = [("wall_loops", "2"), ("outer_wall_line_width", "0.42"), ("inner_wall_line_width", "0.45"), ("precise_outer_wall", precise)];
            let part = standing_part("beads", shape, [24.0, 0.5 * h, 4.0], h, "arachne", &overrides);
            case(Stage::Thin, part, mid, vec![Prop::Nominal(3)])
        }
        _ => panic!("no stage case {name}"),
    }
}

/// The stage cases, one `#[test]` each, in their stage's module (`walls::adaptive_levels`).
#[allow(unused_macros)]
macro_rules! stage_cases {
    ($run:ident) => {
        pub mod rings {
            use super::*;
            cases!($run {
                islands_split: crate::plates::stage_case("islands_split");
                count_rules: crate::plates::stage_case("count_rules");
            });
        }
        pub mod walls {
            use super::*;
            cases!($run {
                adaptive_levels: crate::plates::stage_case("adaptive_levels");
                box_corners_classic: crate::plates::stage_case("box_corners_classic");
                box_corners_arachne: crate::plates::stage_case("box_corners_arachne");
                disk_radius_classic: crate::plates::stage_case("disk_radius_classic");
                disk_radius_arachne: crate::plates::stage_case("disk_radius_arachne");
                l_corner_classic: crate::plates::stage_case("l_corner_classic");
                l_corner_arachne: crate::plates::stage_case("l_corner_arachne");
                foot_split: crate::plates::stage_case("foot_split");
                compensation_grow: crate::plates::stage_case("compensation_grow");
                compensation_mixed: crate::plates::stage_case("compensation_mixed");
                wedge_turns_classic: crate::plates::stage_case("wedge_turns_classic");
                wedge_turns_arachne: crate::plates::stage_case("wedge_turns_arachne");
            });
        }
        pub mod thin {
            use super::*;
            cases!($run {
                arachne_beads: crate::plates::stage_case("arachne_beads");
                arachne_beads_precise: crate::plates::stage_case("arachne_beads_precise");
            });
        }
    };
}
