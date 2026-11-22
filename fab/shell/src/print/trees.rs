//! The trees of a plate's supports as the Sliced view finds their branches under the pointer,
//! lights them on their own beads and deletes them.
//!
//! A branch runs from its tips down to where it joins a branch that carries more tips (or the
//! tree stands): at each disc that carries several, the branch goes on in the one carrying the
//! most tips, then the widest, and where two carry alike two branches meet and both end. A branch
//! is that run with everything it carries: deleting it takes exactly its tips, and its discs are
//! what the plan then leaves out.

use super::{Bead, PrintPreview, PREVIEW_SLOTS};
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;

/// No disc.
const NONE: u32 = u32::MAX;

/// The record roles of support beads (the toolpath's `Role::Support`, `SupportInterface` and
/// `SupportTransition`), as a bead's role slot holds them.
pub const SUPPORT_ROLES: [u8; 3] = [14, 15, 16];

/// The overlay palette slots, past 54, a lit branch's beads take: the picked one orange, the
/// hovered one in the contour's cyan.
pub const PICKED_COLOUR: f32 = 1.0;
pub const HOVERED_COLOUR: f32 = 3.0;

/// A lit branch's beads' `seg_k.y`, where an overlay bead keeps its marker's index (the hovered
/// marker's is `u_marker.x`, -1 for none): the picked branch's, the hovered one's, never a
/// marker's. The bead shader draws the picked branch's beads in every look, the printed
/// surface's too, which leaves every other overlay bead out.
pub const PICKED_TAG: f32 = -3.0;
pub const HOVERED_TAG: f32 = -2.0;

/// How much wider and taller a lit bead's section is than the bead it lights. The bead shader
/// draws a printed bead's section 1.15 times as wide and 1.3 times as tall as the bead (so
/// neighbours seal), hung below the nozzle point by half the bead's height; an overlay bead it
/// draws as given, at the point. A lit bead hangs as the printed one does and is a twentieth
/// wider and taller than its section: it shows over it in place, whole.
const LIT_WIDER: f32 = 1.15 * 1.05;
const LIT_TALLER: f32 = 1.3 * 1.05;

/// One tree of a part's supports.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateTree {
    /// Its part's object (a plate part's `object`).
    pub object: String,
    /// The tips it grew from, in the part's slice frame (mm, z the print z of the tip's layer):
    /// what deleting a branch marks on the part.
    pub tips: Vec<[f64; 3]>,
    /// Each tip's disc, into `discs`.
    pub tip_discs: Vec<u32>,
    /// Its branch on each layer it stands on, on the plate (mm): x, y, the layer's print z and
    /// height, the branch's radius. Lower layers first.
    pub discs: Vec<[f32; 5]>,
    /// Each disc's carrier on the layer below ([`NONE`]: the tree stands there).
    below: Vec<u32>,
    /// The discs each one carries on the layer above: `above[above_at[d]..above_at[d + 1]]`.
    above_at: Vec<u32>,
    above: Vec<u32>,
    /// How many tips each disc carries, its own among them.
    carried: Vec<u32>,
    /// The disc above each one that its branch goes on in ([`NONE`]: none, the branch ends
    /// there or two branches meet).
    on: Vec<u32>,
    /// The box its discs fill (mm): a pointer away from it misses every one of them.
    pub bounds: [[f32; 3]; 2],
}

