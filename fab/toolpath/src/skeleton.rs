//! What a part's supports are as solid geometry, from the very run that prints them: tree
//! supports as a graph of circle stacks with their roof pads, normal supports as stacked
//! footprints. A plan keeps it and printing never reads it. [`Skeleton::pieces`] groups it into
//! the pieces that come off the part in one.

use crate::support::neck_holds;
use makepad_fab_slicer::clip::{intersection_ex, union_ex};
use makepad_fab_slicer::geom::*;

/// One stretch of a tree between joints: a circle per layer, going down from `top`.
#[derive(Clone, Debug, Default)]
pub struct Chain {
    pub top: usize,
    /// (x, y, radius) in mm.
    pub sections: Vec<(f64, f64, f64)>,
    /// The stretch it merges into.
    pub into: Option<usize>,
}

/// One printed layer of support material: its footprint, overlapping parts not yet united.
#[derive(Clone, Debug)]
pub struct Slab {
    /// Top of the layer, as the plan's z.
    pub z: f64,
    pub height: f64,
    pub region: Vec<ExPolygon>,
}

/// A plan's supports, in the frame the plan cuts in.
#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub chains: Vec<Chain>,
    /// Model layers (z, height), which `Chain::top` and the roofs index.
    pub heights: Vec<(f64, f64)>,
    /// Roof pads per model layer, and the pads a trunk spreads into under what it holds.
    pub roofs: Vec<Vec<ExPolygon>>,
    /// The pads a trunk spreads into, per model layer, as the generator leaves them for
    /// [`Skeleton::finish_tree`] to add to the roofs.
    pub pads: Vec<Vec<ExPolygon>>,
    /// How the generator blends a layer's sections: closed by this (mm)...
    pub close: f64,
    /// ...when that leaves a neck this wide (mm).
    pub neck: f64,
    /// The bed layer's support as it prints: its sections grown by the first layer's expansion,
    /// closed, less the part's clearance. It joins what stands on it.
    pub foot: Vec<ExPolygon>,
    /// Support material as footprints, lowest first.
    pub slabs: Vec<Slab>,
    /// The overhang `m.support_block` took, by model layer: it prints with nothing under it.
    pub blocked: Vec<GivenUp>,
    /// The overhang left without support because what would have held it stood on nothing, or
    /// could not reach the bed, by model layer.
    pub given_up: Vec<GivenUp>,
    /// The overhang no branch gets to, by model layer: the part closes it in, so no support
    /// gets down from under it to where it may stand, and it prints with nothing under it.
    /// Nothing in it is in `given_up` too.
    pub inside: Vec<GivenUp>,
    /// The overhang whose contact lies where a support would be locked in the part, by model
    /// layer: with no window to the open as wide as the thinnest column, it could not come out
    /// once printed, so no tip goes under it unless the part lets its supports be locked in.
    /// Nothing in it is in `given_up` or `inside` too.
    pub locked: Vec<GivenUp>,
    /// The support that is locked in the part, by model layer, where the part lets its supports
    /// be: it holds, but it can not come out once printed.
    pub unremovable: Vec<GivenUp>,
    /// The overhang over the part's footprint that no tree holds, by model layer, where the part
    /// keeps its trees out of it ([`crate::OUTSIDE_FOOTPRINT`]): no branch gets out of the
    /// footprint from under it at the branch angle. The person chose it: it is not asked of the
    /// trees ([`Hold::need`] leaves it out), and it prints with nothing under it. Nothing in it is
    /// in `given_up` too.
    pub kept_out: Vec<GivenUp>,
    /// The same over a no-trees area (`m.no_trees`), where it lies outside that footprint.
    pub no_trees: Vec<GivenUp>,
    /// Per model layer, where the trees are kept out: the footprint where the part keeps its
    /// trees out of it, and its no-trees areas. What of the overhang lies over it on the layer
    /// under is the person's choice ([`Skeleton::kept_out`], [`Skeleton::no_trees`]), so the
    /// sliced checks never tell it as uncovered. Empty when nothing is kept out.
    pub keep_out: std::sync::Arc<Vec<Vec<ExPolygon>>>,
    /// What of the tree support breaks the rules a support keeps (see
    /// [`crate::support_rules`]), by model layer: none, when it is built right.
    pub breaks: Vec<crate::support_rules::Break>,
    /// How much overhang its tree or coral supports were asked to hold.
    pub hold: Option<Hold>,
}

