//! The stage cases (GPU-EXEC §10.6): the properties a walls-side stage's output holds (the plan
//! properties W1 took off the old GPU plan path), per stage of §11's table, each on the smallest
//! input that shows it. A case hands the stage the CPU's trace of its chosen layers and of the
//! layer above each (the CPU's stage and a GPU stage get the same input, so a trace error never
//! shows in a stage's case), holds each chosen layer's islands and walls to the CPU's
//! ([`gate::walls_at`]) and its kind on the CPU ([`gate::kinds`]), then checks the case's own
//! properties of the stage's output, each failure a `property` line naming it. The CPU's stage
//! ([`cpu_stage`]) holds them all; a broken output fails.
use crate::gate::{self, GatePart, Kind, Report};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::source::{IslandWalls, WallShape};
use makepad_fab_toolpath::printcheck::{self, Check, Failure, Island, Join, Oracle, Tol};
use std::collections::{BTreeSet, HashMap};

/// The stage of §11's table a case holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Chaining the trace into rings and islands.
    Rings,
    /// Wall construction at the levels.
    Walls,
    /// Thin walls: variable-width beads.
    Thin,
}

/// A property of the stage's output at a case's chosen layers.
#[derive(Clone, Debug)]
pub enum Prop {
    /// This many islands at each chosen layer.
    Islands(usize),
    /// Splitting the outline at its pinch points keeps its islands.
    Pinches,
    /// Each island's wall count at each chosen layer, in order: the count rules (one wall on the
    /// first layer when asked, one under open sky with a top wall rule).
    Walls(Vec<usize>),
    /// The walls by themselves, nothing excused, within this many mm: their exact levels from
    /// the island's outline both ways, no crossings, their spacing (classic walls).
    Exact(f64),
    /// The fill region's boundary at the last wall's level plus half its spacing, within this
    /// many mm.
    FillLevel(f64),
    /// A vertex of wall `wall` within `tol` mm of each point (mm): analytic corners.
    Corners { wall: usize, at: Vec<[f64; 2]>, tol: f64 },
    /// Every point of wall `wall` at its level less `r` mm from the origin, within `tol` mm: a
    /// disk of radius `r`.
    Radius { wall: usize, r: f64, tol: f64 },
    /// A round join: wall `wall` passes within `tol` mm of `arc` and keeps `far` mm from the
    /// sharp `corner`.
    Rounded { wall: usize, arc: [f64; 2], corner: [f64; 2], far: f64, tol: f64 },
    /// On the first layer one island whose outer wall has this many loops: the elephant foot
    /// splits the base, not the island.
    FootSplit(usize),
    /// Every outline point at its XY compensation from the trace within 0.01 mm: contours out
    /// by `contour`, holes into the material by `hole` (mm, both signed).
    Compensated { contour: f64, hole: f64 },
    /// The stage twice on the same input gives the same output, bit for bit.
    Deterministic,
    /// At least this many islands at each chosen layer are thick at the nominal spacing: every
    /// bead closed, none a middle bead, each width within 0.01 mm of its wall's spacing.
    Nominal(usize),
}

impl Prop {
    fn name(&self) -> &'static str {
        match self {
            Prop::Islands(_) => "islands",
            Prop::Pinches => "pinches",
            Prop::Walls(_) => "wall count",
            Prop::Exact(_) => "exact levels",
            Prop::FillLevel(_) => "fill level",
            Prop::Corners { .. } => "corners",
            Prop::Radius { .. } => "radius",
            Prop::Rounded { .. } => "round join",
            Prop::FootSplit(_) => "foot split",
            Prop::Compensated { .. } => "compensation",
            Prop::Deterministic => "deterministic",
            Prop::Nominal(_) => "nominal beads",
        }
    }
}

/// One stage case: a part, its chosen layers (each the [`Kind`] it is on the CPU) and the
/// properties the stage's output holds there.
pub struct StageCase {
    pub stage: Stage,
    pub part: GatePart,
    pub at: Vec<(usize, Kind)>,
    pub props: Vec<Prop>,
}

