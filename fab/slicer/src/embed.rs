//! Items printed into a part (`insert(.., embed: true)`): a nut, a magnet or a board goes into
//! the part while the print pauses, and the layers over it close it in.
//!
//! - The part's regions lose each item's cavity: at a plane, what of the item passes through its
//!   layer as it is lowered in from above, grown by its clearance and [`MARGIN`]. That is all of
//!   the item up to the layer's top, seen from above: its shape swept up. A cavity only widens
//!   going up, so the part has no overhang inside it; the layer after the pause bridges it.
//! - A plane is cut while it lies below the item's cover ([`Embed::cover`]): its top plus the
//!   larger of its clearance and half the thickest layer. The first layer left whole has its
//!   plane at or above the cover, so its bottom is at or above the item's top: the item never
//!   stands above the next layer the nozzle prints. That is the layer the print pauses before.
//! - A plane is cut from where its layer could reach the item's bottom (half the thickest layer
//!   below the plane), so the item rests on a whole layer.
//!
//! An item's cavity is made once, at its first cut, as samples at most [`STEP`] mm apart from its
//! bottom up: each one the sample below, the item's section there and its faces in between as
//! seen from above, which is exactly what of the item lies between its bottom and the sample. A
//! plane takes the first sample at or over the highest point its layer reaches, so its cavity is
//! never smaller than what passes through it. Cost: once per item, its triangles times the
//! samples each spans plus one union per sample; then per plane that crosses an item, one
//! difference.

use crate::clip::{difference_ex, union, FillRule};
use crate::geom::*;
use crate::mesh::Mesh;
use crate::offset::{offset_ex, Join};
use crate::paint::{LayerPaint, PaintParams};
use crate::slice::{SliceIndex, SliceParams};
use crate::source::{ContourSource, PaintAt, SettingsZone, SlopeProfile, SupportPaint};
use std::sync::{Arc, OnceLock};

/// The largest step (mm) between an item's samples.
pub const STEP: f64 = 0.05;
/// What a cavity keeps past the item's clearance (mm): a wall bead whose edge touches the cavity
/// stays clear of the item by more than the clearance, after the walls simplify their beads
/// (Arachne's 0.0125 mm, which cuts the cavity's rounded corners) and the cavity's own chords.
pub const MARGIN: f64 = 0.02;
/// The round join's largest chord deviation (mm), under [`MARGIN`]: the grown cavity keeps the
/// full clearance.
const CHORD: f64 = 0.002;
/// How near a plane may come to an item's cover or bottom and count as on it (mm): a plane at
/// the cover is the first left whole, whatever the rounding of the sums that made either (an
/// item's points may come from single precision). The pause comes before the same layer
/// (`makepad_fab_toolpath::merge_named`).
pub const ON: f64 = 1e-4;

/// An item printed into a part, in the part source's frame (mm).
#[derive(Clone, Debug, PartialEq)]
pub struct Embed {
    pub name: String,
    /// Its box.
    pub min: [f64; 3],
    pub max: [f64; 3],
    /// Sideways and above, how far the cavity keeps from it.
    pub clearance: f64,
    /// The planes below this height are cut ([`Embedded`]): the first layer whose plane is at or
    /// above it is the first over the item.
    pub cover: f64,
}

/// An item to print into a part: its mesh in the part source's frame.
pub struct Item {
    pub name: String,
    pub mesh: Mesh,
    /// Millimetres.
    pub clearance: f64,
}

struct Cavity {
    embed: Embed,
    mesh: Mesh,
    /// How far over and under a plane its layer reaches: half the thickest layer.
    reach: f64,
    drops: OnceLock<Drops>,
}

/// An item's samples from its bottom up and, per sample, its cavity up to there.
struct Drops {
    /// Each sample, then the item's top.
    ends: Vec<f64>,
    /// Per end, all of the item under it seen from above, grown.
    regions: Vec<Vec<ExPolygon>>,
}

impl Cavity {
    fn new(item: Item, thickest: f64) -> Option<Cavity> {
        let (min, max) = item.mesh.bbox();
        if item.mesh.tris.is_empty() || !(min[2] < max[2]) || !item.clearance.is_finite() {
            return None;
        }
        let reach = 0.5 * thickest.max(0.0);
        let clearance = item.clearance.max(0.0);
        let cover = max[2] + clearance.max(reach);
        Some(Cavity { embed: Embed { name: item.name, min, max, clearance, cover }, mesh: item.mesh, reach, drops: OnceLock::new() })
    }

    /// Whether the layer of plane `z` is cut.
    fn cuts(&self, z: f64) -> bool {
        z < self.embed.cover - ON && z + self.reach > self.embed.min[2] + ON
    }

    /// The cavity at plane `z`.
    fn at(&self, z: f64) -> &[ExPolygon] {
        let d = self.drops.get_or_init(|| self.build());
        let i = d.ends.partition_point(|&e| e < z + self.reach).min(d.ends.len() - 1);
        &d.regions[i]
    }

