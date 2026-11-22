//! A part's support paint on its layers: where `m.support_block`, `m.support_enforce` and
//! `m.no_trees` hold at each layer's plane, as regions, and what that does to what the angle asks
//! for.
//!
//! - Block takes the region away from the overhang, and keeps every branch and column out of it
//!   as it does out of the model. What it took is reported ([`LayerDemand::blocked`]): that
//!   overhang prints with nothing under it, and Printability says so.
//! - Enforce adds each downward face in the region, however shallow. It adds nothing on a top or
//!   a vertical wall, which have nothing to hold up.
//! - Both on one spot: block wins, whichever statement came last. It is the safe one for a part.
//! - No trees asks nothing of the overhang: the trees keep out of it as they keep out of the part
//!   (`crate::keep_out`), and other supports do not read it.
//!
//! A part without such paint has an empty [`Masks`]: nothing is computed, nothing moves.

use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::offset::{offset_ex, Join};
use makepad_fab_slicer::sdf::{contour_segments, SdfParams, SdfSource};
use makepad_fab_slicer::slice::segments_to_regions;
use makepad_fab_slicer::source::{SupportPaint, SupportRegion};

use crate::par::{self, Cancelled, Ctl};
use crate::skeleton::GivenUp;

/// A region's outline at a plane: the coarse cell may reach to the sampler's floor (a region's
/// distance is at least a paint brick away from every region it does not list), the fine one
/// resolves a brush stroke's band.
const CONTOUR: SdfParams = SdfParams { coarse: 2.0, fine: 0.1, lipschitz: 1.0, window: f64::INFINITY, tolerance: 1e-3, sharp_deg: 25.0 };
/// How far past the part (mm) a region that may hold anywhere is read: what stands beside the
/// part stays out of it too.
const ANYWHERE_MARGIN: f64 = 20.0;
/// What the layer below carries past its own edge (mm): the contour's noise on a vertical wall,
/// under a tilt of six degrees at the thinnest layers.
const HAIR: f64 = 0.02;
/// Under this area (mm²) a piece of a face is noise: the overhang pass drops it too.
const SLIVER: f64 = 0.01;
/// Under this area (mm²) what block took holds no bead: not worth a line in Printability.
const SPECK: f64 = 0.05;

/// The support paint per layer. Empty: the part has none.
#[derive(Clone, Default, PartialEq)]
pub struct Masks {
    block: Vec<Vec<ExPolygon>>,
    enforce: Vec<Vec<ExPolygon>>,
    no_trees: Vec<Vec<ExPolygon>>,
}

/// What a layer's overhang becomes under the paint.
pub(crate) struct LayerDemand {
    /// The overhang: less what is blocked, plus enforced faces with room for roof lines.
    pub demand: Vec<ExPolygon>,
    /// Enforced faces too narrow for roof lines: a shallow slope's strips, which lie inside the
    /// clearance of the layer under them. They are grown past it, so a bare tip finds room: each
    /// tip stands one side gap off the wall, under the slope a few layers further up.
    pub thin: Vec<ExPolygon>,
    /// What enforce adds to what the angle asks for: a support has to come near all of it.
    pub added: Vec<ExPolygon>,
    /// The overhang the angle asked for and block took.
    pub blocked: Option<GivenUp>,
}

/// Every layer's overhang under the paint.
pub(crate) struct PaintedDemand {
    /// Per layer, [`LayerDemand::demand`].
    pub demand: Vec<Vec<ExPolygon>>,
    /// Per layer, [`LayerDemand::thin`]; empty when nothing is enforced.
    pub thin: Vec<Vec<ExPolygon>>,
    /// Per layer, [`LayerDemand::added`]; empty when nothing is enforced.
    pub added: Vec<Vec<ExPolygon>>,
}

/// Round a part with bounds `part` (mm): as far past it as a region that may hold anywhere is
/// read ([`ANYWHERE_MARGIN`]).
pub(crate) fn round_the_part(part: ([f64; 3], [f64; 3])) -> ([f64; 2], [f64; 2]) {
    ([part.0[0] - ANYWHERE_MARGIN, part.0[1] - ANYWHERE_MARGIN], [part.1[0] + ANYWHERE_MARGIN, part.1[1] + ANYWHERE_MARGIN])
}

