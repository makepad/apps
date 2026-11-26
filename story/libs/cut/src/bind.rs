//! A `Graphic`'s `bind` (reports/M4-demo-overlays.md): the app run an
//! overlay follows, and where that run is in the cut's picture at a time.
//!
//! `bind: @demo` names the `Clip` that plays the run (recommended: when the
//! clip is updated to a new render, §6.5, the overlay follows it with no
//! edit of its own); `bind: run(id, rev, render_id, render_rev)` names the
//! run itself, and the clip is whichever plays that run at the time.
//!
//! The placement is read off the frame plan, so it is exactly what the
//! renderer draws: the clip's fit, every Reframe crop after it, compound
//! clips' placements, all composed into one source → output mapping.

use crate::eval::{fit_rects, DrawKind, CutEval, FramePlan, Slot};
use crate::model::*;
use crate::reframe::Rect;
use makepad_motion::bind::place::BindFrame;
use makepad_motion::bind::run::{Rect as RunRect, RunData};
use makepad_motion::bind::RunBinding;

/// What a Graphic's `bind` names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BindTarget {
    /// The id of a `Clip` whose `src` is a `run(…)`.
    Clip(String),
    /// A run and render, followed on whichever clip plays that run.
    Run(RunRef),
}

/// Where a bound run is in the output at one time.
#[derive(Clone, Debug, PartialEq)]
pub struct BindPlace {
    /// The clip playing it (a compound clip's inner clip is `outer/inner`).
    pub clip: String,
    /// The render that clip plays (its run.json is the overlay's data).
    pub run: RunRef,
    /// Seconds of the recording the clip shows.
    pub run_time: f64,
    /// The part of the recording that shows, in source pixels.
    pub crop: Rect,
    /// Where it lands, in output pixels.
    pub dst: Rect,
}

/// Per axis, `out = in * scale + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Map {
    sx: f64,
    ox: f64,
    sy: f64,
    oy: f64,
}

impl Map {
    fn between(from: &Rect, to: &Rect) -> Option<Map> {
        if !(from.w > 0.0 && from.h > 0.0) {
            return None;
        }
        let (sx, sy) = (to.w / from.w, to.h / from.h);
        Some(Map { sx, ox: to.x - from.x * sx, sy, oy: to.y - from.y * sy })
    }

    fn then(&self, next: &Map) -> Map {
        Map { sx: self.sx * next.sx, ox: self.ox * next.sx + next.ox, sy: self.sy * next.sy, oy: self.oy * next.sy + next.oy }
    }

    fn rect(&self, r: &Rect) -> Rect {
        Rect::new(r.x * self.sx + self.ox, r.y * self.sy + self.oy, r.w * self.sx, r.h * self.sy)
    }

    fn inverse_rect(&self, r: &Rect) -> Rect {
        Rect::new((r.x - self.ox) / self.sx, (r.y - self.oy) / self.sy, r.w / self.sx, r.h / self.sy)
    }
}

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// The ops that read `slot`, with the mapping from `slot`'s texture into
/// theirs and the part of `slot` they show (in `slot`'s pixels).
fn readers(plan: &FramePlan, slot: Slot) -> Vec<(Slot, Map, Rect)> {
    let size = plan.ops[slot].size;
    let whole = Rect::new(0.0, 0.0, size.0 as f64, size.1 as f64);
    let same = |to: (u32, u32)| {
        let target = Rect::new(0.0, 0.0, to.0 as f64, to.1 as f64);
        Map::between(&whole, &target)
    };
    let mut out = Vec::new();
    for (j, op) in plan.ops.iter().enumerate().skip(slot + 1) {
        let map = match &op.kind {
            DrawKind::Reframe { input, crop, dst_rect } if *input == slot => Map::between(crop, dst_rect).map(|m| (m, *crop)),
            DrawKind::Effect { input: Some(input), .. } if *input == slot => same(op.size).map(|m| (m, whole)),
            DrawKind::Transition { a, b, .. } if *a == Some(slot) || *b == Some(slot) => same(op.size).map(|m| (m, whole)),
            DrawKind::Stack { layers } if layers.iter().any(|(s, _)| *s == slot) => same(op.size).map(|m| (m, whole)),
            _ => None,
        };
        if let Some((map, shown)) = map {
            out.push((j, map, shown));
        }
    }
    out
}

