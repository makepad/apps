//! The variants an Optimise run offers: a few families of change, each tuned by real solves to
//! the goal (lighter by the asked share as printed, the safety factor at or over its floor).
//!
//! - Holes: rows of through holes in the plates' lightly loaded bands;
//! - Pockets: the largest pocket each plate may take from a free face;
//! - Thinner: the part's sizes (its plates' thickness parameters) taken down together;
//! - Walls: one wall loop fewer (a settings change: walls print solid, so they weigh most);
//! - Infill: the print's infill density run down (no change to the shape);
//! - a family that falls short of the goal goes on with the infill run down after it.
//!
//! A shape family's knob is the utilisation share under which material may go (more of the
//! plate as it rises); the sizes' is how far each goes from its value to its least; the
//! infill's is the density. Each family is one [`search::tune`]: a few solves. The host makes
//! each candidate (the cuts on its part, its sizes, the print settings) and solves it
//! ([`Eval`]); the runner never knows whether that is a field here or a script in the app.

use super::cuts::Cut;
use super::plan::{analyse, Ground, Rules};
use super::search::{tune, Goal, Score, Step};
use crate::material::{Materials, Print};

/// The utilisation shares a shape family's knob runs over, least removal first.
pub const SHARES: (f64, f64) = (0.1, 0.6);

/// The solves a family may take.
pub const STEPS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Holes,
    Pockets,
    Thinner,
    Walls,
    Infill,
}

/// A size of the part the run may take down: its parameter's name, its value and the least it
/// may go to (mm).
#[derive(Clone, Debug, PartialEq)]
pub struct Size {
    pub name: String,
    pub value: f64,
    pub min: f64,
}

/// What a variant changes: the cuts on the part, the sizes it takes (name, mm) and the print
/// settings it prints with.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub cuts: Vec<Cut>,
    pub sizes: Vec<(String, f64)>,
    pub print: Print,
}

impl Candidate {
    /// What it prints in: the part's materials `part` (its filament and zones) under its print
    /// settings.
    pub fn materials(&self, part: &Materials) -> Materials {
        part.with_print(&self.print)
    }
}

/// A tuned variant: what it is, its change, its score and what it saves as printed (a share;
/// negative: heavier).
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub families: Vec<Family>,
    pub candidate: Candidate,
    pub score: Score,
    pub saved: f64,
    /// It meets the goal (else it is the lightest that keeps the floor).
    pub met: bool,
    /// The score of its change to the shape alone (none for the infill alone): what its cuts
    /// save as printed, the infill run down after them left out.
    pub shape: Option<Score>,
}

/// Makes and solves a candidate.
pub type Eval<'a> = dyn FnMut(&Candidate) -> Result<Score, String> + 'a;

/// The levers a run may pull.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Levers {
    pub holes: bool,
    pub pockets: bool,
    pub walls: bool,
    pub infill: bool,
    /// The lowest density the infill lever goes to (0..1).
    pub min_density: f64,
}

impl Default for Levers {
    fn default() -> Self {
        Levers { holes: true, pockets: true, walls: true, infill: true, min_density: 0.05 }
    }
}

/// The cuts a shape family makes at knob `k` (0..1).
fn cuts_at(g: &Ground, rules: &Rules, family: Family, k: f64) -> Vec<Cut> {
    let share = SHARES.0 + k * (SHARES.1 - SHARES.0);
    let a = analyse(g, rules, share);
    match family {
        Family::Holes => a.holes(1.0),
        Family::Pockets => a.pockets(1.0),
        Family::Thinner | Family::Walls | Family::Infill => Vec::new(),
    }
}

/// The sizes at knob `k` (0..1): each from its value towards its least.
fn sizes_at(sizes: &[Size], k: f64) -> Vec<(String, f64)> {
    sizes.iter().map(|s| (s.name.clone(), s.value - k * (s.value - s.min.min(s.value)))).collect()
}