/// How much overhang a part's supports were asked to hold, how much of it they hold, and where
/// they may stand.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hold {
    /// The overhang that needs support, paint's blocks aside (mm2).
    pub need: f64,
    /// What of it the supports hold (mm2): what lies within half the contact gap of what prints
    /// on its contact's layer (see [`crate::support_rules::held`]).
    pub held: f64,
    /// Supports stand on the bed only.
    pub bed_only: bool,
    /// How far (mm) supports keep off the part's sides.
    pub side_gap: f64,
    /// The window (mm) a support comes out of the part through: what has no way out as wide is
    /// locked in (see [`Skeleton::locked`]).
    pub window: f64,
}

impl Hold {
    /// What `p` asks of supports for `need` mm2 of overhang, of which they hold `held`.
    pub(crate) fn of(need: f64, held: f64, p: &crate::support::SupportParams) -> Hold {
        Hold { need, held, bed_only: p.build_plate_only, side_gap: p.xy_gap, window: crate::support::window(p) }
    }
}

/// A model layer's overhang left without support.
#[derive(Clone, Copy, Debug)]
pub struct GivenUp {
    pub layer: usize,
    /// mm2.
    pub area: f64,
    /// The middle of the largest place, in the plan's frame (mm).
    pub at: [f64; 2],
    /// The box of all of it, in the plan's frame.
    pub bounds: BBox,
}

impl GivenUp {
    /// Layer `layer`'s overhang `left` without support: its area, the middle of its widest place
    /// and its box. `None` when nothing is left.
    pub(crate) fn of(layer: usize, left: &[ExPolygon]) -> Option<GivenUp> {
        let widest = left.iter().max_by(|a, b| a.area_mm2().total_cmp(&b.area_mm2()))?;
        let at = widest.bbox().center();
        Some(GivenUp { layer, area: total_area_mm2(left), at: [at.x_mm(), at.y_mm()], bounds: bbox_of(left) })
    }
}


/// What comes off in one: a connected trunk with its branches and pads, or a connected region of
/// normal support. Its parts are the sections the piece breaks into at a joint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkeletonPiece {
    /// Indices into [`Skeleton::chains`].
    pub chains: Vec<usize>,
    /// Pad islands (model layer, island).
    pub roofs: Vec<(usize, ExPolygon)>,
    /// Footprint islands (index into [`Skeleton::slabs`], island).
    pub slabs: Vec<(usize, ExPolygon)>,
}

struct Sets(Vec<usize>);

impl Sets {
    fn root(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.root(a), self.root(b));
        // The lowest index leads, so pieces come out in one order on every run.
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

fn pt_in_circle(p: Pt, c: (f64, f64, f64)) -> bool {
    (p.x_mm() - c.0).hypot(p.y_mm() - c.1) <= c.2
}

/// Whether a section and a pad island touch: an arc of the circle lies in the island, or the
/// island lies in the circle.
fn section_meets(c: (f64, f64, f64), e: &ExPolygon, bb: &BBox) -> bool {
    let at = |x: f64, y: f64| Pt::from_mm(x, y);
    if !bb.overlaps(&BBox { min: at(c.0 - c.2, c.1 - c.2), max: at(c.0 + c.2, c.1 + c.2) }) {
        return false;
    }
    let rim = (0..8).map(|k| {
        let a = k as f64 * std::f64::consts::FRAC_PI_4;
        at(c.0 + c.2 * a.cos(), c.1 + c.2 * a.sin())
    });
    e.contains(at(c.0, c.1)) || rim.into_iter().any(|p| e.contains(p)) || e.contour.pts.iter().any(|&p| pt_in_circle(p, c))
}

fn islands_meet(a: &ExPolygon, b: &ExPolygon) -> bool {
    intersection_ex(std::slice::from_ref(a), std::slice::from_ref(b)).iter().any(|e| e.area_mm2() > 1e-4)
}

impl Skeleton {
    /// What it holds (bytes): its sections and regions.
    pub(crate) fn bytes(&self) -> usize {
        let regions = |v: &[Vec<ExPolygon>]| v.iter().map(|r| crate::regions_bytes(r)).sum::<usize>();
        let chains: usize = self.chains.iter().map(|c| size_of::<Chain>() + c.sections.len() * size_of::<(f64, f64, f64)>()).sum();
        let slabs: usize = self.slabs.iter().map(|s| size_of::<Slab>() + crate::regions_bytes(&s.region)).sum();
        chains + slabs + regions(&self.roofs) + regions(&self.pads) + crate::regions_bytes(&self.foot)
    }

