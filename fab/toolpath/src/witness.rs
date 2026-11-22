//! A support stem's witness: what it carries, how it gets down, and the room it may print in.
//!
//! Per stem, from its foot (on the bed, or one bottom gap over a part top) up to its highest
//! contact: the tube's centre and radius on every layer, and its body. The body A_l is where a
//! bead's centre may print for this stem on layer `l`: U_l ∩ Dn_l, where U is the flood up from
//! the foot's disc round what keeps a bead off (each layer within its lean of the one under it),
//! and Dn the flood down from the patch it holds, within the lean of the layer over it and only
//! where a bead gets down to its footing ([`Reach`]). So everything in the body stands, layer on
//! layer, on what the stem printed under it, and gets down to the foot; and every layer of it
//! keeps within reach of what it holds above.
//!
//! The route witnesses that the body is never empty: each of its centres is reached, within
//! its layer's lean of the one under it, from the foot's disc up to a contact. The tree fills the
//! body round its tube solid; coral grows its crown in it. Both read the same body.

use crate::coral::reach::Reach;
use crate::par::{Cancelled, Ctl};
use makepad_fab_slicer::clip::{intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};

/// How far (units) a grown corner's chord may lie inside its arc.
const ARC: f64 = 5000.0;

/// One overhang obligation a stem carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// The bead centre under it, on its contact layer.
    pub at: Pt,
    /// The layer its contact prints on: the highest at least one top gap under `part`.
    pub layer: usize,
    /// The part layer whose overhang it holds.
    pub part: usize,
}

/// A stem's witness (see the module).
#[derive(Clone, Debug)]
pub struct Witness {
    /// What it carries, every one held or told with its cause.
    pub contacts: Vec<Contact>,
    /// The layer it stands on and its centre there.
    pub foot: (usize, Pt),
    /// Per layer from the foot's up (`foot.0 + i`), the tube's centre: each within its layer's
    /// lean of the one under it, the first `foot.1`.
    pub route: Vec<Pt>,
    /// Per layer the same, the tube's radius (mm), from the stiffness it needs there.
    pub r: Vec<f64>,
    /// Per layer the same, A_l: where a bead's centre may print for this stem.
    pub body: Vec<Vec<ExPolygon>>,
}

impl Witness {
    /// The layers it spans, the foot's first.
    pub fn layers(&self) -> std::ops::Range<usize> {
        self.foot.0..self.foot.0 + self.route.len()
    }

    /// Its highest layer.
    pub fn top(&self) -> usize {
        self.foot.0 + self.route.len() - 1
    }

    /// On layer `l`, its tube's centre and radius and its body, if it spans `l`.
    pub fn at(&self, l: usize) -> Option<(Pt, f64, &[ExPolygon])> {
        let i = l.checked_sub(self.foot.0).filter(|&i| i < self.route.len())?;
        Some((self.route[i], self.r[i], &self.body[i]))
    }