impl PlateTree {
    /// A tree from its tips (each on its disc), its discs and which carries which: (the lower,
    /// the one it carries on the layer above).
    pub fn new(object: String, tips: Vec<[f64; 3]>, tip_discs: Vec<u32>, discs: Vec<[f32; 5]>, links: &[[u32; 2]]) -> PlateTree {
        let n = discs.len();
        let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
        for d in &discs {
            for (k, (lo, hi)) in [(d[0] - d[4], d[0] + d[4]), (d[1] - d[4], d[1] + d[4]), (d[2] - d[3], d[2])].into_iter().enumerate() {
                bounds[0][k] = bounds[0][k].min(lo);
                bounds[1][k] = bounds[1][k].max(hi);
            }
        }
        let links: Vec<[u32; 2]> = links.iter().copied().filter(|&[a, b]| (a as usize) < n && (b as usize) < n).collect();
        let mut below = vec![NONE; n];
        let mut above_at = vec![0u32; n + 1];
        for &[a, b] in &links {
            below[b as usize] = a;
            above_at[a as usize + 1] += 1;
        }
        for d in 0..n {
            above_at[d + 1] += above_at[d];
        }
        let mut above = vec![0u32; links.len()];
        let mut fill = above_at.clone();
        for &[a, b] in &links {
            above[fill[a as usize] as usize] = b;
            fill[a as usize] += 1;
        }
        // A disc carries its own tips and all its discs above carry; the discs above come later.
        let mut carried = vec![0u32; n];
        for &d in &tip_discs {
            if let Some(c) = carried.get_mut(d as usize) { *c += 1; }
        }
        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_by(|&a, &b| discs[b as usize][2].total_cmp(&discs[a as usize][2]));
        for &d in &order {
            if below[d as usize] != NONE {
                carried[below[d as usize] as usize] += carried[d as usize];
            }
        }
        let on = (0..n).map(|d| {
            let ups = &above[above_at[d] as usize..above_at[d + 1] as usize];
            let key = |u: u32| (carried[u as usize], discs[u as usize][4]);
            let Some(&best) = ups.iter().max_by(|&&a, &&b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)) else { return NONE };
            if ups.iter().filter(|&&u| key(u) == key(best)).count() > 1 { NONE } else { best }
        }).collect();
        PlateTree { object, tips, tip_discs, discs, below, above_at, above, carried, on, bounds }
    }

    fn above(&self, d: u32) -> &[u32] {
        &self.above[self.above_at[d as usize] as usize..self.above_at[d as usize + 1] as usize]
    }

    /// The lowest disc of the branch disc `d` is on.
    fn low(&self, d: u32) -> u32 {
        let mut d = d;
        while let Some(&b) = self.below.get(d as usize).filter(|&&b| b != NONE && self.on[b as usize] == d) {
            d = b;
        }
        d
    }

    /// The branch a pointer on disc `d` picks, `z` (mm) the pointer's height: its lowest disc.
    /// On a stem that carries one tip, that tip's branch; on one that carries several, of its own
    /// branch and those that join it above, the one ending nearest `z`.
    pub fn branch_at(&self, d: u32, z: f32) -> u32 {
        let low = self.low(d);
        let (mut best, mut off) = (low, (self.discs[low as usize][2] - z).abs());
        let mut x = d;
        loop {
            for &up in self.above(x).iter().filter(|&&u| u != self.on[x as usize]) {
                let o = (self.discs[up as usize][2] - z).abs();
                if o < off {
                    (best, off) = (up, o);
                }
            }
            match self.on[x as usize] {
                NONE => return best,
                next => x = next,
            }
        }
    }

    /// The discs of the branch whose lowest disc is `low`: it and all it carries.
    pub fn branch(&self, low: u32) -> Vec<u32> {
        let mut out = vec![low];
        let mut k = 0;
        while let Some(&d) = out.get(k) {
            out.extend_from_slice(self.above(d));
            k += 1;
        }
        out
    }

    /// The tips the branch whose lowest disc is `low` carries: what deleting it marks.
    pub fn branch_tips(&self, low: u32) -> Vec<[f64; 3]> {
        let mut on = vec![false; self.discs.len()];
        for d in self.branch(low) {
            on[d as usize] = true;
        }
        self.tips.iter().zip(&self.tip_discs).filter(|(_, &d)| on.get(d as usize).copied().unwrap_or(false)).map(|(t, _)| *t).collect()
    }

    /// The branch carrying exactly `tips`, by its lowest disc: none when no branch of this tree
    /// does (it was planned again without it).
    pub fn branch_of(&self, tips: &[[f64; 3]]) -> Option<u32> {
        let mut reached = std::collections::HashMap::new();
        for t in tips {
            let i = self.tips.iter().position(|x| x == t)?;
            let mut d = self.tip_discs[i];
            while d != NONE {
                *reached.entry(d).or_insert(0usize) += 1;
                d = self.below[d as usize];
            }
        }
        // The discs under every tip that carry no other: the lowest is the branch's.
        let n = tips.len();
        let low = reached.iter().filter(|&(&d, &k)| k == n && self.carried[d as usize] as usize == n).map(|(&d, _)| d).min_by(|&a, &b| self.discs[a as usize][2].total_cmp(&self.discs[b as usize][2]))?;
        // A branch ends where the tree stands, or where the disc under it goes on in another.
        let b = self.below[low as usize];
        (b == NONE || self.on[b as usize] != low).then_some(low)
    }
}

