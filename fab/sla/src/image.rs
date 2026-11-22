//! The plate's layers: each layer's cross-section of every part on the screen's pixels, as
//! spans. One layer on demand ([`LayerStack::layer`]) for the view, or every layer in parallel
//! ([`LayerStack::map`]) for the export and the checks.

use crate::grid::{Layers, PixelGrid, PixelRect};
use crate::printer::ResinProfile;
use crate::section::Section;
use crate::spans::Spans;
use std::sync::Arc;

/// One layer: the pixels its cross-section covers.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub layer: u32,
    /// The height (mm above the plate) it is cut at.
    pub z: f64,
    pub spans: Spans,
}

impl Layer {
    /// The area it cures (mm²): each pixel's area by its covered share.
    pub fn area(&self, grid: &PixelGrid) -> f64 {
        self.spans.covered() * grid.pixel_area()
    }
}

/// The plate's parts as layers on one printer.
#[derive(Clone)]
pub struct LayerStack {
    pub sections: Vec<Arc<dyn Section>>,
    pub grid: PixelGrid,
    pub layers: Layers,
}

impl LayerStack {
    /// `sections` on `profile`'s screen, layers up to the tallest.
    pub fn new(sections: Vec<Arc<dyn Section>>, profile: &ResinProfile) -> LayerStack {
        let top = sections.iter().map(|s| s.bounds().1[2]).fold(0.0f64, f64::max);
        LayerStack { sections, grid: PixelGrid::of(&profile.screen), layers: Layers::up_to(top, profile.layer_height) }
    }

    /// The sections spanning height `z`, gathered into groups whose footprints overlap: each
    /// group's pixels and its sections. Apart, the groups' pixels never meet.
    pub fn groups_at(&self, z: f64) -> Vec<(PixelRect, Vec<usize>)> {
        let mut groups: Vec<(PixelRect, Vec<usize>)> = Vec::new();
        for (i, s) in self.sections.iter().enumerate() {
            let b = s.bounds();
            let mut rect = self.grid.rect_of([b.0[0], b.0[1]], [b.1[0], b.1[1]]);
            if !(b.0[2]..=b.1[2]).contains(&z) || rect.is_empty() {
                continue;
            }
            let mut members = vec![i];
            // a group it meets joins it, and the larger one may meet others
            while let Some(g) = groups.iter().position(|g| !g.0.intersect(&rect).is_empty()) {
                let (r, m) = groups.swap_remove(g);
                rect = rect.union(&r);
                members.extend(m);
            }
            groups.push((rect, members));
        }
        groups
    }

    /// The pixels any of its layers can light: every section's footprint, together.
    pub fn footprint(&self) -> PixelRect {
        self.sections.iter().fold(PixelRect::EMPTY, |r, s| {
            let b = s.bounds();
            r.union(&self.grid.rect_of([b.0[0], b.0[1]], [b.1[0], b.1[1]]))
        })
    }

    /// Layer `k`.
    pub fn layer(&self, k: u32) -> Layer {
        let z = self.layers.z(k);
        let parts = self
            .groups_at(z)
            .into_iter()
            .map(|(rect, members)| {
                let mut cov = vec![0u8; rect.pixels()];
                for i in members {
                    self.sections[i].cover(z, &self.grid, rect, &mut cov);
                }
                Spans::of(rect, &cov)
            })
            .collect();
        Layer { layer: k, z, spans: Spans::join(parts) }
    }

    /// `f` of every layer, in layer order, the layers made in parallel.
    pub fn map<T: Send>(&self, f: impl Fn(Layer) -> T + Sync) -> Vec<T> {
        makepad_fab_slicer::par::map(self.layers.count as usize, |k| f(self.layer(k as u32)))
    }
}
