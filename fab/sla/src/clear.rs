//! Whether a stretch of support clears the parts: one query for every source
//! ([`Section::clear`], a distance each point is proven not to come closer than: a field's from
//! its proofs, a mesh's from its nearest triangle).
//!
//! A [`Probe`] is every ball on a line, its radius and its gap to keep from the parts each in
//! proportion between the ends (a head's cone, a pillar, a bridge). Its margin at a ball is
//! the parts' distance less the radius and the gap; along the line the margin changes no faster
//! than the line's length plus the radius's and the gap's change, so a step of the margin over
//! that rate can't pass a place it goes below 0. The march is exact: no thin wall slips between
//! samples, and a probe that starts in a part is blocked at once.

use crate::section::{Box3, Section};
use crate::solid::{lerp, norm, sub, V3};
use makepad_fab_slicer::par;
use std::sync::Arc;

/// A margin this small is touching (mm): a probe skimming a part closer than its gap plus this
/// would take ever smaller steps.
pub const TOUCH: f64 = 0.02;
/// Past this far from a part's box, the box's distance stands for the part's (mm).
const FAR: f64 = 4.0;
/// The march's points go to the parts at most this many at a time, the chunks in parallel.
const MARCH_CHUNK: usize = 32;

/// Balls along the line from `a` to `b`, their radii from `ra` to `rb` and their gaps from `ga`
/// to `gb` in proportion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Probe {
    pub a: V3,
    pub b: V3,
    pub ra: f64,
    pub rb: f64,
    pub ga: f64,
    pub gb: f64,
}

impl Probe {
    /// One radius and one gap all along.
    pub fn even(a: V3, b: V3, r: f64, gap: f64) -> Probe {
        Probe { a, b, ra: r, rb: r, ga: gap, gb: gap }
    }

    fn at(&self, t: f64) -> (V3, f64) {
        (lerp(self.a, self.b, t), self.ra + (self.rb - self.ra) * t + self.ga + (self.gb - self.ga) * t)
    }

    /// The most its margin changes per unit of t.
    fn rate(&self) -> f64 {
        norm(sub(self.b, self.a)) + (self.rb - self.ra).abs() + (self.gb - self.ga).abs()
    }
}

/// How a probe came out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Clear {
    /// Clear all the way; the least margin it met (mm), at the places it looked.
    Clear { least: f64 },
    /// Within its gap of a part (or in one) at `t` (0 at a, 1 at b), the first place it found.
    Blocked { t: f64 },
}

impl Clear {
    pub fn is_clear(&self) -> bool {
        matches!(self, Clear::Clear { .. })
    }
}

/// How far `p` is from `b` at least (0 inside it).
pub(crate) fn box_distance(b: &Box3, p: V3) -> f64 {
    let d: [f64; 3] = std::array::from_fn(|k| (b.0[k] - p[k]).max(p[k] - b.1[k]).max(0.0));
    norm(d)
}

/// The parts' distance at each of `p`, proven: the least of each part's, where a part's box is
/// [`FAR`] or more away, the box's. `need`: what each point needs proven ([`Section::clear`]).
pub fn distances(parts: &[Arc<dyn Section>], p: &[V3], need: &[f64], out: &mut [f64]) {
    out.fill(f64::INFINITY);
    let (mut at, mut pts, mut wants, mut d) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for s in parts {
        let b = s.bounds();
        at.clear();
        pts.clear();
        wants.clear();
        for (i, q) in p.iter().enumerate() {
            let far = box_distance(&b, *q);
            if far < FAR {
                at.push(i);
                pts.push(*q);
                wants.push(need[i]);
            } else {
                out[i] = out[i].min(far);
            }
        }
        if pts.is_empty() {
            continue;
        }
        d.resize(pts.len(), 0.0);
        s.clear(&pts, &wants, &mut d);
        for (&i, &v) in at.iter().zip(&d) {
            out[i] = out[i].min(v);
        }
    }
}

/// Each probe against the parts, all marched together.
pub fn clearance(parts: &[Arc<dyn Section>], probes: &[Probe]) -> Vec<Clear> {
    let mut t = vec![0.0f64; probes.len()];
    let mut least = vec![f64::INFINITY; probes.len()];
    let mut out: Vec<Option<Clear>> = vec![None; probes.len()];
    let mut live: Vec<usize> = (0..probes.len()).collect();
    let (mut pts, mut need) = (Vec::new(), Vec::new());
    while !live.is_empty() {
        pts.clear();
        need.clear();
        for &i in &live {
            let (at, reach) = probes[i].at(t[i]);
            pts.push(at);
            // just past touching: what lets the probe on
            need.push(reach + 2.0 * TOUCH);
        }
        // the probes' points each on their own, in chunks in parallel
        // (few points: fewer to a chunk, so the march's last few still spread)
        let chunk = pts.len().div_ceil(par::threads().max(1)).clamp(1, MARCH_CHUNK);
        let chunks: Vec<(usize, usize)> = (0..pts.len()).step_by(chunk).map(|a| (a, (a + chunk).min(pts.len()))).collect();
        let d: Vec<f64> = par::map(chunks.len(), |c| {
            let (a, b) = chunks[c];
            let mut d = vec![0.0; b - a];
            distances(parts, &pts[a..b], &need[a..b], &mut d);
            d
        })
        .concat();
        let mut next = Vec::with_capacity(live.len());
        for (k, &i) in live.iter().enumerate() {
            let pr = &probes[i];
            let margin = d[k] - pr.at(t[i]).1;
            least[i] = least[i].min(margin);
            if margin <= TOUCH {
                out[i] = Some(Clear::Blocked { t: t[i] });
            } else if t[i] >= 1.0 {
                out[i] = Some(Clear::Clear { least: least[i] });
            } else {
                let rate = pr.rate();
                t[i] = if rate > 0.0 { (t[i] + margin / rate).min(1.0) } else { 1.0 };
                next.push(i);
            }
        }
        live = next;
    }
    out.into_iter().map(|c| c.expect("every probe ends")).collect()
}