/// The infill run down from `print`'s density towards the levers' lowest, with `cuts` made and
/// `sizes` taken.
fn infill_run(print: &Print, base: &Score, goal: &Goal, levers: &Levers, change: (Vec<Cut>, Vec<(String, f64)>), families: Vec<Family>, eval: &mut Eval) -> Result<Option<Variant>, String> {
    let (least, most) = (print.density, levers.min_density.min(print.density));
    if least - most < 1e-6 {
        return Ok(None);
    }
    let at = |d: f64| Candidate { cuts: change.0.clone(), sizes: change.1.clone(), print: Print { density: d, ..print.clone() } };
    let mut f = |d: f64| eval(&at(d));
    let (best, _) = tune(&mut f, base, goal, least, most, STEPS)?;
    Ok(best.map(|s: Step| Variant { families, candidate: at(s.knob), score: s.score, saved: Goal::saved(base, &s.score), met: goal.met(base, &s.score), shape: None }))
}

/// Runs every family the levers allow from `base` (the part as it is, solved): each family's
/// best variant, those that meet the goal first, then by what they save. Shape families with
/// no room for a cut give none, and Thinner none without `sizes`.
pub fn run(g: &Ground, rules: &Rules, base: &Score, goal: &Goal, levers: &Levers, sizes: &[Size], eval: &mut Eval) -> Result<Vec<Variant>, String> {
    let mut out = Vec::new();
    for family in [Family::Holes, Family::Pockets, Family::Thinner] {
        let make = |k: f64| match family {
            Family::Thinner => Candidate { cuts: Vec::new(), sizes: sizes_at(sizes, k), print: g.materials.print.clone() },
            _ => Candidate { cuts: cuts_at(g, rules, family, k), sizes: Vec::new(), print: g.materials.print.clone() },
        };
        let allowed = match family {
            Family::Holes => levers.holes && !make(1.0).cuts.is_empty(),
            Family::Pockets => levers.pockets && !make(1.0).cuts.is_empty(),
            _ => sizes.iter().any(|s| s.min < s.value),
        };
        if !allowed {
            continue;
        }
        let mut f = |k: f64| eval(&make(k));
        let (best, _) = tune(&mut f, base, goal, 0.0, 1.0, STEPS)?;
        let Some(best) = best else { continue };
        let c = make(best.knob);
        let met = goal.met(base, &best.score);
        if !met && levers.infill {
            // short of the goal: the infill goes down after the change
            if let Some(v) = infill_run(&g.materials.print, base, goal, levers, (c.cuts.clone(), c.sizes.clone()), vec![family, Family::Infill], eval)? {
                out.push(Variant { shape: Some(best.score), ..v });
                continue;
            }
        }
        out.push(Variant { families: vec![family], candidate: c, score: best.score, saved: Goal::saved(base, &best.score), met, shape: Some(best.score) });
    }
    // a wall loop fewer, down to one: one solve, the infill run down after it when it is short
    if levers.walls && g.materials.print.wall_loops > 1 {
        let fewer = Print { wall_loops: g.materials.print.wall_loops - 1, ..g.materials.print.clone() };
        let c = Candidate { cuts: Vec::new(), sizes: Vec::new(), print: fewer.clone() };
        let s = eval(&c)?;
        if goal.safe(&s) {
            let met = goal.met(base, &s);
            let after = if met || !levers.infill { None } else { infill_run(&fewer, base, goal, levers, (Vec::new(), Vec::new()), vec![Family::Walls, Family::Infill], eval)? };
            out.push(after.unwrap_or(Variant { families: vec![Family::Walls], candidate: c, score: s, saved: Goal::saved(base, &s), met, shape: None }));
        }
    }
    if levers.infill {
        if let Some(v) = infill_run(&g.materials.print, base, goal, levers, (Vec::new(), Vec::new()), vec![Family::Infill], eval)? {
            out.push(v);
        }
    }
    out.sort_by(|a, b| b.met.cmp(&a.met).then(b.saved.total_cmp(&a.saved)));
    Ok(out)
}