/// A stage's output layer: its compensated outline islands and the walls of each.
#[derive(Clone, Debug, PartialEq)]
pub struct StageLayer {
    pub index: usize,
    pub outline: Vec<ExPolygon>,
    pub walls: Vec<IslandWalls>,
}

/// A stage: the CPU's trace of layers `at` in (uncompensated, as a trace hands it on), each
/// layer's outline islands and their walls out. A layer handed only as the one above another
/// needs no output.
pub type StageFn<'a> = dyn Fn(&[usize], Vec<Vec<ExPolygon>>) -> Result<Vec<StageLayer>, String> + 'a;

/// The CPU's walls stage: the planner's compensation of `trace` (layers `at`) and its walls of
/// each island, the layer above read from `trace` (the top layer has none). A layer handed only
/// as the one above another gets no walls.
pub fn cpu_stage(part: &GatePart, at: &[usize], trace: Vec<Vec<ExPolygon>>) -> Result<Vec<StageLayer>, String> {
    let p = part.params();
    let outline: HashMap<usize, Vec<ExPolygon>> = at.iter().copied().zip(part.compensate(at, trace)).collect();
    let n = part.heights.len();
    Ok(at
        .iter()
        .filter(|&&k| k + 1 == n || outline.contains_key(&(k + 1)))
        .map(|&k| {
            let above = outline.get(&(k + 1)).map(|o| o.as_slice());
            let h = part.heights[k].1;
            let walls = outline[&k].iter().map(|i| gate::cpu_walls(&p, k, h, i, above)).collect();
            StageLayer { index: k, outline: outline[&k].clone(), walls }
        })
        .collect())
}

/// One case: the stage held to the CPU's stage at the chosen layers, then the case's properties
/// of the stage's output, one report.
pub fn run(case: &StageCase, stage: &StageFn<'_>) -> Report {
    let part = &case.part;
    let n = part.heights.len();
    let at: Vec<usize> = case.at.iter().map(|c| c.0).collect();
    let mut report = Report { part: part.name.clone(), layers: n, sampled: at.clone(), windows: Vec::new(), planned: 0, failures: gate::kinds(part, &case.at), excused: Default::default(), unreferenced: Vec::new() };
    if at.iter().any(|&k| k >= n) {
        return report;
    }
    let asked: Vec<usize> = at.iter().flat_map(|&k| [k, k + 1]).filter(|&k| k < n).collect::<BTreeSet<_>>().into_iter().collect();
    let trace = part.contours(&asked);
    let fail = |layer: usize, detail: String| Failure { layer, island: None, check: Check::Islands, detail };
    let out = match stage(&asked, trace.clone()) {
        Ok(out) => out,
        Err(e) => {
            report.failures.push(fail(0, format!("the stage failed: {e}")));
            return report;
        }
    };
    for r in gate::reference(part, &at) {
        match out.iter().find(|s| s.index == r.index) {
            Some(s) => report.failures.extend(gate::walls_at(&r, &s.outline, &s.walls, false, &Tol::default()).0),
            None => report.failures.push(fail(r.index, "the stage gave no such layer".into())),
        }
    }
    let again = case.props.iter().any(|p| matches!(p, Prop::Deterministic)).then(|| stage(&asked, trace.clone()));
    for (prop, (layer, detail)) in case.props.iter().flat_map(|p| props(case, p, &asked, &trace, &out, again.as_ref()).into_iter().map(move |f| (p, f))) {
        report.failures.push(Failure { layer, island: None, check: Check::Prop, detail: format!("{}: {detail}", prop.name()) });
    }
    report
}

/// Each of wall `k`'s lines (rings, or beads at inset `k`).
fn wall_lines(w: &IslandWalls, k: usize) -> Vec<&[Pt]> {
    match &w.shape {
        WallShape::Rings(r) => r.get(k).map(|rings| rings.iter().map(|p| p.pts.as_slice()).collect()).unwrap_or_default(),
        WallShape::Beads(b) => b.iter().filter(|l| l.inset == k).map(|l| l.pts.as_slice()).collect(),
    }
}

fn wall_count(w: &IslandWalls) -> usize {
    match &w.shape {
        WallShape::Rings(r) => r.len(),
        WallShape::Beads(b) => b.iter().map(|l| l.inset + 1).max().unwrap_or(0),
    }
}