    fn build(&self) -> Drops {
        let (bottom, top) = (self.embed.min[2], self.embed.max[2]);
        let mut heights: Vec<f64> = self.mesh.verts.iter().map(|v| v[2]).collect();
        heights.sort_by(f64::total_cmp);
        heights.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        // Samples between the vertex heights, never on one: no face lies in a sample's plane.
        let mut samples = Vec::new();
        for w in heights.windows(2) {
            let k = ((w[1] - w[0]) / STEP).ceil().max(1.0) as usize;
            samples.extend((0..k).map(|j| w[0] + (j as f64 + 0.5) * (w[1] - w[0]) / k as f64));
        }
        // Slab i runs from levels[i] to ends[i].
        let mut levels = Vec::with_capacity(samples.len() + 1);
        levels.push(bottom);
        levels.extend_from_slice(&samples);
        let mut ends = samples.clone();
        ends.push(top);
        let sections = SliceIndex::new(&self.mesh).cut(&self.mesh, &samples, &SliceParams { resolution: 0.0, ..SliceParams::default() });
        // The faces in each slab as seen from above. A vertical face shows nothing; the sections
        // hold its edge.
        let mut faces: Vec<Vec<Polygon>> = vec![Vec::new(); levels.len()];
        for t in &self.mesh.tris {
            let p = t.map(|i| self.mesh.verts[i as usize]);
            let shown = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if shown.abs() < 1e-12 {
                continue;
            }
            let (z0, z1) = (p[0][2].min(p[1][2]).min(p[2][2]), p[0][2].max(p[1][2]).max(p[2][2]));
            let first = levels.partition_point(|&l| l <= z0).saturating_sub(1);
            for i in first..levels.len() {
                let (a, b) = (levels[i], ends[i]);
                if a > z1 {
                    break;
                }
                if let Some(face) = seen_from_above(&p, a, b) {
                    faces[i].push(face);
                }
            }
        }
        let grow = (self.embed.clearance + MARGIN) * SCALE;
        let mut regions = Vec::with_capacity(ends.len());
        let mut below: Vec<ExPolygon> = Vec::new();
        for (i, slab) in faces.iter_mut().enumerate() {
            let mut rings = to_polygons(&below);
            if let Some(section) = sections.get(i) {
                rings.extend(to_polygons(section));
            }
            rings.append(slab);
            below = union(&rings, FillRule::NonZero);
            regions.push(offset_ex(&below, grow, Join::Round(CHORD * SCALE)));
        }
        Drops { ends, regions }
    }
}

/// Triangle `p` between heights `a` and `b`, as seen from above: a counter-clockwise ring, or
/// None when nothing of it shows there.
fn seen_from_above(p: &[[f64; 3]; 3], a: f64, b: f64) -> Option<Polygon> {
    let mut ring: Vec<[f64; 3]> = p.to_vec();
    for (bound, keep_above) in [(a, true), (b, false)] {
        let inside = |q: &[f64; 3]| if keep_above { q[2] >= bound } else { q[2] <= bound };
        let mut out = Vec::with_capacity(ring.len() + 2);
        for k in 0..ring.len() {
            let (s, e) = (ring[k], ring[(k + 1) % ring.len()]);
            if inside(&s) {
                out.push(s);
            }
            if inside(&s) != inside(&e) {
                let t = (bound - s[2]) / (e[2] - s[2]);
                out.push([s[0] + t * (e[0] - s[0]), s[1] + t * (e[1] - s[1]), bound]);
            }
        }
        ring = out;
        if ring.len() < 3 {
            return None;
        }
    }
    let mut poly = Polygon::new(ring.iter().map(|q| Pt::from_mm(q[0], q[1])).collect());
    poly.pts.dedup();
    if poly.pts.len() < 3 || poly.area2() == 0 {
        return None;
    }
    if !poly.is_ccw() {
        poly.reverse();
    }
    Some(poly)
}

/// A part with items printed into it: its regions less each item's cavity. Everything else is the
/// part's own.
pub struct Embedded {
    pub inner: Arc<dyn ContourSource + Send + Sync>,
    cavities: Vec<Cavity>,
}

impl Embedded {
    /// `inner` with `items` cut into it. `thickest` is the thickest layer the part prints, its
    /// first included (mm). An item with no height cuts nothing.
    pub fn new(inner: Arc<dyn ContourSource + Send + Sync>, items: Vec<Item>, thickest: f64) -> Embedded {
        Embedded { inner, cavities: items.into_iter().filter_map(|i| Cavity::new(i, thickest)).collect() }
    }

    /// The cavities the layer of plane `z` loses.
    fn own_cavity(&self, z: f64) -> Vec<ExPolygon> {
        self.cavities.iter().filter(|c| c.cuts(z)).flat_map(|c| c.at(z).iter().cloned()).collect()
    }
}

impl ContourSource for Embedded {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.inner.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.inner.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        let mut out = self.inner.contours(zs);
        for (&z, regions) in zs.iter().zip(out.iter_mut()) {
            if regions.is_empty() {
                continue;
            }
            let cavity = self.own_cavity(z);
            if !cavity.is_empty() {
                *regions = difference_ex(regions, &cavity);
            }
        }
        out
    }
    fn painted(&self) -> bool {
        self.inner.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &PaintParams) -> Vec<LayerPaint> {
        self.inner.paint_regions(zs, slices, p)
    }
    fn paint_at(&self) -> Option<PaintAt> {
        self.inner.paint_at()
    }
    fn support_paint(&self) -> Option<SupportPaint> {
        self.inner.support_paint()
    }
    fn settings_zones(&self) -> Vec<SettingsZone> {
        self.inner.settings_zones()
    }
    fn np_plan(&self) -> Option<Arc<crate::nonplanar::NpPlan>> {
        self.inner.np_plan()
    }
    fn layer_stack(&self) -> Option<Arc<[(f64, f64)]>> {
        self.inner.layer_stack()
    }
    fn slope_profile(&self, step: f64) -> Option<SlopeProfile> {
        self.inner.slope_profile(step)
    }
    fn embeds(&self) -> Vec<Embed> {
        self.cavities.iter().map(|c| c.embed.clone()).chain(self.inner.embeds()).collect()
    }
    fn cavity(&self, z: f64) -> Vec<ExPolygon> {
        let mut out = self.own_cavity(z);
        out.extend(self.inner.cavity(z));
        out
    }
}

#[cfg(test)]
#[path = "embed_tests.rs"]
mod embed_tests;
