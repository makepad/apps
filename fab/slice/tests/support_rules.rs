//! Tree supports keep the rules a support keeps on every layer
//! ([`makepad_fab_toolpath::support_rules`]): nothing on thin air, no growth past the steep
//! angle, a footing on the bed or the part, a solid top under part, the gaps, and two lines'
//! width. The target: on the demo plates from anywhere and from the bed only, at the default side
//! gap and at 2 mm, and on an L held up at its corner, no layer breaks one. Until then each plate,
//! setting and rule keeps under a ceiling that only comes down ([`CEILINGS`]).
//!
//! The same tree, told by the contracts on material its successor keeps, is a report.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{plan_frames_outlines, plan_frames_supports, Frame, PartField};
use makepad_fab_slicer::geom::{dist_point_segment, total_area_mm2, Polygon, Pt, SCALE};
use makepad_fab_toolpath::coral::material::{of_extrusion, Role, SupportPath};
use makepad_fab_toolpath::support_rules::{ledger, prove, tally, Contract, Legal};
use std::collections::BTreeMap;

const SCRIPTS: [(&str, &str); 6] = [
    ("bracket.cad.splash", include_str!("../../app/resources/bracket.cad.splash")),
    ("vase.cad.splash", include_str!("../../app/resources/vase.cad.splash")),
    ("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash")),
    ("slice_bench_plate.cad.splash", include_str!("../../app/resources/slice_bench_plate.cad.splash")),
    ("trefoil_knot.cad.splash", include_str!("../../app/resources/demos/trefoil_knot/trefoil_knot.cad.splash")),
    ("ell.cad.splash", ELL),
];

/// An L held up at its corner by a post: an overhang that is not a square.
const ELL: &str = "use mod.model.*
use mod.fab.*
let m = mod.model
let post = m.box({size: [6mm, 20mm, 6mm] at: [-11mm, 10mm, -11mm]})
let arm_x = m.box({size: [30mm, 2mm, 8mm] at: [0mm, 21mm, -11mm]})
let arm_z = m.box({size: [8mm, 2mm, 30mm] at: [-11mm, 21mm, 0mm]})
object(m.blend([post arm_x arm_z], 0.2mm))
";

const PLATES: [(&str, &str); 3] = [
    ("demo", include_str!("../../app/resources/demo.fab.splash")),
    ("slice bench", include_str!("../../app/resources/slice_bench_plate.fab.splash")),
    ("L", "use mod.fab.*\nscript(\"ell.cad.splash\")\nplace(\"ell\", at: [150mm 110mm], name: \"L\")\n"),
];

const SETTINGS: [(&str, &str); 3] = [
    ("from anywhere", "enable_support: \"1\" support_type: \"tree(auto)\""),
    ("from the bed only", "enable_support: \"1\" support_type: \"tree(auto)\" support_on_build_plate_only: \"1\""),
    ("from the bed only, 2 mm off the walls", "enable_support: \"1\" support_type: \"tree(auto)\" support_on_build_plate_only: \"1\" support_object_xy_distance: \"2\" support_threshold_angle: \"45\""),
];

/// The slice bench from anywhere 2 mm off the walls, besides [`SETTINGS`]: where the bud vase's
/// branches part, holes open over the hollow insides of the ones under them.
const BENCH_2MM: (&str, &str) = ("from anywhere, 2 mm off the walls", "enable_support: \"1\" support_type: \"tree(auto)\" support_object_xy_distance: \"2\" support_threshold_angle: \"45\"");