/// The region's outline at plane `z`, within `reach` (mm): what of it lies past there is never
/// contoured, however far it runs. Made once for every part that holds the region and reads it
/// at this plane within the same box ([`makepad_fab_slicer::source::RegionOutlines`]).
pub(crate) fn outline_at(region: &SupportRegion, z: f64, part: ([f64; 3], [f64; 3]), reach: ([f64; 2], [f64; 2])) -> Vec<ExPolygon> {
    let (mut lo, mut hi) = region.bounds.unwrap_or_else(|| {
        let (lo, hi) = round_the_part(part);
        ([lo[0], lo[1], part.0[2]], [hi[0], hi[1], part.1[2]])
    });
    if region.bounds.is_some() && (z < lo[2] || z > hi[2]) {
        return Vec::new();
    }
    for k in 0..2 {
        (lo[k], hi[k]) = (lo[k].max(reach.0[k]), hi[k].min(reach.1[k]));
    }
    if lo[0] >= hi[0] || lo[1] >= hi[1] {
        return Vec::new();
    }
    let key = [z, lo[0], lo[1], hi[0], hi[1]].map(f64::to_bits);
    let made = region.outlines.get_or_make(key, || {
        // The box closes a region that runs out of it.
        let (cx, cy, hx, hy) = ((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (hi[0] - lo[0]) / 2.0 + CONTOUR.fine, (hi[1] - lo[1]) / 2.0 + CONTOUR.fine);
        let field = |x: f64, y: f64| (region.at)(x, y, z).max((x - cx).abs() - hx).max((y - cy).abs() - hy);
        let pad = 2.0 * CONTOUR.fine;
        segments_to_regions(contour_segments(&field, [cx - hx - pad, cy - hy - pad], [cx + hx + pad, cy + hy + pad], &CONTOUR), &SdfSource::SLICE_PARAMS)
    });
    made.to_vec()
}

fn near(region: &[ExPolygon], bb: &BBox) -> Vec<ExPolygon> {
    region.iter().filter(|e| e.bbox().overlaps(bb)).cloned().collect()
}

impl Masks {
    /// The paint at the planes `zs` of a part with bounds `part`, cut to `reach` (mm): where its
    /// supports can be.
    pub fn of(ctl: Ctl, paint: Option<SupportPaint>, zs: &[f64], part: ([f64; 3], [f64; 3]), reach: ([f64; 2], [f64; 2])) -> Result<Masks, Cancelled> {
        let Some(paint) = paint else { return Ok(Masks::default()) };
        let layers = |r: &Option<SupportRegion>| match r {
            Some(r) => par::try_map(ctl, zs.len(), |l| outline_at(r, zs[l], part, reach)),
            None => Ok(Vec::new()),
        };
        Ok(Masks { block: layers(&paint.block)?, enforce: layers(&paint.enforce)?, no_trees: layers(&paint.no_trees)? })
    }

    /// Whether it neither blocks nor enforces: what the overhang asks for is the angle's.
    pub fn is_empty(&self) -> bool {
        self.block.is_empty() && self.enforce.is_empty()
    }

    /// Whether anything is blocked at all.
    pub fn blocks(&self) -> bool {
        !self.block.is_empty()
    }

    /// Where supports never go, per layer.
    pub(crate) fn block_layers(&self) -> &[Vec<ExPolygon>] {
        &self.block
    }

    /// Per layer, the faces enforce adds ([`Masks::face`]) less what block takes: what the trees
    /// are told to hold besides the overhang they find. Block wins here, before the trees grow what
    /// they are told to enforce by more than what they are told to block. Empty when nothing is
    /// enforced.
    pub(crate) fn enforced(&self, ctl: Ctl, slices: &[Vec<ExPolygon>]) -> Result<Vec<Vec<ExPolygon>>, Cancelled> {
        if self.enforce.is_empty() {
            return Ok(Vec::new());
        }
        par::try_map(ctl, slices.len(), |l| {
            let (face, mask) = (self.face(l, slices), self.block(l));
            if face.is_empty() || mask.is_empty() { face } else { difference_ex(&face, mask) }
        })
    }

    /// Layer `l`'s enforced face: what of the part on it lies in an enforced region and the layer
    /// under it does not carry, slivers aside.
    fn face(&self, l: usize, slices: &[Vec<ExPolygon>]) -> Vec<ExPolygon> {
        let Some(enforced) = self.enforce.get(l).filter(|m| l > 0 && !m.is_empty() && !slices[l].is_empty()) else { return Vec::new() };
        let carried = offset_ex(&slices[l - 1], HAIR * SCALE, Join::Miter(3.0));
        difference_ex(&intersection_ex(&slices[l], enforced), &carried).into_iter().filter(|e| e.area_mm2() > SLIVER).collect()
    }

    /// Where no tree stands or grows, per layer: none past its end.
    pub(crate) fn no_trees_layers(&self) -> &[Vec<ExPolygon>] {
        &self.no_trees
    }

    /// Where supports never go on layer `l`.
    pub fn block(&self, l: usize) -> &[ExPolygon] {
        self.block.get(l).map_or(&[], Vec::as_slice)
    }

    /// Layer `l`'s overhang as the angle asks for it, `asked`, under the paint. `line_width`: an
    /// enforced face narrower than twice that is [`LayerDemand::thin`], grown by `pad` (mm).
    pub(crate) fn layer(&self, l: usize, slices: &[Vec<ExPolygon>], asked: Vec<ExPolygon>, line_width: f64, pad: f64) -> LayerDemand {
        let mask = self.block(l);
        let blocked = if mask.is_empty() || asked.is_empty() {
            None
        } else {
            let lost: Vec<ExPolygon> = intersection_ex(&asked, &near(mask, &bbox_of(&asked))).into_iter().filter(|e| e.area_mm2() >= SPECK).collect();
            GivenUp::of(l, &lost)
        };
        let mut demand = asked;
        let (mut thin, mut added) = (Vec::new(), Vec::new());
        let face = self.face(l, slices);
        if !face.is_empty() {
            added = difference_ex(&face, &demand);
            let (wide, narrow): (Vec<ExPolygon>, Vec<ExPolygon>) = face.into_iter().partition(|e| !offset_ex(std::slice::from_ref(e), -line_width * SCALE, Join::Miter(3.0)).is_empty());
            if !wide.is_empty() {
                demand.extend(wide);
                demand = union_ex(&demand);
            }
            if !narrow.is_empty() {
                thin = offset_ex(&narrow, pad * SCALE, Join::Round(0.05 * SCALE));
            }
        }
        if !mask.is_empty() {
            demand = difference_ex(&demand, mask);
            thin = difference_ex(&thin, mask);
            added = difference_ex(&added, mask);
        }
        LayerDemand { demand, thin, added, blocked }
    }

    /// Every layer's overhang under the paint. What block took goes in `blocked`.
    pub(crate) fn demand(&self, ctl: Ctl, slices: &[Vec<ExPolygon>], demand: Vec<Vec<ExPolygon>>, line_width: f64, pad: f64, blocked: &mut Vec<GivenUp>) -> Result<PaintedDemand, Cancelled> {
        let mut out = PaintedDemand { demand: Vec::new(), thin: Vec::new(), added: Vec::new() };
        if self.is_empty() {
            out.demand = demand;
            return Ok(out);
        }
        for layer in par::try_map_vec(ctl, demand, |l, asked| self.layer(l, slices, asked, line_width, pad))? {
            out.demand.push(layer.demand);
            blocked.extend(layer.blocked);
            if !self.enforce.is_empty() {
                out.thin.push(layer.thin);
                out.added.push(layer.added);
            }
        }
        Ok(out)
    }

    /// `slices`, per layer the part, without what lies under something blocked up to `reach` layers
    /// over it: where a support may rest. Borrowed when nothing is blocked.
    pub(crate) fn rests_on<'a>(&self, ctl: Ctl, slices: &'a [Vec<ExPolygon>], reach: usize) -> Result<std::borrow::Cow<'a, [Vec<ExPolygon>]>, Cancelled> {
        if !self.blocks() {
            return Ok(std::borrow::Cow::Borrowed(slices));
        }
        par::try_map(ctl, slices.len(), |l| {
            let over: Vec<ExPolygon> = (l..=(l + reach).min(slices.len() - 1)).flat_map(|k| self.block[k].iter().cloned()).collect();
            if over.is_empty() { slices[l].clone() } else { difference_ex(&slices[l], &over) }
        }).map(std::borrow::Cow::Owned)
    }

    /// Per layer the overhang the paint answers for: what enforce added, and what asked for support
    /// over a block within `window` layers of it, grown by `pad` (mm), where a tip has no room to start.
    pub(crate) fn kept_away(&self, ctl: Ctl, demand: &[Vec<ExPolygon>], added: &[Vec<ExPolygon>], window: usize, pad: f64) -> Result<Vec<Vec<ExPolygon>>, Cancelled> {
        par::try_map(ctl, demand.len(), |l| {
            let under: Vec<ExPolygon> = if demand[l].is_empty() || !self.blocks() {
                Vec::new()
            } else {
                (l.saturating_sub(window)..l).flat_map(|k| self.block[k].iter().cloned()).collect()
            };
            let over = if under.is_empty() { Vec::new() } else { intersection_ex(&demand[l], &offset_ex(&under, pad * SCALE, Join::Round(0.05 * SCALE))) };
            match added.get(l).filter(|a| !a.is_empty()) {
                Some(a) => union_ex(&[over, a.clone()].concat()),
                None => over,
            }
        })
    }
}

