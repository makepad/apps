//! Supports hold what needs them. A lattice cube (a wavy sheet in a frame, scaled up so its windows
//! are wide) from the bed only, 2 mm off the walls: what has no support under it, one top gap down
//! and within the bridging distance, is told, with where it is; no layer breaks the support rules;
//! no pad stands on the bed without a support on it that holds part; and of what a support could
//! get to from the bed, the support holds some.

use fab_model::eval::{Evaluated, Evaluator};
use fab_model::FabModel;
use fab_plate::machine::Machine;
use makepad_fab_settings::profiles::{JobSetup, Preset};
use makepad_fab_slice::live::{plan_frames_outlines, Frame, PartField};
use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};
use makepad_fab_toolpath::support::SupportLayer;
use makepad_fab_toolpath::support_rules::{Legal, BRIDGE};
use std::collections::BTreeMap;

const SCRIPTS: [(&str, &str); 1] = [("lattice_cube.cad.splash", include_str!("../../app/resources/lattice_cube.cad.splash"))];

/// The supports as asked of both cubes: trees from the bed only, 2 mm off the walls.
const SUPPORTS: &str = "settings({enable_support: \"1\" support_type: \"tree(auto)\" support_on_build_plate_only: \"1\" support_object_xy_distance: \"2\" support_threshold_angle: \"45\"})";

/// The cube the told and pad tests look at.
const PLATE: &str = "use mod.fab.*
script(\"lattice_cube.cad.splash\", params: {lattice_period: 13mm lattice_size: 27mm})
place(\"lattice_cube\", object: \"lattice\", at: [90mm 160mm], scale: 1.6, name: \"Block\")
";

/// An uncovered place this big (mm2) is told.
const TOLD_FROM: f64 = 1.0;

fn evaluated(project: &str) -> Evaluated {
    let scripts: BTreeMap<String, String> = SCRIPTS.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let m = FabModel::load("plate", project, scripts).unwrap_or_else(|f| panic!("{f}"));
    let mut e = Evaluator::new(Machine::dual_nozzle_350());
    m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1
}

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

/// The feet on the bed (where, mm2) whose tree holds no part: no top of it comes within the
/// rules' aside of the part one top gap over it.
fn feet_holding_nothing(layers: &[SupportLayer], slices: &[Vec<ExPolygon>], gap_top: usize, w: f64) -> Vec<((f64, f64), f64)> {
    let n = layers.len();
    let islands: Vec<Vec<ExPolygon>> = layers.iter().map(|l| union_ex(&[l.base.clone(), l.interface.clone()].concat())).collect();
    let meets = |a: &ExPolygon, b: &ExPolygon| a.bbox().overlaps(&b.bbox()) && !intersection_ex(std::slice::from_ref(a), std::slice::from_ref(b)).is_empty();
    // Going down, whether each island's tree holds part: it reaches the part over it, or an island
    // over it holds.
    let mut holds: Vec<Vec<bool>> = vec![Vec::new(); n];
    for l in (0..n).rev() {
        holds[l] = islands[l].iter().map(|e| {
            let part = makepad_fab_toolpath::support_rules::part_over(slices, l, std::slice::from_ref(e), gap_top, w);
            makepad_fab_toolpath::support_rules::reaches(e, &part) || l + 1 < n && (0..islands[l + 1].len()).any(|j| holds[l + 1][j] && meets(e, &islands[l + 1][j]))
        }).collect();
    }
    islands.first().map(|f| f.iter().zip(&holds[0]).filter(|(_, h)| !**h).map(|(e, _)| { let c = e.bbox().center(); ((c.x_mm(), c.y_mm()), e.area_mm2()) }).collect()).unwrap_or_default()
}

/// The cube planned: its outline, heights and support settings, the support, what of it the
/// generator told, and what needs support per layer.
struct Planned {
    outline: Vec<Vec<ExPolygon>>,
    heights: Vec<(f64, f64)>,
    p: makepad_fab_toolpath::support::SupportParams,
    layers: Vec<SupportLayer>,
    /// The ways ring walls keep the support out of, per layer.
    rings: Vec<Vec<ExPolygon>>,
    skeleton: makepad_fab_toolpath::skeleton::Skeleton,
    need: Vec<Vec<ExPolygon>>,
    gap_top: usize,
}

impl Planned {
    /// Per layer, what needs support and has support under it, one top gap down and within the
    /// bridging distance.
    fn covered(&self, l: usize) -> Vec<ExPolygon> {
        let by = makepad_fab_toolpath::support_rules::BRIDGE * self.p.line_width / 2.0;
        let Some(k) = l.checked_sub(self.gap_top + 1) else { return Vec::new() };
        let under = union_ex(&[self.layers[k].base.clone(), self.layers[k].interface.clone()].concat());
        if under.is_empty() || self.need[l].is_empty() { Vec::new() } else { intersection_ex(&self.need[l], &offset_ex(&under, by * SCALE, Join::Round(0.02 * SCALE))) }
    }
}

fn planned() -> &'static Planned {
    static PLANNED: std::sync::OnceLock<Planned> = std::sync::OnceLock::new();
    PLANNED.get_or_init(|| plan(PLATE))
}

/// `plate` with [`SUPPORTS`], planned.
fn plan(plate: &str) -> Planned {
    {
        makepad_fab_toolpath::par::set_threads(makepad_fab_toolpath::par::budget_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)));
        let ev = evaluated(&plate.replacen("use mod.fab.*\n", &format!("use mod.fab.*\n{SUPPORTS}\n"), 1));
        let ctl = makepad_fab_toolpath::par::Ctl::default();
        let mut outlines = plan_frames_outlines(ctl, &setup(&ev), &ev.plate, &frames_of(&ev), &[]).unwrap_or_else(|f| panic!("{f}"));
        let (outline, heights, p) = outlines.remove(0);
        let p = p.expect("the plate asks for support");
        let mut skeleton = makepad_fab_toolpath::skeleton::Skeleton::default();
        let tree = makepad_fab_toolpath::support::generate_recording(ctl, &outline, &heights, &p, &mut skeleton).unwrap();
        let need = makepad_fab_toolpath::support::demand(ctl, &outline, &heights, &p).unwrap();
        let h = heights.get(1).map_or(0.2, |x| x.1);
        let gap_top = makepad_fab_toolpath::support::gap_layers(p.top_gap, h);
        Planned { outline, heights, p, layers: tree.layers, rings: tree.rings, skeleton, need, gap_top }
    }
}

