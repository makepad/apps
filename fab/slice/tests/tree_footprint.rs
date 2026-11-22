//! Trees only outside the part's footprint. With `fab_tree_outside_footprint` on, the user's
//! Lattice cube's trees stand outside what it covers (its outline at or under each layer, holes
//! filled) and reach into its faces' openings: no element of a tree is in the footprint under where
//! its branch got out of it, and no tree stands in it. Printability tells the overhang no branch
//! gets out from under as one item, kept out by the setting: information, the person's choice.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{plan_frames_outlines, plan_frames_supports, Frame, PartField};
use makepad_fab_slicer::geom::*;
use makepad_fab_toolpath::par::Ctl;
use makepad_fab_toolpath::tree;
use std::collections::BTreeMap;

const SCRIPTS: [(&str, &str); 1] = [("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash"))];

/// The user's Lattice cube: from the bed only, 2 mm off its walls, at 45 deg.
const PLATE: &str = "use mod.fab.*
script(\"lattice_cube.cad.splash\", params: {lattice_period: 13.2195mm lattice_size: 27mm})
place(\"lattice_cube\", object: \"lattice\", at: [80mm 165mm], scale: 1.6, name: \"Lattice cube\", settings: {sparse_infill_density: \"100%\"})
";

const SUPPORTS: &str = "enable_support: \"1\" support_type: \"tree(auto)\" support_on_build_plate_only: \"1\" support_object_xy_distance: \"2\" support_threshold_angle: \"45\"";

fn evaluated(settings: &str) -> Evaluated {
    let scripts: BTreeMap<String, String> = SCRIPTS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let project = PLATE.replacen("use mod.fab.*\n", &format!("use mod.fab.*\nsettings({{{settings}}})\n"), 1);
    let m = FabModel::load("plate", &project, scripts).unwrap_or_else(|f| panic!("{f}"));
    m.evaluate(&mut Evaluator::new(Machine::dual_nozzle_350()), None).unwrap_or_else(|f| panic!("{f}")).1
}

fn setup(ev: &Evaluated) -> JobSetup {
    let mut setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default()], filament_map: vec![1], nozzle_volume_type: vec!["Standard".into()], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None] };
    // The plate's printer's bed, as the app lists it.
    setup.machine.values.set_vec("printable_area", ev.machine.bed.corners());
    for (k, v) in &ev.plate.settings {
        setup.process.values.set_str(k, v);
    }
    setup
}

fn frames_of(ev: &Evaluated) -> Vec<Frame> {
    ev.frames.iter().enumerate().map(|(i, f)| {
        let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
        Frame { local: f.local.clone(), offset: f.offset, field }
    }).collect()
}

/// The cube's supports as planned, and the outline and layers they are planned from.
fn planned(settings: &str) -> (makepad_fab_toolpath::PlannedSupports, Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let ev = evaluated(settings);
    let (setup, frames) = (setup(&ev), frames_of(&ev));
    let supports = plan_frames_supports(Ctl::default(), &setup, &ev.plate, &frames, &[], None).unwrap_or_else(|f| panic!("{f}")).remove(0);
    let (outline, heights, _) = plan_frames_outlines(Ctl::default(), &setup, &ev.plate, &frames, &[]).unwrap_or_else(|f| panic!("{f}")).remove(0);
    (supports, outline, heights)
}