/// A box as a region (mm): for the tests of the planners, read the way a script's box is.
#[cfg(test)]
pub(crate) fn box_region(lo: [f64; 3], hi: [f64; 3]) -> SupportRegion {
    let at = move |x: f64, y: f64, z: f64| {
        let d: [f64; 3] = std::array::from_fn(|k| {
            let (c, h) = ((lo[k] + hi[k]) / 2.0, (hi[k] - lo[k]) / 2.0);
            ([x, y, z][k] - c).abs() - h
        });
        d[0].max(d[1]).max(d[2])
    };
    SupportRegion::new(std::sync::Arc::new(at), Some((lo, hi)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box's outline at a plane is that box, where the plane cuts it.
    #[test]
    fn a_region_reads_as_its_outline_at_each_plane() {
        let paint = SupportPaint { block: Some(box_region([10.0, 20.0, 1.0], [14.0, 23.0, 2.0])), enforce: None, no_trees: None };
        let part = ([0.0; 3], [50.0, 50.0, 5.0]);
        let masks = Masks::of(Ctl::default(), Some(paint), &[0.5, 1.5, 2.5], part, round_the_part(part)).unwrap();
        assert!(masks.block(0).is_empty() && masks.block(2).is_empty(), "the plane misses the box");
        let bb = bbox_of(masks.block(1));
        for (v, want) in [(bb.min.x_mm(), 10.0), (bb.min.y_mm(), 20.0), (bb.max.x_mm(), 14.0), (bb.max.y_mm(), 23.0)] {
            assert!((v - want).abs() < 0.02, "{v} against {want}");
        }
        assert!((total_area_mm2(masks.block(1)) - 12.0).abs() < 0.1, "{}", total_area_mm2(masks.block(1)));
    }

    /// A channel with no box holds everywhere the part could reach, and the outline closes.
    #[test]
    fn a_region_with_no_box_holds_all_around_the_part() {
        let region = SupportRegion::new(std::sync::Arc::new(|_, _, _| -1e9), None);
        let part = ([10.0, 10.0, 0.0], [30.0, 40.0, 5.0]);
        let masks = Masks::of(Ctl::default(), Some(SupportPaint { block: Some(region), enforce: None, no_trees: None }), &[2.0], part, round_the_part(part)).unwrap();
        let bb = bbox_of(masks.block(0));
        assert!(bb.min.x_mm() <= 10.0 - ANYWHERE_MARGIN && bb.max.y_mm() >= 40.0 + ANYWHERE_MARGIN, "{:?}", (bb.min.x_mm(), bb.max.y_mm()));
    }

    /// The person's slip, a no-trees box drawn 2.6 by 18.4 m off towards the horizon (plate mm,
    /// beside their cube), is read where the part's supports can be and no further: its outline
    /// at each plane it reaches is the box cut to the reach, the bed round the part, and the
    /// planes over it have none. Read whole, it stalled the planner.
    #[test]
    fn a_region_metres_across_is_read_within_the_reach_alone() {
        let (lo, hi) = ([216.6, 250.6, 0.0], [2785.9, 18652.7, 5.0]);
        let part = ([58.4, 143.4, 0.0], [101.6, 186.6, 43.2]);
        // Their printer's bed, 350 by 320 mm, slid everywhere round the cube that keeps it on.
        let mut cfg = makepad_fab_settings::config::Config::new();
        cfg.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
        let reach = crate::paint_reach(&makepad_fab_settings::machine::Job::new(cfg), part);
        let want = ([101.6 - 350.0, 186.6 - 320.0], [58.4 + 350.0, 143.4 + 320.0]);
        assert!((0..2).all(|k| (reach.0[k] - want.0[k]).abs() < 1e-6 && (reach.1[k] - want.1[k]).abs() < 1e-6), "{reach:?}");
        let paint = SupportPaint { block: None, enforce: None, no_trees: Some(box_region(lo, hi)) };
        let zs: Vec<f64> = (0..30).map(|l| 0.1 + 0.2 * l as f64).collect();
        let masks = Masks::of(Ctl::default(), Some(paint), &zs, part, reach).unwrap();
        let cut = ([lo[0], lo[1]], [reach.1[0], reach.1[1]]);
        for (l, &z) in zs.iter().enumerate() {
            let outline = &masks.no_trees_layers()[l];
            if z > hi[2] {
                assert!(outline.is_empty(), "plane {z}: over the box");
                continue;
            }
            let bb = bbox_of(outline);
            let edges = [(bb.min.x_mm(), cut.0[0]), (bb.min.y_mm(), cut.0[1]), (bb.max.x_mm(), cut.1[0]), (bb.max.y_mm(), cut.1[1])];
            assert!(edges.iter().all(|(v, want)| (v - want).abs() < 2.0 * CONTOUR.fine + 0.01), "plane {z}: {edges:?}");
            let area = (cut.1[0] - cut.0[0]) * (cut.1[1] - cut.0[1]);
            let rim = 2.0 * (cut.1[0] - cut.0[0] + cut.1[1] - cut.0[1]) * CONTOUR.fine;
            assert!((total_area_mm2(outline) - area).abs() < rim, "plane {z}: {} mm² against {area}", total_area_mm2(outline));
        }
    }

    /// Both on one spot: block wins, and an enforced face block took is no loss to report. A part
    /// with no paint is left as it is.
    #[test]
    fn block_wins_over_enforce_and_no_paint_changes_nothing() {
        let square = |x0: f64, y0: f64, side: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x0 + side, y0), Pt::from_mm(x0 + side, y0 + side), Pt::from_mm(x0, y0 + side)]));
        // A 6 mm square on a 2 mm one: a face all round it. The angle asks for nothing here.
        let slices = vec![vec![square(2.0, 2.0, 2.0)], vec![square(0.0, 0.0, 6.0)]];
        let zs = [0.1, 0.3];
        let part = ([0.0; 3], [6.0, 6.0, 0.4]);
        let all = || box_region([-1.0, -1.0, 0.0], [8.0, 8.0, 1.0]);
        let run = |paint: SupportPaint| {
            let masks = Masks::of(Ctl::default(), Some(paint), &zs, part, round_the_part(part)).unwrap();
            masks.layer(1, &slices, Vec::new(), 0.4, 0.4)
        };
        let enforced = run(SupportPaint { block: None, enforce: Some(all()), no_trees: None });
        assert!(total_area_mm2(&enforced.demand) > 30.0, "enforce adds the face: {}", total_area_mm2(&enforced.demand));
        let both = run(SupportPaint { block: Some(all()), enforce: Some(all()), no_trees: None });
        assert!(both.demand.is_empty() && both.thin.is_empty() && both.blocked.is_none(), "block wins, and enforce asked for what it took");
        let asked = vec![square(0.0, 0.0, 6.0)];
        let plain = Masks::default().layer(1, &slices, asked.clone(), 0.4, 0.4);
        assert!(plain.demand == asked && plain.thin.is_empty() && plain.blocked.is_none());
    }
}