/// The support beads of `preview` on `discs` of `tree`, as overlay beads in overlay palette slot
/// `colour` (past 54) tagged `tag` (`PICKED_TAG`, `HOVERED_TAG`), each a little wider and taller
/// than the bead it lights so it shows over it in place; at their place on the plate.
pub fn branch_beads(preview: &PrintPreview, tree: &PlateTree, discs: &[u32], [colour, tag]: [f32; 2], data: &mut Vec<f32>) {
    // The discs by their layer's print z (µm).
    let mut by_z: std::collections::BTreeMap<i64, Vec<[f32; 3]>> = Default::default();
    let (mut zlo, mut zhi) = (f32::INFINITY, f32::NEG_INFINITY);
    for &d in discs {
        let Some(c) = tree.discs.get(d as usize) else { continue };
        by_z.entry((c[2] * 1000.0).round() as i64).or_default().push([c[0], c[1], c[4]]);
        zlo = zlo.min(c[2]);
        zhi = zhi.max(c[2]);
    }
    if by_z.is_empty() {
        return;
    }
    let offsets = &preview.part_offsets;
    let mut light = |bead: &[f32]| {
        if bead[2] < zlo - 0.01 || bead[2] > zhi + 0.01 {
            return;
        }
        let b = Bead::of(bead);
        let kind = b.kind - 8.0 * (b.kind / 8.0).floor();
        let role = b.role - 32.0 * (b.role / 32.0).floor();
        if kind > 0.5 || !SUPPORT_ROLES.iter().any(|&r| (role - r as f32).abs() < 0.5) {
            return;
        }
        // A live preview's beads are in their part's frame: its tag is nozzle + 2 * (1 + part).
        let part = (b.tag / 2.0 + 0.001).floor() as isize - 1;
        let o = if offsets.is_empty() { [0.0, 0.0] } else { usize::try_from(part).ok().and_then(|p| offsets.get(p)).copied().unwrap_or([0.0, 0.0]) };
        let mid = [(b.a[0] + b.b[0]) / 2.0 + o[0], (b.a[1] + b.b[1]) / 2.0 + o[1]];
        let key = (b.a[2] * 1000.0).round() as i64;
        let on = by_z.range(key - 10..=key + 10).flat_map(|(_, c)| c).any(|c| (mid[0] - c[0]).hypot(mid[1] - c[1]) <= c[2] + b.width.abs() / 2.0);
        if !on {
            return;
        }
        let hang = b.height.abs() / 2.0;
        let shift = |p: [f32; 3]| [p[0] + o[0], p[1] + o[1], p[2] - hang];
        let lit = Bead { a: shift(b.a), b: shift(b.b), width: b.width * LIT_WIDER, height: b.height * LIT_TALLER, role: colour, tag, kind: 3.0, ..b };
        data.extend_from_slice(&lit.pack());
    };
    for b in &preview.blocks {
        if b.bounds[1][2] < zlo - 0.01 || b.bounds[0][2] > zhi + 0.01 {
            continue;
        }
        // A run may span two of its stream's segments.
        let instances = preview.beads.of(b.stream);
        let end = (b.end as usize * PREVIEW_SLOTS).min(instances.float_len());
        for (_, part) in instances.data_slices((b.first as usize * PREVIEW_SLOTS).min(end)..end) {
            for bead in part.chunks_exact(PREVIEW_SLOTS) {
                light(bead);
            }
        }
    }
    for chunk in &preview.chunks {
        for bead in chunk.beads.data().chunks_exact(PREVIEW_SLOTS) {
            light(bead);
        }
    }
}