/// From `slot` to the plan's result: the composed mapping and the part of
/// the source still showing, depth first (the first path that reaches the
/// output wins, as the first drawn layer would).
fn to_result(plan: &FramePlan, slot: Slot, map: Map, visible: Rect, depth: usize) -> Option<(Map, Rect)> {
    let size = plan.ops[slot].size;
    let visible = intersect(&visible, &map.inverse_rect(&Rect::new(0.0, 0.0, size.0 as f64, size.1 as f64)))?;
    if Some(slot) == plan.result {
        return Some((map, visible));
    }
    if depth > 64 {
        return None;
    }
    for (next, step, shown) in readers(plan, slot) {
        let Some(visible) = intersect(&visible, &map.inverse_rect(&shown)) else { continue };
        if let Some(found) = to_result(plan, next, map.then(&step), visible, depth + 1) {
            return Some(found);
        }
    }
    None
}

impl FramePlan {
    /// Map a layer's texture through the cut's composition into output pixels.
    /// The returned clip is the visible part in output pixels.
    pub fn placement(&self, slot: Slot) -> Option<([f64; 4], Rect)> {
        let op = self.ops.get(slot)?;
        let whole = Rect::new(0.0, 0.0, op.size.0 as f64, op.size.1 as f64);
        let (map, visible) = to_result(self, slot, Map { sx: 1.0, ox: 0.0, sy: 1.0, oy: 0.0 }, whole, 0)?;
        Some(([map.sx, map.sy, map.ox, map.oy], map.rect(&visible)))
    }
}

impl CutEval<'_> {
    /// Where a bound run shows in `plan` (this evaluator's plan at
    /// `plan.t`): the clip it follows, the run time and the source →
    /// output placement. `None` while no clip plays it, while the clip's
    /// source size or marker is unresolved, or when the clip does not
    /// reach the output (hidden under a transition's other side still
    /// counts: it is drawn).
    pub fn bind_place(&self, plan: &FramePlan, target: &BindTarget) -> Option<BindPlace> {
        let candidates: Vec<(Slot, &RunRef)> = plan
            .ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match &op.kind {
                DrawKind::Source { media: MediaRef::Run(run), still: false, .. } => {
                    let matches = match target {
                        BindTarget::Clip(id) => op.node == *id,
                        BindTarget::Run(want) => run.id == want.id,
                    };
                    matches.then_some((i, run))
                }
                _ => None,
            })
            .collect();
        // An exact render first (a `run()` bind pins one).
        let exact = |(_, run): &&(Slot, &RunRef)| matches!(target, BindTarget::Run(want) if want == *run);
        let ordered = candidates.iter().filter(exact).chain(candidates.iter().filter(|c| !exact(c)));
        for &(slot, run) in ordered {
            let DrawKind::Source { source_time: Some(run_time), src_rect: Some(src), dst_rect: Some(dst), .. } = &plan.ops[slot].kind else { continue };
            let Some(map) = Map::between(src, dst) else { continue };
            if let Some((map, visible)) = to_result(plan, slot, map, *src, 0) {
                return Some(BindPlace { clip: plan.ops[slot].node.clone(), run: run.clone(), run_time: *run_time, crop: visible, dst: map.rect(&visible) });
            }
        }
        None
    }
}

/// Problems with the Graphics' binds that loading cannot see (a clip named
/// before it is defined): a `bind: @clip` that names no Clip, or a Clip
/// that plays no app run. One message per problem, naming the graphic.
pub fn check(doc: &CutDoc) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for graphic in &doc.graphics {
        let Some(BindTarget::Clip(clip)) = &graphic.bind else { continue };
        match doc.node(clip).map(|node| &node.kind) {
            Some(CutNodeKind::Clip { src: MediaRef::Run(_), .. }) => {}
            Some(CutNodeKind::Clip { .. }) => out.push((graphic.id.clone(), format!("`{}.bind` names `{clip}`, a Clip that plays no app run", graphic.id))),
            Some(_) => out.push((graphic.id.clone(), format!("`{}.bind` names `{clip}`, which is not a Clip", graphic.id))),
            None => {
                let clips: Vec<&str> = doc.nodes.iter().filter(|n| matches!(n.kind, CutNodeKind::Clip { src: MediaRef::Run(_), .. })).map(|n| n.id.as_str()).collect();
                out.push((graphic.id.clone(), format!("`{}.bind` names `{clip}`, which is not in the cut; app-run clips: {}", graphic.id, if clips.is_empty() { "none".into() } else { clips.join(", ") })));
            }
        }
    }
    out
}

