//! A study's answer in a few numbers, for the AI and the panel: the plastic, the extremes, the
//! highly loaded spots and the lightly loaded bands of plate that may be cut (with how they may
//! be cut). Printer frame, mm; the caller names them after the part's features.

use super::fit;
use super::plan::{Analysis, Ground};
use crate::V3;

/// A highly loaded spot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub at: V3,
    pub vm: f64,
    pub sf: f64,
}

/// How a band may be cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Through: neither face is held or loaded.
    Through,
    /// Into the face at the high (or low) end of the plate's axis, a floor left at the other.
    FromHigh,
    FromLow,
}

/// A lightly loaded band of a plate that may be cut.
#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub axis: usize,
    pub access: Access,
    /// Its box (mm) and its area across the plate (mm²).
    pub lo: V3,
    pub hi: V3,
    pub area: f64,
    pub thickness: f64,
    /// The plastic the plate prints per mm² across it (mm: walls and skins solid, infill at
    /// its density), and the floor a pocket leaves (mm), which prints solid.
    pub printed: f64,
    pub floor: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub plastic: f64,
    pub max_vm: (f64, V3),
    pub min_sf: (f64, V3),
    pub min_sf_fixed: f64,
    pub max_u: f64,
    pub high: Vec<Spot>,
    pub low: Vec<Band>,
}

/// The highest-stressed cells' spots, at least `apart` mm from each other, at most `n`.
fn spots(g: &Ground, n: usize, apart: f64) -> Vec<Spot> {
    let r = g.results;
    let grid = &g.model.grid;
    let mut order: Vec<usize> = (0..grid.cells()).filter(|&c| r.cells[c][3] > 0.0 && r.cells[c][2].is_finite()).collect();
    order.sort_by(|&a, &b| r.cells[a][2].total_cmp(&r.cells[b][2]));
    let mut out: Vec<Spot> = Vec::new();
    for c in order {
        let at = grid.cell_center(c);
        if out.iter().any(|s| (0..3).map(|k| (s.at[k] - at[k]).powi(2)).sum::<f64>().sqrt() < apart) {
            continue;
        }
        out.push(Spot { at, vm: r.cells[c][0] as f64, sf: r.cells[c][2] as f64 });
        if out.len() == n {
            break;
        }
    }
    out
}

pub fn summarise(g: &Ground, a: &Analysis) -> Summary {
    let r = g.results;
    let grid = &g.model.grid;
    let mut low = Vec::new();
    for p in &a.plates {
        let [ua, va] = super::plates::uv_of(p.plate.axis);
        for (access, mask) in [(Access::Through, &p.through), (Access::FromHigh, &p.from_hi), (Access::FromLow, &p.from_lo)] {
            let clear = fit::clearance(mask, a.cell, a.web);
            for band in fit::bands(mask, &clear, a.cell, 0.0) {
                let area = band.cells.len() as f64 * a.cell * a.cell;
                // bands smaller than a hole of the smallest size and its web are no use
                if area < std::f64::consts::PI * (a.rules.r_min + a.web).powi(2) {
                    continue;
                }
                let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
                for &c in &band.cells {
                    let (i, j) = (c % mask.w, c / mask.w);
                    for (k, t) in [(ua, (p.lo[0] + i) as f64), (va, (p.lo[1] + j) as f64)] {
                        lo[k] = lo[k].min(a.origin[k] + t * a.cell);
                        hi[k] = hi[k].max(a.origin[k] + (t + 1.0) * a.cell);
                    }
                }
                lo[p.plate.axis] = p.faces[0];
                hi[p.plate.axis] = p.faces[1];
                low.push(Band { axis: p.plate.axis, access, lo, hi, area, thickness: p.faces[1] - p.faces[0], printed: p.plastic, floor: a.web });
            }
        }
    }
    low.sort_by(|x, y| y.area.total_cmp(&x.area));
    Summary {
        plastic: super::mass::plastic(grid, g.materials),
        max_vm: (r.max_vm.0, grid.cell_center(r.max_vm.1)),
        min_sf: (r.min_sf.0, grid.cell_center(r.min_sf.1)),
        min_sf_fixed: r.min_sf_fixed.0,
        max_u: r.max_u.0,
        high: spots(g, 3, 4.0 * a.cell),
        low,
    }
}