/// The lit branches' beads: the hovered one in cyan, the picked one in orange; each as (its
/// tree, its lowest disc) in one list of trees.
pub fn lit_beads(preview: &PrintPreview, [hovered, picked]: [Option<(&PlateTree, u32)>; 2]) -> RetainedInstances {
    let picked = picked.filter(|(t, low)| (*low as usize) < t.discs.len()).map(|(t, low)| (t, t.branch(low)));
    let mut data = Vec::new();
    if let Some((tree, low)) = hovered.filter(|(t, low)| (*low as usize) < t.discs.len()) {
        let mut discs = tree.branch(low);
        // What the picked branch lights, it lights alone.
        if let Some((_, p)) = picked.as_ref().filter(|(t, _)| std::ptr::eq(*t, tree)) {
            discs.retain(|d| !p.contains(d));
        }
        branch_beads(preview, tree, &discs, [HOVERED_COLOUR, HOVERED_TAG], &mut data);
    }
    if let Some((tree, discs)) = &picked {
        branch_beads(preview, tree, discs, [PICKED_COLOUR, PICKED_TAG], &mut data);
    }
    RetainedInstances::new(PREVIEW_SLOTS, data.into()).expect("whole lit beads")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree standing at x 0, a disc a layer (0.2 mm, the first at print z 0.2), and the place
    /// of the disc at (layer, x). Its trunk goes up to its tip on layer 12, thinning; a twig of
    /// one tip joins it at layer 9 (from x 1.5, its tip on layer 11); a branch at x -2 joins it at
    /// layer 4 and carries two alike twigs from layer 7 (to x -3 and -1, their tips on layer 9).
    fn tree() -> (PlateTree, impl Fn(u32, f32) -> u32) {
        let mut spec: Vec<(u32, f32, f32)> = (1..=12).map(|l| (l, 0.0, 2.0 - 0.1 * (l - 1) as f32)).collect();
        spec.extend([(10, 1.5, 0.5), (11, 1.5, 0.4), (5, -2.0, 1.0), (6, -2.0, 0.9), (7, -2.0, 0.8), (8, -3.0, 0.5), (9, -3.0, 0.4), (8, -1.0, 0.5), (9, -1.0, 0.4)]);
        let discs: Vec<[f32; 5]> = spec.iter().map(|&(l, x, r)| [x, 0.0, 0.2 * l as f32, 0.2, r]).collect();
        let mut links: Vec<[u32; 2]> = (0..11).map(|i| [i, i + 1]).collect();
        links.extend([[8, 12], [12, 13], [3, 14], [14, 15], [15, 16], [16, 17], [17, 18], [16, 19], [19, 20]]);
        let tips = vec![[0.0, 0.0, 2.4], [1.5, 0.0, 2.2], [-3.0, 0.0, 1.8], [-1.0, 0.0, 1.8]];
        let t = PlateTree::new("a".into(), tips, vec![11, 13, 18, 20], discs, &links);
        let at = move |layer: u32, x: f32| spec.iter().position(|&(l, sx, _)| l == layer && sx == x).expect("a disc there") as u32;
        (t, at)
    }

    /// A pointer on a branch picks it from its tips down to where it joins one carrying more
    /// (alike ones each end where they meet); on a stem carrying several, of its own branch and
    /// those joining it above, the one ending nearest the pointer's height. A branch's tips are
    /// what a delete marks, and the same tips find it again.
    #[test]
    fn a_pointer_picks_one_branch_and_its_tips() {
        let (t, at) = tree();
        let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5);
        assert!(near(t.bounds[0], [-3.5, -2.0, 0.0]) && near(t.bounds[1], [2.0, 2.0, 2.4]), "its discs' box: {:?}", t.bounds);
        // The twig: its one tip, down to the trunk, which carries more.
        let twig = t.branch_at(at(11, 1.5), 2.2);
        assert_eq!((twig, t.branch(twig)), (at(10, 1.5), vec![at(10, 1.5), at(11, 1.5)]));
        assert_eq!(t.branch_tips(twig), [[1.5, 0.0, 2.2]]);
        // Alike twigs meeting: each tip's own, ending where they meet.
        let left = t.branch_at(at(9, -3.0), 1.8);
        assert_eq!((left, t.branch_tips(left)), (at(8, -3.0), vec![[-3.0, 0.0, 1.8]]));
        // The branch's stem carries both: near its foot the branch whole (it joins the trunk,
        // as thick in tips but thinner), near the meeting the twig ending there.
        let branch = t.branch_at(at(6, -2.0), 1.2);
        assert_eq!(branch, at(5, -2.0));
        assert_eq!(t.branch_tips(branch), [[-3.0, 0.0, 1.8], [-1.0, 0.0, 1.8]]);
        assert_eq!(t.branch(branch).len(), 7, "its stem and both twigs");
        assert_eq!(t.branch_at(at(7, -2.0), 1.4), at(8, -3.0), "near the meeting, the first twig there");
        // The trunk carries all four: near its foot the whole tree, near where the twig joins
        // above, the twig; never the branch joining under the pointer.
        assert_eq!(t.branch_at(at(2, 0.0), 0.4), at(1, 0.0));
        assert_eq!(t.branch_tips(at(1, 0.0)).len(), 4);
        assert_eq!(t.branch_at(at(8, 0.0), 1.6), at(10, 1.5));
        assert_eq!(t.branch_at(at(5, 0.0), 1.0), at(1, 0.0));
        // Found again by its tips; not when no branch carries exactly them.
        for low in [twig, left, branch, at(1, 0.0)] {
            assert_eq!(t.branch_of(&t.branch_tips(low)), Some(low), "the branch at disc {low}");
        }
        assert_eq!(t.branch_of(&[[1.5, 0.0, 2.2], [-1.0, 0.0, 1.8]]), None, "tips no branch carries alone");
        assert_eq!(t.branch_of(&[[0.0, 0.0, 2.4]]), None, "the trunk's tip alone");
        assert_eq!(t.branch_of(&[[9.0, 0.0, 1.0]]), None, "a tip of no tree here");
    }

    /// A live preview's bead from `a` to `b` (its part's frame) on the layer at print z `z`, of
    /// `role`, of the plate's first part.
    fn bead(a: [f32; 2], b: [f32; 2], z: f32, role: f32) -> [f32; PREVIEW_SLOTS] {
        Bead { a: [a[0], a[1], z], b: [b[0], b[1], z], dur: 1.0, width: 0.4, height: 0.2, value: 30.0, role, tag: 2.0, filament: 1.0 + 256.0 * (127.0 + 256.0 * 127.0), ..Bead::default() }.pack()
    }

    /// A lit branch is its own support beads, at their place on the plate, a little wider and
    /// taller, in the overlay colour: not the part's beads, nor another branch's, nor a bead on
    /// another layer. The hovered branch lights what the picked one does not.
    #[test]
    fn a_branch_lights_its_own_support_beads() {
        let (t, at) = tree();
        let mut preview = crate::print::follow::tests::preview_of(1);
        let p = std::sync::Arc::get_mut(&mut preview).expect("its own preview");
        // The part stands 10 mm along x: its beads are in its frame.
        p.part_offsets = vec![[10.0, 0.0]].into();
        let support = SUPPORT_ROLES[0] as f32;
        let beads = [
            bead([-10.2, 0.0], [-9.8, 0.0], 2.0, support), // the trunk on layer 10
            bead([-8.7, 0.0], [-8.3, 0.0], 2.0, support),  // the twig on layer 10
            bead([-8.7, 0.0], [-8.3, 0.0], 2.0, 1.0),      // a wall there
            bead([-8.7, 0.0], [-8.3, 0.0], 2.6, support),  // layer 13: no disc
        ];
        p.chunks = vec![crate::print::LiveChunk { beads: RetainedInstances::new(PREVIEW_SLOTS, beads.concat().into()).unwrap(), part: 0, runs: std::sync::Arc::from([]) }];
        let twig = at(10, 1.5);
        let mut data = Vec::new();
        branch_beads(p, &t, &t.branch(twig), [PICKED_COLOUR, PICKED_TAG], &mut data);
        assert_eq!(data.len(), PREVIEW_SLOTS, "the twig's one support bead: {data:?}");
        let (lit, printed) = (Bead::of(&data), Bead::of(&beads[1]));
        assert!((lit.a[0] - 1.3).abs() < 1e-5 && (lit.b[0] - 1.7).abs() < 1e-5, "on the plate: {lit:?}");
        assert!((lit.a[2] - (2.0 - printed.height / 2.0)).abs() < 1e-6 && (lit.b[2] - lit.a[2]).abs() < 1e-6, "hung as the printed bead is, by half its height: {lit:?}");
        // the section's f16s: within 2^-10 of each
        assert!((lit.width - printed.width * LIT_WIDER).abs() < printed.width * LIT_WIDER / 1024.0 && (lit.height - printed.height * LIT_TALLER).abs() < printed.height * LIT_TALLER / 1024.0, "wider and taller than the printed bead's section: {lit:?}");
        assert_eq!([lit.role, lit.tag, lit.kind], [PICKED_COLOUR, PICKED_TAG, 3.0], "the overlay kind, orange, the picked one's");
        assert_eq!(lit.filament, printed.filament, "its joints, as the bead's");
        let trees = [t];
        let lit = lit_beads(p, [Some((&trees[0], at(1, 0.0))), Some((&trees[0], twig))]);
        let colours: Vec<[f32; 2]> = lit.data().chunks_exact(PREVIEW_SLOTS).map(|b| { let b = Bead::of(b); [b.role, b.tag] }).collect();
        assert_eq!(colours, [[HOVERED_COLOUR, HOVERED_TAG], [PICKED_COLOUR, PICKED_TAG]], "the hovered tree's trunk bead, the picked twig's bead once");
    }
}