#[test]
fn trees_reach_in_from_outside_the_parts_footprint() {
    let settings = format!("{SUPPORTS} fab_tree_outside_footprint: \"1\"");
    let (supports, outline, heights) = planned(&settings);
    // R14's trees on the cube's slices, kept out of its footprint as the plan keeps them, in a
    // frame round the cube's middle.
    let job = makepad_fab_slice::prepare_job(&setup(&evaluated(&settings))).unwrap_or_else(|f| panic!("{f}"));
    let told = makepad_fab_toolpath::tree_told(&job).expect("the job asks for trees");
    assert!(told.outside_footprint, "the cube keeps its trees out of its footprint");
    let n = outline.len().min(heights.len());
    let mut bb = BBox::empty();
    outline[..n].iter().flatten().flat_map(|e| e.contour.pts.iter()).for_each(|&p| bb.add(p));
    let at = Pt::new(-(bb.min.x + bb.max.x) / 2, -(bb.min.y + bb.max.y) / 2);
    let moved = |sets: &[Vec<ExPolygon>]| -> Vec<Vec<ExPolygon>> {
        sets.iter().map(|set| set.iter().map(|e| {
            let mut e = e.clone();
            e.translate(at);
            e
        }).collect()).collect()
    };
    let footprint = moved(&makepad_fab_toolpath::footprint_below(Ctl::default(), &outline[..n]).unwrap());
    let slices = moved(&outline[..n]);
    let bed = vec![ExPolygon::new(Polygon::rect(Pt::from_mm(-200.0, -200.0), Pt::from_mm(200.0, 200.0)))];
    let input = tree::Input { keys: told.keys, print: told.print, slices: &slices, heights: &heights[..n], enforce: &[], block: &[], keep_out: &footprint, bed: &bed, dropped: &[] };
    let g = tree::generate(Ctl::default(), &input, &mut tree::Cache::default()).unwrap_or_else(|_| panic!("cancelled"));
    // Top down: whether a branch through each element was out of the footprint over it. An
    // element in the footprint under that, or a root in it, stands in the part.
    let inside = |l: usize, e: &tree::element::Element| footprint[l].iter().any(|f| f.contains(e.placed.expect("placed")));
    let mut out_over: Vec<Vec<bool>> = g.trees.iter().map(|t| vec![false; t.len()]).collect();
    let (mut back_in, mut reaching) = (Vec::new(), 0);
    for l in (0..g.trees.len()).rev() {
        for (i, e) in g.trees[l].iter().enumerate() {
            out_over[l][i] = e.carries.iter().any(|&c| out_over[l + 1][c] || !inside(l + 1, &g.trees[l + 1][c]));
            if inside(l, e) {
                reaching += 1;
                if out_over[l][i] || l == 0 {
                    back_in.push(l);
                }
            }
        }
    }
    let trees = g.trees.first().map_or(0, Vec::len);
    eprintln!("lattice cube: {trees} trees, {reaching} elements reaching into the footprint, {} in it under their way out", back_in.len());
    assert!(trees > 0, "trees stand round the cube");
    assert!(back_in.is_empty(), "elements in the footprint under where their branch got out, or roots in it, on layers {back_in:?}");
    assert!(reaching > 0, "the trees reach into the cube's faces");
    // The plan prints them, and tells the overhang deeper in as kept out, in one item.
    assert!(supports.additions.iter().any(|a| a.3.islands.iter().any(|i| !i.fills.is_empty())), "the plan prints the trees");
    let sk = &supports.skeleton;
    assert!(!sk.kept_out.is_empty(), "the cube's inside overhangs are told");
    let told = fab_printability::support_issues("Lattice cube", sk, &heights, [0.0; 2]);
    let kept: Vec<_> = told.iter().filter(|i| i.kind == "support_footprint").collect();
    assert!(kept.len() == 1 && kept.iter().all(|i| i.severity == fab_printability::Severity::Info && i.message.contains("inside the footprint, unsupported by your setting") && i.hint.contains("Only outside the part's footprint")), "{:?}", told.iter().map(|i| (i.kind, &i.message)).collect::<Vec<_>>());
    let area: f64 = sk.kept_out.iter().map(|g| g.area).sum();
    let held = sk.hold.map_or(0.0, |h| h.held);
    eprintln!("lattice cube: {area:.0} mm2 kept out, {held:.0} mm2 held");
    assert!(area > 100.0 && held > 0.0, "the overhang kept out: {area:.0} mm2, held: {held:.0} mm2");
}
