//! Stress-driven lightening: from one solved study to the cuts that make the part lighter where
//! it carries little, each variant checked by a real solve.
//!
//! - [`field`]: utilisation (1 / safety factor) per cell, smoothed by the cone filter;
//! - [`plates`]: where the part is a plate, its thin axis and faces;
//! - [`fit`]: 2D masks across a plate, their clearance, rows of holes and the largest pocket;
//! - [`plan`]: the masks from the stress and the study's fixtures and loads, and the cuts;
//! - [`cuts`]: the cuts (holes, teardrops, pockets with roofs) and their distance fields;
//! - [`mass`]: plastic as printed (walls and skins solid, infill at its density);
//! - [`search`]: a variant's knob tuned to the goal by solves, and the infill floor;
//! - [`summary`]: a study in a few numbers (extremes, loaded spots, bands that may be cut);
//! - [`variants`]: an Optimise run: each family of change tuned to the goal by solves.
//!
//! Printer frame, mm, Z the build direction. The script side (the generators, the edits, names
//! of features) is the app's.

pub mod cuts;
pub mod field;
pub mod fit;
pub mod mass;
pub mod plan;
pub mod plates;
pub mod search;
pub mod summary;
pub mod variants;

#[cfg(test)]
mod tests;

use crate::material::Materials;
use crate::study::{Load, Model};
use crate::{Outcome, V3};
use search::Score;

/// A part given as a field over `lo..hi`, sampled at cells of `h` and solved under `loads`
/// (picked again on its surface from their seeds): its model and outcome (an error when a
/// load is on a piece nothing holds).
pub fn solve_field(lo: V3, hi: V3, h: f64, field: &dyn Fn(V3) -> f64, loads: &[Load], m: &Materials, opts: &crate::solve::Options) -> Result<(Model, Outcome), String> {
    let mut distance = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = field(*p);
        }
    };
    let model = Model::new(lo, hi, h, &mut distance, m);
    let conditions = model.conditions(loads);
    let outcome = model.solve(m, &conditions, opts, &mut |_, _| false)?;
    Ok((model, outcome))
}

/// A solved variant's score: its plastic as printed (every piece of it, held or not; `m` the
/// materials its model was graded with) and its safety factor, 0 when it has more pieces
/// nothing holds than the part had (`loose`): a cut that cuts a piece off never passes.
pub fn score(model: &Model, outcome: &Outcome, m: &Materials, loose: usize) -> Score {
    let plastic = mass::plastic(&model.grid, m);
    let sf = if outcome.loose.pieces > loose { 0.0 } else { outcome.results.min_sf.0 };
    Score { plastic, sf }
}