/// The trefoil knot on its own, whose tube arches over itself, for the contracts from the bed
/// only 2 mm off the walls.
const TREFOIL: &str = "use mod.fab.*
script(\"trefoil_knot.cad.splash\")
color(\"Magenta\", rgb: #xd93373)
color(\"Azure\", rgb: #x2673d9)
place(\"trefoil_knot\", at: [175mm 160mm], name: \"Trefoil knot\", color: \"Magenta\")
";

/// How far a bead may stand off every bead under it and still print on one, squashed (mm).
const SQUISH: f64 = 0.12;

/// How far `path`'s bead stands off the nearest bead of `under`, edge to edge (mm).
fn off(path: &SupportPath, under: &[SupportPath]) -> f64 {
    let to = |p: Pt, q: &SupportPath| q.pts.windows(2).map(|w| dist_point_segment(p, w[0], w[1])).fold(f64::INFINITY, f64::min);
    under.iter().map(|q| path.pts.iter().map(|&p| to(p, q)).chain(q.pts.iter().map(|&p| to(p, path))).fold(f64::INFINITY, f64::min) / SCALE - (path.width + q.width) / 2.0).fold(f64::INFINITY, f64::min)
}

/// Whether `path` lies over the hollow inside of one loop of `under`: a path whose ends meet
/// within two of its lines.
fn hollow(path: &SupportPath, under: &[SupportPath]) -> bool {
    under.iter().filter(|q| q.pts.len() > 2 && q.pts[0].dist(q.pts[q.pts.len() - 1]) <= 2.0 * q.width * SCALE).any(|q| {
        let ring = Polygon { pts: q.pts.clone() };
        path.pts.iter().all(|&p| ring.contains(p))
    })
}

fn evaluated(project: &str) -> Evaluated {
    let scripts: BTreeMap<String, String> = SCRIPTS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let m = FabModel::load("plate", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

/// Fab's default process with the plate's own settings, and a filament for each of its two
/// colours.
fn setup(ev: &Evaluated) -> JobSetup {
    let mut setup = JobSetup { machine: Preset::default(), process: Preset::default(), filaments: vec![Preset::default(); 2], filament_map: vec![1, 2], nozzle_volume_type: vec!["Standard".into(); 2], bed_type: "Textured PEI Plate".into(), filament_map_mode: "Manual".into(), filament_colours: vec![None; 2] };
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

/// The breaks today's tree still makes, per plate, setting and rule: ceilings, not a goal, which
/// is none. A fix that lowers a count lowers its ceiling in the same landing; a count past its
/// ceiling fails. A plate, setting and rule not listed may break none.
///
/// Each one left is known, by its cause:
/// - R4 (the L's layer 95 in every setting; the lattice cube's layer 111, two from the bed and
///   one 2 mm off the walls): a leaning tip, fed from a tree that stands outside the part's edge,
///   reaches past the band under that edge on its top layer's back side. The back of a leaning
///   tube's section is its lower rings rising, and that rise is also what covers each layer from
///   the one over it: tip rings kept from rising only move the top a layer or two down (clipped at
///   their element's height, 11 breaks became 51; turned about their highest point, 869).
const CEILINGS: &[(&str, &str, &str, usize)] = &[
    ("L", "from anywhere", "R4", 1),
    ("L", "from the bed only", "R4", 1),
    ("L", "from the bed only, 2 mm off the walls", "R4", 1),
    ("demo", "from the bed only", "R4", 2),
    ("demo", "from the bed only, 2 mm off the walls", "R4", 1),
];

/// No plate, setting and rule breaks more than its ceiling ([`CEILINGS`]) on any layer.
#[test]
fn tree_supports_keep_the_rules_on_every_layer() {
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    // Each plate, setting and rule's breaks, as lines for a person.
    let mut broken: BTreeMap<(&str, &str, &str), Vec<String>> = BTreeMap::new();
    for (plate, text) in PLATES {
        for (how, set) in SETTINGS {
            let project = text.replacen("use mod.fab.*\n", &format!("use mod.fab.*\nsettings({{{set}}})\n"), 1);
            let ev = evaluated(&project);
            let planned = plan_frames_supports(makepad_fab_toolpath::par::Ctl::default(), &setup(&ev), &ev.plate, &frames_of(&ev), &[], None).unwrap_or_else(|f| panic!("{plate}, {how}: {f}"));
            // From anywhere every plate needs some (from the bed only, the demo's needs none it can reach).
            assert!(how != SETTINGS[0].0 || planned.iter().any(|p| !p.additions.is_empty()), "{plate}, {how}: no support at all");
            for (p, part) in planned.iter().zip(&ev.plate.parts) {
                for b in &p.skeleton.breaks {
                    broken.entry((plate, how, b.rule.code())).or_default().push(format!("{plate}, {how}: {}: {} {} on layer {} ({:.2} mm2 at {:.1}, {:.1})", part.name, b.rule.code(), b.why, b.layer, b.area_mm2, b.at.0, b.at.1));
                }
            }
        }
    }
    let ceiling = |k: &(&str, &str, &str)| CEILINGS.iter().find(|c| (c.0, c.1, c.2) == *k).map_or(0, |c| c.3);
    let count = |k: &(&str, &str, &str)| broken.get(k).map_or(0, Vec::len);
    let lower: Vec<String> = CEILINGS.iter().map(|c| (c.0, c.1, c.2)).filter(|k| count(k) < ceiling(k)).map(|k| format!("{}, {}: {} {} under its ceiling {}", k.0, k.1, count(&k), k.2, ceiling(&k))).collect();
    let past: Vec<_> = broken.keys().filter(|k| count(k) > ceiling(k)).collect();
    if !lower.is_empty() || !past.is_empty() {
        let now: Vec<String> = broken.iter().map(|((p, h, r), v)| format!("    ({p:?}, {h:?}, {r:?}, {}),", v.len())).collect();
        eprintln!("the counts now, as CEILINGS lists them:\n{}", now.join("\n"));
    }
    if !lower.is_empty() {
        eprintln!("lower these ceilings in this landing:\n{}", lower.join("\n"));
    }
    if !past.is_empty() {
        let what: Vec<String> = past.iter().map(|k| format!("{}, {}: {} {} breaks, its ceiling {}", k.0, k.1, count(k), k.2, ceiling(k))).collect();
        let lines: Vec<&String> = past.iter().flat_map(|k| &broken[*k]).collect();
        panic!("breaks of the support rules past their ceilings:\n{}\n{}", what.join("\n"), lines.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n"));
    }
}

/// Today's tree, told by the contracts on material (C2, C4-C7): per plate, setting and part,
/// each contract's faults and the worst, the interface's R1 and the fills' C5 among them, and
/// where the demand went, on each plate and setting, the slice bench at [`BENCH_2MM`] and the
/// [`TREFOIL`]. It asserts that the ledger is the demand, and that nothing prints on air (D29): a
/// path over the first layer whose bead is on no bead under it stands within [`SQUISH`] of one,
/// and not over the hollow inside of a loop under it.
#[test]
fn tree_supports_are_told_by_the_contracts() {
    let ctl = makepad_fab_toolpath::par::Ctl::default();
    makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
    let cases = PLATES.into_iter().flat_map(|(plate, text)| SETTINGS.into_iter().map(move |(how, set)| (plate, text, how, set)));
    for (plate, text, how, set) in cases.chain([(PLATES[1].0, PLATES[1].1, BENCH_2MM.0, BENCH_2MM.1), ("trefoil", TREFOIL, SETTINGS[2].0, SETTINGS[2].1)]) {
        let project = text.replacen("use mod.fab.*\n", &format!("use mod.fab.*\nsettings({{{set}}})\n"), 1);
        let ev = evaluated(&project);
        let (setup, frames) = (setup(&ev), frames_of(&ev));
        let planned = plan_frames_supports(ctl, &setup, &ev.plate, &frames, &[], None).unwrap_or_else(|f| panic!("{plate}, {how}: {f}"));
        let outlines = plan_frames_outlines(ctl, &setup, &ev.plate, &frames, &[]).unwrap_or_else(|f| panic!("{plate}, {how}: {f}"));
        for ((planned, (slices, heights, sp)), part) in planned.iter().zip(&outlines).zip(&ev.plate.parts) {
            let Some(sp) = sp else { continue };
            let n = heights.len();
            // Per layer its paths, and which of them are fills.
            let (mut paths, mut fill): (Vec<Vec<SupportPath>>, Vec<Vec<bool>>) = (vec![Vec::new(); n], vec![Vec::new(); n]);
            for (li, _, _, layer) in planned.additions.iter().filter(|a| a.0 < n) {
                let role = |e: &makepad_fab_toolpath::ExtrusionPath| if e.role == makepad_fab_toolpath::Role::SupportInterface { Role::Interface } else if *li == 0 { Role::Pad } else { Role::Body };
                for island in &layer.islands {
                    for (set, is_fill) in [(&island.walls, false), (&island.fills, true)] {
                        let got = of_extrusion(set, role, 0);
                        fill[*li].extend(std::iter::repeat_n(is_fill, got.len()));
                        paths[*li].extend(got);
                    }
                }
            }
            let legal = Legal::new(slices, heights, sp, &[], &[]);
            let faults = prove(ctl, &legal, &paths).unwrap();
            let count = tally(&faults);
            let worst = |k: Contract| faults.iter().filter(|f| f.contract == k).map(|f| f.by).fold(0.0, f64::max);
            let roof = faults.iter().filter(|f| f.contract == Contract::Carried && paths[f.layer][f.path].role == Role::Interface).count();
            let fills = faults.iter().filter(|f| f.contract == Contract::Clear && (fill[f.layer][f.path] || f.other.is_some_and(|o| fill[f.layer][o]))).count();
            let demand = makepad_fab_toolpath::support::demand(ctl, slices, heights, sp).unwrap();
            let reach = legal.column_reach(ctl).unwrap();
            let book = ledger(ctl, &legal, &demand, &paths, &reach).unwrap();
            let [covered, blocked, unreachable, infeasible] = book.areas();
            let need: f64 = demand.iter().map(|l| total_area_mm2(l)).sum();
            let told: Vec<String> = Contract::ALL.iter().zip(count).map(|(k, c)| format!("{} {c} (worst {:.3})", k.code(), worst(*k))).collect();
            println!(
                "CONTRACTS {plate}, {how}: {}: {} paths; {}; roof R1 {roof}; fill C5 {fills}; demand {need:.1} mm2: covered {covered:.1}, blocked {blocked:.1}, unreachable {unreachable:.1}, infeasible {infeasible:.1}",
                part.name,
                paths.iter().map(Vec::len).sum::<usize>(),
                told.join(", ")
            );
            let sum = covered + blocked + unreachable + infeasible;
            assert!((sum - need).abs() <= 1e-3 * need + 0.05, "{plate}, {how}: {}: the ledger is {sum:.3} mm2 of a demand of {need:.3}", part.name);
            let air: Vec<String> = faults.iter().filter(|f| f.contract == Contract::Footing && f.layer > 0).filter_map(|f| {
                let (path, under) = (&paths[f.layer][f.path], &paths[f.layer - 1]);
                let (gap, inside) = (off(path, under), hollow(path, under));
                (gap > SQUISH || inside).then(|| format!("layer {} path {}: {gap:.3} mm off every bead under it{}", f.layer, f.path, if inside { ", over the hollow inside of a loop" } else { "" }))
            }).collect();
            assert!(air.is_empty(), "{plate}, {how}: {}: support prints on air:\n{}", part.name, air.join("\n"));
        }
    }
}