impl CutEval<'_> {
    /// A bound graphic's run placement along its own clock, sampled at
    /// `fps` over its span: where the run is (its time, the recording's
    /// part that shows and where, mapped into the document's `doc_size`
    /// units through the graphic's `fit`) at each document time `k / fps`.
    /// Frames where no clip plays the run hold the nearest placement.
    /// `data` is the run's `run.json` (of the render the clip plays).
    pub fn run_binding(&self, graphic: &crate::graphic::GraphicNode, doc_size: [f64; 2], data: std::sync::Arc<RunData>, fps: f64) -> Option<RunBinding> {
        let target = graphic.bind.as_ref()?;
        let fps = if fps > 0.0 { fps } else { 30.0 };
        let (start, length) = match graphic.span {
            Some(span) => (span.start, span.length),
            None => (0.0, self.doc().duration),
        };
        let output = (self.doc().output.width, self.doc().output.height);
        let doc_px = (doc_size[0].round().max(1.0) as u32, doc_size[1].round().max(1.0) as u32);
        // The document drawn into the output: its `src` part lands on `dst`.
        let (doc_part, out_part) = fit_rects(doc_px, output, graphic.fit);
        let to_doc = |r: &Rect| RunRect::new(r.x, r.y, r.w, r.h);
        let count = ((length * fps).ceil() as usize).max(1) + 1;
        let mut frames: Vec<Option<BindFrame>> = Vec::with_capacity(count);
        let mut name = String::new();
        for k in 0..count {
            let local = (k as f64 / fps).min(length);
            let t = start + local;
            // The span is half open: sample its last instant just inside.
            let t = if k + 1 == count { t - 1e-6 } else { t };
            let plan = self.frame_plan(t);
            frames.push(self.bind_place(&plan, target).map(|place| {
                if name.is_empty() {
                    name = place.clip.clone();
                }
                let frame = BindFrame { run_time: place.run_time, crop: to_doc(&place.crop), dst: to_doc(&place.dst) };
                frame.then(&to_doc(&out_part), &to_doc(&doc_part))
            }));
        }
        let first = frames.iter().position(Option::is_some)?;
        // Hold the nearest known placement where none was found.
        let mut last = frames[first]?;
        let frames: Vec<BindFrame> = frames
            .into_iter()
            .map(|frame| {
                if let Some(frame) = frame {
                    last = frame;
                }
                last
            })
            .collect();
        if let BindTarget::Clip(clip) = target {
            name = clip.clone();
        }
        Some(RunBinding { name, data, start: 0.0, rate: fps, frames })
    }
}

/// Everything a renderer needs to bind graphic `graphic` of `doc`: the run
/// context its document reads, placed from the cut alone. `runs` are the
/// recorded runs by the render the clips pin (each render's `run.json`);
/// their recording sizes and marks stand in for the clips' source sizes
/// and `marker()` cut points. `None` when the graphic has no `bind` or the
/// run is not given. `doc_size` is the Motion document's `size`, `fps` the
/// rate its placement is sampled at (the cut's).
pub fn graphic_runs(
    doc: &CutDoc,
    graphic: &str,
    doc_size: [f64; 2],
    runs: &std::collections::HashMap<RunRef, std::sync::Arc<RunData>>,
    fps: f64,
) -> Option<makepad_motion::bind::RunContext> {
    let graphic = doc.graphic(graphic)?;
    let target = graphic.bind.as_ref()?;
    let run = match target {
        BindTarget::Run(run) => run.clone(),
        BindTarget::Clip(clip) => match &doc.node(clip)?.kind {
            CutNodeKind::Clip { src: MediaRef::Run(run), .. } => run.clone(),
            _ => return None,
        },
    };
    // The pinned render's data, or (a `run()` bind to another render of
    // the same run) any render of that run.
    let data = runs.get(&run).or_else(|| runs.iter().find(|(r, _)| r.id == run.id).map(|(_, d)| d))?.clone();
    let mut eval = CutEval::new(doc);
    for (r, d) in runs {
        eval.set_source_size(MediaRef::Run(r.clone()), d.size.0.round() as u32, d.size.1.round() as u32);
        eval.set_marks(r.clone(), d.mark_seconds().into_iter().collect());
    }
    let binding = eval.run_binding(graphic, doc_size, data, fps)?;
    Some(makepad_motion::bind::RunContext { runs: vec![binding] })
}
