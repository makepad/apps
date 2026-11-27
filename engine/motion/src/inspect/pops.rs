//! The continuity check: a layer that appears or vanishes in one frame,
//! with no entrance or exit, is a hard pop (`layer.pop_in`,
//! `layer.pop_out`). What shows of a layer is its ink's area on the canvas
//! weighted by its alpha ([`super::Presence`]), so a fade, a scale from
//! small, a draw-on, letters typed one by one or a slide in from beyond
//! the frame's edge all grow it over frames; a pop is a first (or last)
//! frame that already carries most of what the layer settles at.
//!
//! The document's first and last frames are never pops (the cut before and
//! after is the edit's), and a layer marked `hard_cut: true` (or inside one)
//! cuts in and out by intent: a beat hit, a flash.

use crate::plain::GroupProps;
use super::{issue, measure, LayoutIssue, Presence};
use crate::{Layer, LayerKind, MotionDoc};
use std::collections::HashMap;

/// A first (last) frame carrying at least this share of the layer's
/// settled weight is a pop.
pub const POP_SHARE: f64 = 0.75;
/// Seconds after an appearance (before a disappearance) over which the
/// layer's settled weight is taken: an entrance's usual length.
pub const POP_SETTLE: f64 = 0.4;
/// Layers whose settled weight is under this share of the frame's area
/// (sparks, particles, dots) are not checked.
pub const POP_MIN_AREA: f64 = 0.0003;

/// The pops of `doc`, from its frames at the inspect sample times (each
/// with what shows of each layer); the frames around each change are
/// evaluated on the document's frame grid to find the exact frame.
pub(super) fn pops(doc: &MotionDoc, samples: &[(f64, Presence)]) -> Vec<LayoutIssue> {
    Pops::new(doc, samples).run()
}

/// The pops of `doc` at `times` alone (see [`super::layout_checks`], which
/// runs this check with its own).
pub fn pop_checks(doc: &MotionDoc, times: &[f64]) -> Vec<LayoutIssue> {
    let mut times: Vec<f64> = times.iter().copied().filter(|t| t.is_finite() && *t >= 0.0 && *t < doc.duration).collect();
    times.sort_by(f64::total_cmp);
    times.dedup();
    let samples: Vec<(f64, Presence)> = times.iter().map(|&t| (t, measure(doc, &doc.eval(t)))).collect();
    pops(doc, &samples)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Change {
    In,
    Out,
}

/// A pop found: the item path, the change and the frame it shows on (an
/// appearance's first frame, a disappearance's first frame without it).
struct Found {
    path: String,
    change: Change,
    frame: i64,
    /// The share of its settled weight the layer jumps by.
    share: f64,
}

struct Pops<'a> {
    doc: &'a MotionDoc,
    samples: &'a [(f64, Presence)],
    fps: f64,
    frames: i64,
    area: f64,
    /// Frames evaluated on the grid, by index.
    grid: HashMap<i64, Presence>,
    /// Declared layers: whether each is a hard cut, and where its clock
    /// starts on the document's.
    hard: HashMap<String, bool>,
    starts: HashMap<String, f64>,
    names: HashMap<String, String>,
}

/// `layers[0].layer.layers[1]#3` → (`layers[0].layer.layers[1]`, `#3`).
fn split_copy(path: &str) -> (&str, &str) {
    match path.find('#') {
        Some(at) => (&path[..at], &path[at..]),
        None => (path, ""),
    }
}

/// The declared paths `path` sits inside, nearest first, itself included.
fn lineage(path: &str) -> impl Iterator<Item = &str> {
    let base = split_copy(path).0;
    std::iter::once(base).chain(base.char_indices().rev().filter(|(_, c)| *c == '.').map(move |(i, _)| &base[..i]))
}