    /// A new stretch whose first section is on layer `top`.
    pub(crate) fn start(&mut self, top: usize) -> usize {
        self.chains.push(Chain { top, ..Default::default() });
        self.chains.len() - 1
    }

    /// Closes a tree's record: the model layers it runs on and the pads printed on them.
    pub fn finish_tree(&mut self, heights: &[(f64, f64)], roofs: Vec<Vec<ExPolygon>>) {
        self.heights = heights.to_vec();
        self.roofs = roofs;
        for (roof, pads) in self.roofs.iter_mut().zip(std::mem::take(&mut self.pads)) {
            roof.extend(pads);
        }
    }

    /// Takes away the sections that did not stand, `gone[chain][section]`. A stretch keeps what
    /// runs from its first section left to its last; one with nothing left goes.
    pub(crate) fn remove_sections(&mut self, gone: &[Vec<bool>]) {
        let mut moved = vec![None; self.chains.len()];
        let mut kept = Vec::with_capacity(self.chains.len());
        for (c, chain) in std::mem::take(&mut self.chains).into_iter().enumerate() {
            let flags = &gone[c];
            let (Some(first), Some(last)) = (flags.iter().position(|g| !g), flags.iter().rposition(|g| !g)) else { continue };
            moved[c] = Some(kept.len());
            kept.push(Chain { top: chain.top - first, sections: chain.sections[first..=last].to_vec(), into: chain.into });
        }
        for chain in &mut kept {
            chain.into = chain.into.and_then(|to| moved[to]);
        }
        self.chains = kept;
    }

    /// The footprints as islands (slab index, island), the bed foot's expansion with them.
    fn slab_islands(&self) -> Vec<(usize, ExPolygon)> {
        self.slabs.iter().enumerate().flat_map(|(i, s)| union_ex(&s.region).into_iter().map(move |e| (i, e))).collect()
    }

