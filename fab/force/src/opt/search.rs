//! The searches a variant takes: one knob from "removes least" to "removes most" (a cut's size,
//! or the infill density run down), each step a real solve. A variant meets its goal when it is
//! at least the asked share lighter and its safety factor stays at or over the floor; of those,
//! the one with the most safety left. When the goal cannot be met, the lightest one that keeps
//! the floor.

/// What one solved step weighs and how safe it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Score {
    /// Plastic as printed (mm³).
    pub plastic: f64,
    /// The lowest safety factor away from the fixtures.
    pub sf: f64,
}

/// Lighter by at least `cut` (a share, 0.3: 30 %) with a safety factor of at least `min_sf`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Goal {
    pub cut: f64,
    pub min_sf: f64,
}

impl Goal {
    /// The share of `base`'s plastic a score saves (negative: heavier).
    pub fn saved(base: &Score, s: &Score) -> f64 {
        1.0 - s.plastic / base.plastic.max(1e-9)
    }
    pub fn safe(&self, s: &Score) -> bool {
        s.sf >= self.min_sf
    }
    pub fn met(&self, base: &Score, s: &Score) -> bool {
        self.safe(s) && Self::saved(base, s) >= self.cut
    }
}

/// One step of a search: the knob's value and its score.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub knob: f64,
    pub score: Score,
}

/// The knob's best value in `least..most` (either order) within `steps` solves (`eval`; an
/// error ends the search with it): the most removal first; if that is safe and saves more than
/// asked, a bisection back towards the goal (more safety left); if it is unsafe, a bisection
/// for the most removal that is safe. None: not even `least` holds the floor. The steps taken
/// come back too, in order.
pub fn tune(eval: &mut dyn FnMut(f64) -> Result<Score, String>, base: &Score, goal: &Goal, least: f64, most: f64, steps: usize) -> Result<(Option<Step>, Vec<Step>), String> {
    let mut trace = Vec::new();
    let mut run = |k: f64, trace: &mut Vec<Step>| -> Result<Score, String> {
        let s = eval(k)?;
        trace.push(Step { knob: k, score: s });
        Ok(s)
    };
    let top = run(most, &mut trace)?;
    if goal.safe(&top) && Goal::saved(base, &top) <= goal.cut {
        return Ok((Some(trace[0]), trace));
    }
    // the bracket: `ok` is safe (and, when the top is safe, meets the cut), `bad` is not
    let mut best = if goal.safe(&top) { Some(trace[0]) } else { None };
    let (mut ok, mut bad) = if goal.safe(&top) { (most, least) } else { (least, most) };
    let mut ok_known = goal.safe(&top);
    for _ in 1..steps.max(1) {
        let k = if ok_known { 0.5 * (ok + bad) } else { ok };
        let s = run(k, &mut trace)?;
        let step = *trace.last().unwrap();
        let good = if goal.safe(&top) { goal.met(base, &s) } else { goal.safe(&s) };
        if good {
            ok = k;
            ok_known = true;
            let better = match best {
                None => true,
                // a safe top: the least removal that still meets the cut; else the most removal that is safe
                Some(b) => if goal.safe(&top) { s.sf > b.score.sf } else { Goal::saved(base, &s) > Goal::saved(base, &b.score) },
            };
            if better {
                best = Some(step);
            }
        } else if !ok_known {
            // not even the least removal holds the floor
            return Ok((None, trace));
        } else {
            bad = k;
        }
    }
    Ok((best, trace))
}

/// The infill floor: the lowest density in `0..=current` (0..1) at which the safety factor
/// stays at or over `min_sf`, to `step` (a share, 0.01: 1 %), by bisection on `sf_at`
/// (each a solve). None: even `current` is under the floor. The densities tried come back too.
pub fn infill_floor(sf_at: &mut dyn FnMut(f64) -> Result<f64, String>, current: f64, min_sf: f64, step: f64) -> Result<(Option<f64>, Vec<(f64, f64)>), String> {
    let mut tried = Vec::new();
    let mut at = |d: f64, tried: &mut Vec<(f64, f64)>| -> Result<f64, String> {
        let sf = sf_at(d)?;
        tried.push((d, sf));
        Ok(sf)
    };
    if at(current, &mut tried)? < min_sf {
        return Ok((None, tried));
    }
    if at(0.0, &mut tried)? >= min_sf {
        return Ok((Some(0.0), tried));
    }
    let (mut bad, mut ok) = (0.0, current);
    while ok - bad > step {
        let mid = 0.5 * (ok + bad);
        if at(mid, &mut tried)? >= min_sf {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    Ok((Some(ok), tried))
}