fn clocks(layers: &[Layer], origin: f64, out: &mut HashMap<String, f64>) {
    for layer in layers {
        let start = origin + layer.span.as_ref().map_or(0.0, |span| span.start.value);
        out.insert(layer.path.clone(), start);
        match &layer.kind {
            LayerKind::Group { layers, mask, props: GroupProps { stagger, .. }, .. } => {
                for (i, child) in layers.iter().enumerate() {
                    clocks(std::slice::from_ref(child), start + stagger.delay(i, layers.len()), out);
                }
                if let Some(mask) = mask {
                    clocks(std::slice::from_ref(mask), start, out);
                }
            }
            LayerKind::Repeat { layer, .. } | LayerKind::Anchored { layer, .. } => clocks(std::slice::from_ref(layer), start, out),
            LayerKind::FlyThrough { layers, .. } => clocks(layers, start, out),
            LayerKind::Bones(rig) => clocks(&rig.layers, start, out),
            _ => {}
        }
    }
}

impl<'a> Pops<'a> {
    fn new(doc: &'a MotionDoc, samples: &'a [(f64, Presence)]) -> Self {
        let fps = if doc.fps > 0.0 { doc.fps } else { 30.0 };
        let mut starts = HashMap::new();
        clocks(&doc.layers, 0.0, &mut starts);
        let layers = doc.all_layers();
        Pops {
            doc,
            samples,
            fps,
            frames: (doc.duration * fps - 1e-9).ceil().max(1.0) as i64,
            area: (doc.size[0] * doc.size[1]).max(1.0),
            grid: HashMap::new(),
            hard: layers.iter().map(|l| (l.path.clone(), l.hard_cut)).collect(),
            starts,
            names: layers.iter().filter_map(|l| Some((l.path.clone(), l.name.clone()?))).collect(),
        }
    }

    fn present(&self, weight: f64) -> bool {
        weight > self.area * 1e-6
    }

    /// What shows of `path` on grid frame `k`.
    fn at(&mut self, k: i64, path: &str) -> f64 {
        let (doc, fps) = (self.doc, self.fps);
        let frame = self.grid.entry(k).or_insert_with(|| measure(doc, &doc.eval(k as f64 / fps)));
        frame.weight.get(path).copied().unwrap_or(0.0)
    }

    fn unchecked(&mut self, k: i64, path: &str) -> bool {
        let _ = self.at(k, path);
        let frame = &self.grid[&k];
        lineage(path).any(|p| frame.unchecked.iter().any(|u| split_copy(u).0 == p))
    }

    /// The most the layer shows over [`POP_SETTLE`] from frame `k` on
    /// (`dir` 1) or up to it (`dir` -1): frames near `k`, then the samples.
    fn settled(&mut self, k: i64, dir: i64, path: &str) -> f64 {
        let span = (POP_SETTLE * self.fps).round().max(1.0) as i64;
        let mut best = self.at(k, path);
        for step in [1, 2, 4, span / 2, span] {
            let j = k + dir * step;
            if step > 0 && (0..self.frames).contains(&j) {
                best = best.max(self.at(j, path));
            }
        }
        let (t, reach) = (k as f64 / self.fps, POP_SETTLE + 1e-9);
        for (ts, frame) in self.samples {
            let d = (ts - t) * dir as f64;
            if d >= 0.0 && d <= reach {
                best = best.max(frame.weight.get(path).copied().unwrap_or(0.0));
            }
        }
        best
    }