    /// The connected pieces, in one order on every run: what prints in one. A union errs towards
    /// one piece: two sections that overlap or that the generator blends are joined, so a piece is
    /// never taken for freer than what prints. On the bed the foot joins what stands on it, as it
    /// does in normal supports.
    pub fn pieces(&self) -> Vec<SkeletonPiece> {
        let roofs: Vec<(usize, &ExPolygon, BBox)> = self.roofs.iter().enumerate().flat_map(|(l, r)| r.iter().map(move |e| (l, e, e.bbox()))).collect();
        let slabs = self.slab_islands();
        let (nc, nr) = (self.chains.len(), roofs.len());
        let mut sets = Sets((0..nc + nr + slabs.len()).collect());
        let mut on_layer: Vec<Vec<(usize, (f64, f64, f64))>> = vec![Vec::new(); self.heights.len()];
        for (i, c) in self.chains.iter().enumerate() {
            if let Some(to) = c.into {
                sets.join(i, to);
            }
            for (k, &s) in c.sections.iter().enumerate() {
                on_layer[c.top - k].push((i, s));
            }
        }
        for sections in &mut on_layer {
            sections.sort_by(|a, b| (a.1.0 - a.1.2).total_cmp(&(b.1.0 - b.1.2)));
            for (i, &(ca, a)) in sections.iter().enumerate() {
                let end = a.0 + a.2 + 2.0 * self.close;
                for &(cb, b) in &sections[i + 1..] {
                    if b.0 - b.2 > end {
                        break;
                    }
                    if (a.0 - b.0).hypot(a.1 - b.1) < a.2 + b.2 || neck_holds(a, b, self.close, self.neck) {
                        sets.join(ca, cb);
                    }
                }
            }
        }
        for (i, &(l, e, bb)) in roofs.iter().enumerate() {
            // A pad holds the sections under it and those it cuts into on its own layer.
            for below in [Some(l), l.checked_sub(1)].into_iter().flatten() {
                for &(chain, s) in &on_layer[below] {
                    if section_meets(s, e, &bb) {
                        sets.join(nc + i, chain);
                    }
                }
            }
            for (j, &(k, other, obb)) in roofs.iter().enumerate().skip(i + 1) {
                if k > l + 1 {
                    break;
                }
                if k == l + 1 && bb.overlaps(&obb) && islands_meet(e, other) {
                    sets.join(nc + i, nc + j);
                }
            }
        }
        // The foot ties together what it meets on its own layer and the one on it.
        for e in &self.foot {
            let bb = e.bbox();
            let mut first = None;
            for (l, sections) in on_layer.iter().enumerate().take(2) {
                let held = sections.iter().filter(|(_, s)| section_meets(*s, e, &bb)).map(|&(chain, _)| chain);
                let pads = roofs.iter().enumerate().filter(|(_, (k, pad, pbb))| *k == l && bb.overlaps(pbb) && islands_meet(e, pad)).map(|(i, _)| nc + i);
                for node in held.chain(pads) {
                    match first {
                        Some(a) => sets.join(a, node),
                        None => first = Some(node),
                    }
                }
            }
        }
        let boxes: Vec<BBox> = slabs.iter().map(|(_, e)| e.bbox()).collect();
        // A coral interface joins the body material touching it, on its layer or either side.
        for (i, &(l, roof, bb)) in roofs.iter().enumerate() {
            let (z, h) = self.heights[l];
            let first = slabs.partition_point(|(s, _)| self.slabs[*s].z < z - h - 1e-6);
            for j in first..slabs.len() {
                let slab = &self.slabs[slabs[j].0];
                if slab.z - slab.height > z + 1e-6 { break; }
                if bb.overlaps(&boxes[j]) && islands_meet(roof, &slabs[j].1) {
                    sets.join(nc + i, nc + nr + j);
                }
            }
        }
        for i in 0..slabs.len() {
            for j in i + 1..slabs.len() {
                if slabs[j].0 > slabs[i].0 + 1 {
                    break;
                }
                if slabs[j].0 == slabs[i].0 + 1 && boxes[i].overlaps(&boxes[j]) && islands_meet(&slabs[i].1, &slabs[j].1) {
                    sets.join(nc + nr + i, nc + nr + j);
                }
            }
        }
        let mut out: Vec<SkeletonPiece> = Vec::new();
        let mut index = vec![usize::MAX; sets.0.len()];
        let mut piece_of = |sets: &mut Sets, out: &mut Vec<SkeletonPiece>, i: usize| {
            let root = sets.root(i);
            if index[root] == usize::MAX {
                index[root] = out.len();
                out.push(SkeletonPiece::default());
            }
            index[root]
        };
        for i in 0..nc {
            let k = piece_of(&mut sets, &mut out, i);
            out[k].chains.push(i);
        }
        for (i, &(l, e, _)) in roofs.iter().enumerate() {
            let k = piece_of(&mut sets, &mut out, nc + i);
            out[k].roofs.push((l, e.clone()));
        }
        for (i, (s, e)) in slabs.into_iter().enumerate() {
            let k = piece_of(&mut sets, &mut out, nc + nr + i);
            out[k].slabs.push((s, e));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{circle, generate_recording, SupportParams, SupportLayer};
    use makepad_fab_slicer::clip::{difference_ex, union2_ex};
    use makepad_fab_slicer::offset::{offset_ex, Join};

    const H: f64 = 0.2;
    const W: f64 = 0.4;
    const XY_GAP: f64 = 0.35;
    const BELOW: usize = 60;
    const LAYERS: usize = BELOW + 10;

    fn params() -> SupportParams {
        SupportParams { threshold_deg: 30.0, xy_gap: XY_GAP, top_gap: H, bottom_gap: H, interface_top_layers: 2, line_width: W, wall_width: W, tip_diameter: 0.8, branch_diameter: 5.0, taper_deg: 5.0, angle_max_deg: 40.0, angle_slow_deg: 25.0, build_plate_only: false, remove_small: true, first_expansion: 2.0, stand_inside: false }
    }

    fn square(cx: f64, cy: f64, side: f64) -> ExPolygon {
        let a = side / 2.0;
        ExPolygon::new(Polygon::new(vec![Pt::from_mm(cx - a, cy - a), Pt::from_mm(cx + a, cy - a), Pt::from_mm(cx + a, cy + a), Pt::from_mm(cx - a, cy + a)]))
    }

    /// Stems carrying wide caps, each far enough from the next to grow trees of its own.
    fn plate(stems: &[(f64, f64)]) -> (Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) {
        let slices = (0..LAYERS).map(|l| stems.iter().map(|&(x, y)| square(x, y, if l < BELOW { 4.0 } else { 18.0 })).collect()).collect();
        (slices, (0..LAYERS).map(|l| ((l + 1) as f64 * H, H)).collect())
    }

    fn recorded(slices: &[Vec<ExPolygon>], heights: &[(f64, f64)]) -> (Vec<SupportLayer>, Skeleton) {
        let mut skeleton = Skeleton::default();
        let layers = generate_recording(crate::par::Ctl::default(), slices, heights, &params(), &mut skeleton).unwrap().layers;
        skeleton.finish_tree(heights, layers.iter().map(|l| l.interface.clone()).collect());
        (layers, skeleton)
    }

    fn circles(skeleton: &Skeleton, l: usize, of: impl Fn(usize) -> bool) -> Vec<ExPolygon> {
        let polys: Vec<Polygon> = skeleton.chains.iter().enumerate().filter(|(i, c)| of(*i) && l <= c.top && c.top - l < c.sections.len()).map(|(_, c)| {
            let (x, y, r) = c.sections[c.top - l];
            circle(x, y, r)
        }).collect();
        makepad_fab_slicer::clip::union(&polys, makepad_fab_slicer::clip::FillRule::NonZero)
    }

    /// The model clearance as the generator cuts it: the XY gap around the model on this layer
    /// and those in the bottom gap, and the half line under the top gap's.
    fn clearance(slices: &[Vec<ExPolygon>], l: usize) -> Vec<ExPolygon> {
        let mut v = slices[l].clone();
        if l > 0 {
            v.extend(slices[l - 1].iter().cloned());
        }
        let c = offset_ex(&union_ex(&v), XY_GAP * SCALE, Join::Miter(3.0));
        match slices.get(l + 1) {
            Some(up) => union2_ex(&c, &offset_ex(up, (W / 2.0).min(XY_GAP) * SCALE, Join::Miter(3.0))),
            None => c,
        }
    }

    fn area(e: &[ExPolygon]) -> f64 {
        e.iter().map(ExPolygon::area_mm2).sum()
    }

    /// How many pieces the support prints in: each layer's islands, joined where they meet the
    /// layer below's.
    fn printed_pieces(layers: &[SupportLayer]) -> usize {
        let islands: Vec<Vec<ExPolygon>> = layers.iter().map(|l| union_ex(&[l.base.clone(), l.interface.clone()].concat())).collect();
        let mut first = Vec::with_capacity(islands.len());
        let mut sets = Sets(Vec::new());
        for i in &islands {
            first.push(sets.0.len());
            sets.0.extend(sets.0.len()..sets.0.len() + i.len());
        }
        for l in 1..islands.len() {
            for (a, e) in islands[l].iter().enumerate() {
                for (b, f) in islands[l - 1].iter().enumerate() {
                    if e.bbox().overlaps(&f.bbox()) && islands_meet(e, f) {
                        sets.join(first[l] + a, first[l - 1] + b);
                    }
                }
            }
        }
        (0..sets.0.len()).filter(|&i| sets.root(i) == i).count()
    }

    /// The skeleton is what prints: each layer's sections, less the model's clearance and the
    /// roofs, are the layer's support base; and the base never leaves the sections and the pads
    /// a trunk spreads into by more than the blend between them.
    #[test]
    fn its_cross_sections_are_the_support_base() {
        let (slices, heights) = plate(&[(50.0, 50.0), (90.0, 50.0)]);
        let (layers, skeleton) = recorded(&slices, &heights);
        let printed: f64 = layers.iter().map(|l| area(&l.base)).sum();
        assert!(printed > 100.0, "a real tree: {printed} mm2 of base");
        for l in 1..layers.len() {
            let base = &layers[l].base;
            let drawn = circles(&skeleton, l, |_| true);
            let blended = offset_ex(&[drawn.clone(), skeleton.roofs[l].clone()].concat(), (skeleton.close + 0.02) * SCALE, Join::Round(0.01 * SCALE));
            let past = area(&difference_ex(base, &blended));
            assert!(past < 0.01, "layer {l}: {past} mm2 of base outside the sections");
            let kept = difference_ex(&difference_ex(&drawn, &clearance(&slices, l)), &layers[l].interface);
            let missing = area(&difference_ex(&kept, &offset_ex(base, 0.02 * SCALE, Join::Miter(3.0))));
            // Islands under the generator's small-area cut and opened slivers go missing.
            assert!(missing < 1.0, "layer {l}: {missing} mm2 of sections not in the base");
        }
    }

    /// Each stem's trees are their own piece, nothing is lost, and no printed island spans
    /// two pieces: a piece that really is two is the safe mistake, a piece that is half of
    /// one would call what can't come off free.
    #[test]
    fn pieces_are_what_is_connected() {
        let (slices, heights) = plate(&[(50.0, 50.0), (90.0, 50.0)]);
        let (layers, skeleton) = recorded(&slices, &heights);
        let pieces = skeleton.pieces();
        assert_eq!(pieces, skeleton.pieces(), "one order on every run");
        assert_eq!(printed_pieces(&layers), pieces.len(), "as many pieces as print");
        assert!(pieces.len() >= 2, "{} pieces", pieces.len());
        let mut chains: Vec<usize> = pieces.iter().flat_map(|p| p.chains.iter().copied()).collect();
        chains.sort_unstable();
        assert_eq!(chains, (0..skeleton.chains.len()).collect::<Vec<_>>());
        for p in &pieces {
            assert!(!p.chains.is_empty(), "a pad held by no branch");
        }
        let left = |p: &SkeletonPiece| p.chains.iter().map(|&c| skeleton.chains[c].sections.iter().map(|s| s.0).fold(f64::MAX, f64::min)).fold(f64::MAX, f64::min);
        assert!(pieces.iter().any(|p| left(p) < 70.0) && pieces.iter().any(|p| left(p) > 70.0), "one piece per stem at least");
        for l in 1..layers.len() {
            let per: Vec<Vec<ExPolygon>> = pieces.iter().map(|p| circles(&skeleton, l, |c| p.chains.contains(&c))).collect();
            for island in &layers[l].base {
                let on = per.iter().filter(|c| area(&intersection_ex(std::slice::from_ref(island), c)) > 0.01).count();
                assert!(on <= 1, "layer {l}: an island in {on} pieces");
            }
        }
    }

    /// A spool on its side: support under its core and under each flange's rim, far apart above
    /// the bed. It comes off as it prints: the pieces the removal check sees are the ones that
    /// print, and the bed foot joins what stands close on the bed.
    #[test]
    fn a_spool_on_its_side_comes_off_as_it_prints() {
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| ExPolygon::new(Polygon::new(vec![Pt::from_mm(x0, y0), Pt::from_mm(x1, y0), Pt::from_mm(x1, y1), Pt::from_mm(x0, y1)]));
        let (slices, heights): (Vec<Vec<ExPolygon>>, Vec<(f64, f64)>) = ((0..100).map(|l| {
            // flanges of radius 10 at the ends, a core of radius 4 between them, its axis along x
            let z = l as f64 * H + H / 2.0 - 10.0;
            let mut v = Vec::new();
            if z.abs() < 10.0 {
                let w = (100.0 - z * z).sqrt();
                v.push(rect(30.0, 50.0 - w, 33.0, 50.0 + w));
                v.push(rect(57.0, 50.0 - w, 60.0, 50.0 + w));
            }
            if z.abs() < 4.0 {
                let w = (16.0 - z * z).sqrt();
                v.push(rect(32.0, 50.0 - w, 58.0, 50.0 + w));
            }
            union_ex(&v)
        }).collect(), (0..100).map(|l| ((l + 1) as f64 * H, H)).collect());
        let (layers, skeleton) = recorded(&slices, &heights);
        let trees = Skeleton { foot: Vec::new(), ..skeleton.clone() }.pieces().len();
        assert!(trees >= 3, "apart above the bed: {trees} trees");
        let printed = printed_pieces(&layers);
        assert!(printed < trees, "the foot joins some of them in print: {printed} of {trees}");
        assert_eq!(skeleton.pieces().len(), printed, "and they come off as they print");
    }

    /// Normal supports too: the bed foot joins the columns it reaches.
    #[test]
    fn a_normal_support_foot_joins_its_columns() {
        let slab = |z: f64, region: Vec<ExPolygon>| Slab { z, height: H, region };
        let skeleton = Skeleton { slabs: vec![slab(H, vec![square(10.0, 10.0, 8.0), square(16.0, 10.0, 8.0)]), slab(2.0 * H, vec![square(10.0, 10.0, 4.0), square(16.0, 10.0, 4.0)])], ..Default::default() };
        assert_eq!(skeleton.pieces().len(), 1);
        let apart = Skeleton { slabs: vec![skeleton.slabs[1].clone()], ..Default::default() };
        assert_eq!(apart.pieces().len(), 2);
    }
}