fn mm2(p: Pt) -> [f64; 2] {
    [p.x_mm(), p.y_mm()]
}

/// The distance (mm) from `q` to the nearest segment of `lines` (closed).
fn to_lines(lines: &[&[Pt]], q: [f64; 2]) -> f64 {
    let mut best = f64::INFINITY;
    for l in lines {
        let n = l.len();
        for i in 0..n {
            let (a, b) = (mm2(l[i]), mm2(l[(i + 1) % n]));
            let ab = [b[0] - a[0], b[1] - a[1]];
            let t = (((q[0] - a[0]) * ab[0] + (q[1] - a[1]) * ab[1]) / (ab[0] * ab[0] + ab[1] * ab[1]).max(1e-30)).clamp(0.0, 1.0);
            best = best.min((q[0] - a[0] - t * ab[0]).hypot(q[1] - a[1] - t * ab[1]));
        }
    }
    best
}

/// One property's failures: (layer, detail).
fn props(case: &StageCase, prop: &Prop, asked: &[usize], trace: &[Vec<ExPolygon>], out: &[StageLayer], again: Option<&Result<Vec<StageLayer>, String>>) -> Vec<(usize, String)> {
    let part = &case.part;
    let p = part.params();
    let mut f = Vec::new();
    if let Prop::Deterministic = prop {
        match again {
            Some(Ok(b)) => {
                for (x, y) in out.iter().zip(b) {
                    if x != y {
                        f.push((x.index, "the stage's second run on the same input differs".into()));
                    }
                }
                if out.len() != b.len() {
                    f.push((0, format!("{} layers, then {}", out.len(), b.len())));
                }
            }
            Some(Err(e)) => f.push((0, format!("the second run failed: {e}"))),
            None => {}
        }
        return f;
    }
    for (i, &(k, _)) in case.at.iter().enumerate() {
        let Some(s) = out.iter().find(|s| s.index == k) else {
            f.push((k, "the stage gave no such layer".into()));
            continue;
        };
        let walls = &s.walls;
        let h = part.heights[k].1;
        let (levels, join) = gate::levels(&p, k, h);
        let level = |w: usize| levels.get(w).map_or(0.0, |l| l.inset);
        let foot = if p.first(k) { p.elephant_foot.max(0.0) } else { 0.0 };
        match prop {
            Prop::Islands(n) => {
                if s.outline.len() != *n {
                    f.push((k, format!("{} islands, the case has {n}", s.outline.len())));
                }
            }
            Prop::Pinches => {
                let split = split_pinches(s.outline.clone()).len();
                if split != s.outline.len() {
                    f.push((k, format!("{} islands split at their pinches into {split}", s.outline.len())));
                }
            }
            Prop::Walls(counts) => {
                let want = counts.get(i).copied().unwrap_or(0);
                for (j, w) in walls.iter().enumerate() {
                    let got = wall_count(w);
                    if got != want {
                        f.push((k, format!("island {j} has {got} walls, the count rule {want}")));
                    }
                }
            }
            Prop::Exact(pos) => {
                let islands: Vec<Island> = s.outline.iter().zip(walls).map(|(o, w)| Island { outline: o, walls: w }).collect();
                let tol = Tol { pos: *pos, space: 2.0 * pos, ..Tol::default() };
                f.extend(printcheck::own_walls(k, &levels, join, &islands, &tol).into_iter().map(|x| (k, format!("[{}] {}", x.check, x.detail))));
            }
            Prop::FillLevel(pos) => {
                for (o, w) in s.outline.iter().zip(walls) {
                    let n = wall_count(w);
                    let want = if n == 0 { foot } else { levels[n - 1].inset + 0.5 * levels[n - 1].spacing };
                    let oracle = Oracle::new(o, join);
                    let worst = w.fill.iter().flat_map(|e| e.rings()).flat_map(|r| r.pts.iter()).map(|&q| ((oracle.metric(mm2(q)) - want).abs(), q)).fold((0.0f64, Pt::default()), |m, x| if x.0 > m.0 { x } else { m });
                    if worst.0 > *pos {
                        f.push((k, format!("the fill boundary at ({:.3}, {:.3}) is {:.4} mm off its level {want:.3}", worst.1.x_mm(), worst.1.y_mm(), worst.0)));
                    }
                }
            }
            Prop::Corners { wall, at, tol } => {
                for q in at {
                    let near = walls.iter().flat_map(|w| wall_lines(w, *wall)).flat_map(|l| l.iter()).map(|&v| (v.x_mm() - q[0]).hypot(v.y_mm() - q[1])).fold(f64::INFINITY, f64::min);
                    if near > *tol {
                        f.push((k, format!("no wall {wall} vertex within {tol} mm of ({:.4}, {:.4}) (nearest {near:.4} mm)", q[0], q[1])));
                    }
                }
            }
            Prop::Radius { wall, r, tol } => {
                let want = r - level(*wall);
                let worst = walls.iter().flat_map(|w| wall_lines(w, *wall)).flat_map(|l| l.iter()).map(|&v| ((v.x_mm().hypot(v.y_mm()) - want).abs(), v)).fold((0.0f64, Pt::default()), |m, x| if x.0 > m.0 { x } else { m });
                if worst.0 > *tol {
                    f.push((k, format!("wall {wall} at ({:.3}, {:.3}) is {:.4} mm off radius {want:.4}", worst.1.x_mm(), worst.1.y_mm(), worst.0)));
                }
            }
            Prop::Rounded { wall, arc, corner, far, tol } => {
                let lines: Vec<&[Pt]> = walls.iter().flat_map(|w| wall_lines(w, *wall)).collect();
                let to_arc = to_lines(&lines, *arc);
                if to_arc > *tol {
                    f.push((k, format!("wall {wall} misses the arc point ({:.4}, {:.4}) by {to_arc:.4} mm", arc[0], arc[1])));
                }
                let to_corner = to_lines(&lines, *corner);
                if to_corner < *far {
                    f.push((k, format!("wall {wall} is {to_corner:.4} mm from the sharp corner ({:.4}, {:.4}), a round join keeps {far} mm", corner[0], corner[1])));
                }
            }
            Prop::FootSplit(loops) => {
                if s.outline.len() != 1 {
                    f.push((k, format!("{} islands, the foot splits the base, not the island", s.outline.len())));
                } else if let Some(w) = walls.first() {
                    let n = wall_lines(w, 0).len();
                    if n != *loops {
                        f.push((k, format!("the outer wall has {n} loops, the split base {loops}")));
                    }
                }
            }
            Prop::Compensated { contour, hole } => {
                let Some(pos) = asked.iter().position(|&a| a == k) else { continue };
                let raw = &trace[pos];
                let oracles: Vec<Oracle> = raw.iter().map(|e| Oracle::new(e, Join::Miter(3.0))).collect();
                for o in &s.outline {
                    for (r, want) in o.rings().zip(std::iter::once(-contour).chain(std::iter::repeat(*hole))) {
                        for &q in &r.pts {
                            // the raw island the point belongs to: the one it is nearest the level of
                            let got = oracles.iter().map(|x| x.metric(mm2(q))).min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs())).unwrap_or(f64::NAN);
                            if !((got - want).abs() <= 0.01) {
                                f.push((k, format!("({:.3}, {:.3}) is {got:.4} mm into the trace, its compensation {want:.3}", q.x_mm(), q.y_mm())));
                                break;
                            }
                        }
                    }
                }
            }
            Prop::Nominal(thick) => {
                let nominal = |b: &makepad_fab_slicer::source::BeadLine| {
                    let spacing = levels.get(b.inset).map_or(f64::NAN, |l| l.spacing);
                    b.closed && !b.odd && b.widths.iter().all(|x| (x - spacing).abs() <= 0.01)
                };
                let n = walls.iter().filter(|w| matches!(&w.shape, WallShape::Beads(b) if b.iter().all(nominal))).count();
                if n < *thick {
                    f.push((k, format!("{n} islands thick at the nominal spacing, the case needs {thick}")));
                }
            }
            Prop::Deterministic => {}
        }
    }
    f
}