    fn run(mut self) -> Vec<LayoutIssue> {
        let mut paths: Vec<String> = self.samples.iter().flat_map(|(_, frame)| frame.weight.keys().cloned()).collect();
        paths.sort();
        paths.dedup();
        let mut found = Vec::new();
        for path in &paths {
            if lineage(path).any(|p| self.hard.get(p).copied().unwrap_or(false)) {
                continue;
            }
            for i in 1..self.samples.len() {
                let (t0, t1) = (self.samples[i - 1].0, self.samples[i].0);
                let weight = |frame: &Presence| frame.weight.get(path).copied().unwrap_or(0.0);
                let (was, is) = (self.present(weight(&self.samples[i - 1].1)), self.present(weight(&self.samples[i].1)));
                // The grid frames from the one after t0 to the one at or after t1.
                let (lo, hi) = ((t0 * self.fps + 1e-6).floor() as i64 + 1, ((t1 * self.fps - 1e-6).ceil() as i64).min(self.frames - 1));
                if !was && is {
                    let Some(k) = (lo..=hi).find(|k| { let w = self.at(*k, path); self.present(w) }) else { continue };
                    if k == 0 || self.unchecked(k, path) || self.unchecked(k - 1, path) {
                        continue;
                    }
                    let (first, full) = (self.at(k, path), self.settled(k, 1, path));
                    if full >= self.area * POP_MIN_AREA && first >= POP_SHARE * full {
                        found.push(Found { path: path.clone(), change: Change::In, frame: k, share: first / full });
                    }
                } else if was && !is {
                    // The last frame it shows on, before the first without it.
                    let Some(k) = (lo - 1..=hi).rev().find(|k| *k >= 0 && { let w = self.at(*k, path); self.present(w) }) else { continue };
                    let gone = k + 1;
                    if gone >= self.frames {
                        continue;
                    }
                    let after = self.at(gone, path);
                    if self.present(after) || self.unchecked(k, path) || self.unchecked(gone, path) {
                        continue;
                    }
                    let (last, full) = (self.at(k, path), self.settled(k, -1, path));
                    if full >= self.area * POP_MIN_AREA && last >= POP_SHARE * full {
                        found.push(Found { path: path.clone(), change: Change::Out, frame: gone, share: last / full });
                    }
                }
            }
        }
        self.report(found)
    }

    /// One issue per pop: inner layers popping with a parent at the same
    /// frame are the parent's pop; a Repeat's copies popping together are
    /// one, with their count.
    fn report(&self, found: Vec<Found>) -> Vec<LayoutIssue> {
        let outer: Vec<&Found> = found.iter().filter(|f| {
            let (base, copy) = split_copy(&f.path);
            !found.iter().any(|g| {
                let (gb, gc) = split_copy(&g.path);
                g.change == f.change && g.frame == f.frame && gc == copy && base.len() > gb.len() && base.starts_with(gb) && base[gb.len()..].starts_with('.')
            })
        }).collect();
        let mut groups: Vec<(String, Change, i64, usize, f64)> = Vec::new();
        for f in outer {
            let base = split_copy(&f.path).0;
            match groups.iter_mut().find(|g| g.0 == base && g.1 == f.change && g.2 == f.frame) {
                Some(g) => {
                    g.3 += 1;
                    g.4 = g.4.max(f.share);
                }
                None => groups.push((base.to_string(), f.change, f.frame, 1, f.share)),
            }
        }
        groups.sort_by(|a, b| (a.2, &a.0, a.1).cmp(&(b.2, &b.0, b.1)));
        groups.into_iter().map(|(path, change, frame, copies, share)| {
            let t = frame as f64 / self.fps;
            let local = t - self.starts.get(&path).copied().unwrap_or(0.0);
            let who = match (self.names.get(&path), copies) {
                (Some(name), 1) => format!("`{name}`"),
                (Some(name), n) => format!("{n} copies of `{name}`"),
                (None, 1) => "the layer".to_string(),
                (None, n) => format!("{n} copies of the layer"),
            };
            let share = (share.min(1.0) * 100.0).round();
            match change {
                Change::In => issue(t, "layer.pop_in", &path, None,
                    format!("{who} pops in at {t:.3} s (frame {frame}): its first frame already shows {share:.0}% of it, with no entrance"),
                    &format!("give it an entrance over 0.3–0.5 s from {local:.2} s on its clock: fade, scale or slide it in (`opacity: keys([{local:.2}, 0], [{:.2}, 1, @ease_out])`); if the hard cut is intended (a beat hit, a flash), set `hard_cut: true`", local + 0.35)),
                Change::Out => issue(t, "layer.pop_out", &path, None,
                    format!("{who} vanishes at {t:.3} s (frame {frame}): its last frame still shows {share:.0}% of it, with no exit"),
                    &format!("give it an exit over 0.2–0.3 s ending at {local:.2} s on its clock: fade, shrink or slide it out (`opacity: keys([{:.2}, 1], [{local:.2}, 0, @ease_in])`), or `exit: @defocus`; if the hard cut is intended, set `hard_cut: true`", (local - 0.25).max(0.0))),
            }
        }).collect()
    }
}