/// What has no support under it is told, near where it is; no layer breaks the support rules.
#[test]
fn what_has_no_support_is_told_near_it() {
    let c = planned();
    let told: Vec<&makepad_fab_toolpath::skeleton::GivenUp> = c.skeleton.given_up.iter().chain(&c.skeleton.inside).collect();
    let mut untold: Vec<String> = Vec::new();
    for l in 0..c.need.len() {
        if c.need[l].is_empty() {
            continue;
        }
        for e in difference_ex(&c.need[l], &c.covered(l)).into_iter().filter(|e| e.area_mm2() >= TOLD_FROM) {
            let b = e.bbox();
            let near = BBox { min: Pt::new(b.min.x - SCALE as i64, b.min.y - SCALE as i64), max: Pt::new(b.max.x + SCALE as i64, b.max.y + SCALE as i64) };
            if !told.iter().any(|g| g.layer.abs_diff(l) <= c.gap_top + c.p.interface_top_layers + 2 && g.bounds.overlaps(&near)) {
                untold.push(format!("layer {l}: {:.1} mm2 at ({:.1}, {:.1})", e.area_mm2(), b.center().x_mm(), b.center().y_mm()));
            }
        }
    }
    assert!(untold.is_empty(), "{} places without support are not told:\n{}", untold.len(), untold.join("\n"));
    let legal = Legal::new(&c.outline, &c.heights, &c.p, &[], &c.rings);
    // No lines are made of these regions here: a line prints in every piece of them.
    let breaks = makepad_fab_toolpath::support_rules::check(makepad_fab_toolpath::par::Ctl::default(), &legal, &c.layers, &|_, _| true).unwrap();
    assert!(breaks.is_empty(), "{} breaks of the support rules: {:?}", breaks.len(), breaks.iter().take(10).map(|b| (b.rule.code(), b.layer, b.area_mm2, b.at)).collect::<Vec<_>>());
}

/// Every pad on the bed has a support standing on it that holds part.
#[test]
fn no_pad_without_a_support() {
    let c = planned();
    let feet = feet_holding_nothing(&c.layers, &c.outline, c.gap_top, c.p.line_width);
    assert!(feet.is_empty(), "{} pads on the bed hold no part (where, mm2): {feet:?}", feet.len());
}

/// Place by place (per layer, each piece of what needs support), what of it a support could get
/// to and what of that is held (mm2): on its contact layer, where the thinnest column's centre gets
/// down to the bed from, at the steep angle round what the support keeps off (a free lean path,
/// see [`Legal::column_reach`]), within the bridging distance and the column's half of it, and
/// what of that has support under it.
fn held_where_a_support_gets_to(c: &Planned) -> (f64, f64) {
    let ctl = makepad_fab_toolpath::par::Ctl::default();
    let legal = Legal::new(&c.outline, &c.heights, &c.p, &[], &c.rings);
    let reach = legal.column_reach(ctl).unwrap();
    let by = BRIDGE * c.p.line_width / 2.0 + c.p.column();
    let (mut got_to, mut held) = (0.0, 0.0);
    for l in 0..c.need.len() {
        let Some(k) = l.checked_sub(c.gap_top + 1) else { continue };
        let covered = c.covered(l);
        for e in c.need[l].iter().filter(|e| e.area_mm2() >= TOLD_FROM) {
            let b = e.bbox();
            let near = BBox { min: Pt::new(b.min.x - (by * SCALE) as i64, b.min.y - (by * SCALE) as i64), max: Pt::new(b.max.x + (by * SCALE) as i64, b.max.y + (by * SCALE) as i64) };
            let from = reach.area(k, &near);
            if from.is_empty() {
                continue;
            }
            let to = intersection_ex(std::slice::from_ref(e), &offset_ex(&from, by * SCALE, Join::Round(0.02 * SCALE)));
            got_to += total_area_mm2(&to);
            if !to.is_empty() && !covered.is_empty() {
                held += total_area_mm2(&intersection_ex(&to, &covered));
            }
        }
    }
    (got_to, held)
}

/// The support holds something of what it could get to (no share of it kept as a floor).
#[test]
fn the_support_holds_some_of_what_it_gets_to() {
    let (got_to, held) = held_where_a_support_gets_to(planned());
    assert!(got_to > 0.0 && held > 0.0, "{held:.1} of the {got_to:.1} mm2 a support gets to is held");
}