    /// The witness of a stem standing at `route[0]` on layer `foot` with tube radii `r` (mm, one
    /// per layer of `route`), carrying `contacts`, whose `patch` per layer (layer, bead centres)
    /// is what it holds there: its body, U ∩ Dn, from `reach`, the bead's own reach (its keep is
    /// K, its lean m).
    pub fn new(ctl: Ctl, reach: &Reach, foot: usize, route: Vec<Pt>, r: Vec<f64>, contacts: Vec<Contact>, patch: &[(usize, Vec<ExPolygon>)]) -> Result<Witness, Cancelled> {
        assert!(!route.is_empty() && r.len() == route.len(), "a witness has a radius per layer of its route");
        let n = route.len();
        let grow = |set: &[ExPolygon], by: f64| if set.is_empty() || by <= 0.0 { set.to_vec() } else { offset_ex(set, by * SCALE, Join::Round(ARC)) };
        // U: up from the foot's disc, where it is reached (so round what keeps a bead off, and
        // never past where the reach's own rounding stops).
        let mut up: Vec<Vec<ExPolygon>> = Vec::with_capacity(n);
        let (c, r0) = (route[0], r[0]);
        up.push(reach.reached_of(foot, vec![ExPolygon::new(crate::support::circle(c.x_mm(), c.y_mm(), r0.max(0.01)))]));
        for i in 1..n {
            if ctl.cancelled() {
                return Err(Cancelled);
            }
            let l = foot + i;
            up.push(reach.reached_of(l, grow(&up[i - 1], reach.lean(l))));
        }
        // Dn: down from the patch, where it is reached.
        let mut down: Vec<Vec<ExPolygon>> = vec![Vec::new(); n];
        for i in (0..n).rev() {
            if ctl.cancelled() {
                return Err(Cancelled);
            }
            let l = foot + i;
            let mut here: Vec<ExPolygon> = if i + 1 < n { grow(&down[i + 1], reach.lean(l + 1)) } else { Vec::new() };
            here.extend(patch.iter().filter(|(k, _)| *k == l).flat_map(|(_, set)| set.iter().cloned()));
            let here = if here.len() > 1 { union_ex(&here) } else { here };
            down[i] = reach.reached_of(l, here);
        }
        let body = up.iter().zip(&down).map(|(u, d)| if u.is_empty() || d.is_empty() { Vec::new() } else { intersection_ex(u, d) }).collect();
        Ok(Witness { contacts, foot: (foot, route[0]), route, r, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_slicer::clip::difference_ex;

    /// A layer's lean at 0.2 mm layers and 40 degrees (mm).
    const LEAN: f64 = 0.168;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ExPolygon {
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]))
    }

    /// A ledge's patch on layer 50 over a block standing to layer 30 under its middle: the stem
    /// that holds its middle goes down round the block, and its body stands layer on layer from
    /// the foot, keeps off the block, and ends on the patch.
    #[test]
    fn a_body_stands_on_its_foot_and_ends_on_its_patch() {
        let n = 51;
        let keep: Vec<Vec<ExPolygon>> = (0..n).map(|l| if l <= 30 { vec![rect(10.0, 10.0, 16.0, 16.0)] } else { Vec::new() }).collect();
        let lean = vec![LEAN; n];
        let reach = Reach::new(Ctl::default(), &keep, &[], &lean).unwrap();
        let patch = vec![rect(11.0, 11.0, 15.0, 15.0)];
        let top = Pt::from_mm(13.0, 13.0);
        let mut route = reach.route(top, 50).expect("round the block, the patch's middle gets down");
        route.reverse();
        let foot = 51 - route.len();
        assert_eq!(foot, 0, "a stem from the bed stands on the bed");
        let r = vec![1.0; route.len()];
        let w = Witness::new(Ctl::default(), &reach, foot, route.clone(), r, vec![Contact { at: top, layer: 50, part: 51 }], &[(50, patch.clone())]).unwrap();
        assert_eq!((w.top(), w.layers()), (50, 0..51));
        for l in w.layers() {
            let (c, _, body) = w.at(l).unwrap();
            assert!(body.iter().any(|e| e.contains(c)), "layer {l}: the route's centre lies in the body");
            if !keep[l].is_empty() {
                assert!(intersection_ex(body, &keep[l]).is_empty(), "layer {l}: the body keeps off the block");
            }
            if l > foot {
                let under = offset_ex(w.at(l - 1).unwrap().2, (LEAN + 1e-3) * SCALE, Join::Round(ARC));
                let air = total_area_mm2(&difference_ex(body, &under));
                assert!(air < 1e-3, "layer {l}: {air} mm2 of the body stands on nothing of the layer under it");
            }
        }
        let over = total_area_mm2(&difference_ex(&w.body[50], &patch));
        assert!(over < 1e-6 && total_area_mm2(&w.body[50]) > 1.0, "the top is of the patch: {over} mm2 off it");
    }
}
