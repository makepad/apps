//! Serial work on the platform pool, retaining the latest sheet and reveal requests separately.

use fab_elec_circuit::{SheetHint, SheetHints};
use fab_elec_design::Design;
use fab_elec_explore::{sheet::block_origin, KeepBase, Opts, SheetLayout};
use fab_elec_explore::reveal::{reveal_sheet, RevealSheet};
use makepad_widgets_core::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets_core::{log, Cx};
use std::{collections::HashSet, sync::Arc};
use super::liveix::{sheet_trees, SheetPieces, SheetTrees};
use crate::live::segs::{Prepared, SegRows};

/// What a drag keeps in place during relayout. The frame stays at its world origin and
/// each group head keeps the hint in the source snapshot being laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DragAnchor {
    pub block: u32,
    pub origin: [i32; 2],
    pub heads: Option<Arc<HashSet<String>>>,
}

/// Where a new layout sits in the world. A layout starts at the sheet's own origin; the view
/// moves it by its standing `shift`, or, with `block`, so that block's origin lands where it was:
/// a part dragged left grows its block left while the block's other parts stay put.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keep {
    pub shift: [i32; 2],
    pub block: Option<DragAnchor>,
}

enum Request {
    Layout { design: Arc<Design>, unit: usize, hints: SheetHints, lead: Option<String>, kept: Option<(Arc<KeepBase>, String)>, keep: Keep, gen: u64, opts: Opts },
    Drop { design: Arc<Design>, unit: usize, path: String, heads: Arc<SheetHints>, token: u64, opts: Opts },
    Keep { design: Arc<Design>, unit: usize, base: Arc<KeepBase>, carried: Option<String>, token: u64, opts: Opts },
    Reveal { design: Arc<Design>, unit: usize, sheet: Arc<SheetLayout>, focus: u32, gen: u64 },
}

/// A finished sheet layout, with everything the UI thread would otherwise compute: the wiring's
/// trees prepared for the host's segment rows (`SegRows::prepare`), so registering them only
/// merges, and the pieces the scene draws.
pub struct Laid {
    pub gen: u64,
    pub sheet: SheetLayout,
    /// A Library preview's exact keep hints, or its no-room note when the lead layout was drawn.
    pub kept: Option<KeepHints>,
    pub trees: Prepared,
    pub pieces: SheetPieces,
    /// How far the layout was moved (the view's next standing shift).
    pub shift: [i32; 2],
    /// The layout's own time on the worker, in milliseconds.
    pub ms: f64,
    /// The pane shape it was laid out for (`Opts::aspect`).
    pub aspect: f64,
}

/// A finished reveal with its wiring prepared for the host on the worker.
pub struct Revealed {
    /// Generation of the reveal request.
    pub gen: u64,
    /// The reveal's layout and relations.
    pub reveal: RevealSheet,
    /// Wiring prepared for registration with the host's segment allocator.
    pub trees: Prepared,
    /// Drawn wire pieces indexed like the reveal's runs and entries.
    pub pieces: SheetPieces,
}

/// The hints a released drag writes besides the dragged heads' own (`fab_elec_explore::drag_group_drop`).
pub struct Dropped {
    /// The drag's token (`Drag::dropped`).
    pub token: u64,
    pub drop: Vec<(String, SheetHint)>,
}

/// The hints that keep the drawn sheet, or the note when the added parts have no room.
pub type KeepHints = std::result::Result<Vec<(String, SheetHint)>, String>;

/// A released edit's keep result, paired with its evaluated preview.
pub struct Kept {
    /// The edit's preview tag.
    pub token: u64,
    pub hints: KeepHints,
}

pub enum Result {
    Layout(Laid),
    Reveal(Revealed),
    Drop(Dropped),
    Kept(Kept),
}

#[derive(Default)]
pub struct LayoutJobs {
    running: Option<TaskHandle<Result>>,
    pending_layout: Option<Request>,
    pending_reveal: Option<Request>,
    pending_drop: Option<Request>,
    next_reveal: bool,
}

impl LayoutJobs {
    /// Lay out `unit`. With `lead`, the part at that path is being dragged to its hint in the
    /// design: it leads, and the sheet is the one its drop commits (`fab_elec_explore::drag_layout`).
    /// A Library part keeps its frozen sheet in up to three layouts. If there is no room, it leads
    /// the preview and its drop receives the note. Only the latest waiting layout runs.
    pub fn request(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, hints: SheetHints, lead: Option<String>,
        kept: Option<(Arc<KeepBase>, String)>, keep: Keep, gen: u64, opts: Opts) {
        let request = Request::Layout { design, unit, hints, lead, kept, keep, gen, opts };
        if self.running.is_some() {
            self.pending_layout = Some(request);
        } else {
            self.start(cx, request);
        }
    }

    pub fn reveal(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, sheet: Arc<SheetLayout>, focus: u32, gen: u64) {
        let request = Request::Reveal { design, unit, sheet, focus, gen };
        if self.running.is_some() {
            self.pending_reveal = Some(request);
        } else {
            self.start(cx, request);
        }
    }

    /// Lay out the release's full sample before the next waiting layout or reveal.
    pub fn drop_hints(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, path: String, heads: Arc<SheetHints>, token: u64, opts: Opts) {
        let request = Request::Drop { design, unit, path, heads, token, opts };
        if self.running.is_some() {
            self.pending_drop = Some(request);
        } else {
            self.start(cx, request);
        }
    }

    /// Ask for keep hints from the edit's evaluated preview before the next waiting layout or reveal.
    pub fn keep_hints(&mut self, cx: &mut Cx, design: Arc<Design>, unit: usize, base: Arc<KeepBase>, carried: Option<String>, token: u64, opts: Opts) {
        let request = Request::Keep { design, unit, base, carried, token, opts };
        if self.running.is_some() {
            self.pending_drop = Some(request);
        } else {
            self.start(cx, request);
        }
    }

    /// Pool completion posts `Event::Signal`, after publishing the result. Poll there;
    /// neither a frame loop nor a blocking join is needed.
    pub fn poll(&mut self, cx: &mut Cx) -> Option<Result> {
        let result = self.running.as_mut()?.try_take()?;
        self.running = None;
        let pending = if let Some(drop) = self.pending_drop.take() {
            Some(drop)
        } else if self.next_reveal {
            self.pending_reveal.take().or_else(|| self.pending_layout.take())
        } else {
            self.pending_layout.take().or_else(|| self.pending_reveal.take())
        };
        if let Some(request) = pending {
            self.start(cx, request);
        }
        match result {
            Ok(layout) => Some(layout),
            Err(error) => {
                log!("fab.explorer: layout worker failed: {error}");
                None
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.running.is_some() || self.pending_layout.is_some() || self.pending_reveal.is_some() || self.pending_drop.is_some()
    }

    fn start(&mut self, cx: &mut Cx, request: Request) {
        self.next_reveal = matches!(&request, Request::Layout { .. });
        // submit uses the runtime's warm workers and signals success, cancellation or panic.
        match cx.task_pool().submit(Class::Visible, move || {
            let start = Cx::monotonic_now();
            match request {
                Request::Layout { design, unit, hints, lead, kept, keep, gen, opts } => {
                    // Use the hints of this host snapshot even if the pointer has sampled again.
                    let layout = |lead: Option<String>| {
                        let led = lead.and_then(|path| {
                            let source = &design.units.get(unit)?.circuit.hints.parts;
                            let (_, hint) = source.iter().rev().find(|(p, _)| *p == path)?;
                            let parts = if let Some(heads) = keep.block.as_ref().and_then(|a| a.heads.as_ref()) {
                                source.iter().filter(|(p, _)| heads.contains(p)).cloned().collect()
                            } else { vec![(path.clone(), hint.clone())] };
                            Some((path, SheetHints { parts, ..Default::default() }))
                        });
                        match led {
                            Some((path, heads)) if hints.parts.is_empty() => fab_elec_explore::drag_group_layout(&design, unit, &path, &heads, &opts).sheet,
                            _ => fab_elec_explore::layout(&design, unit, &hints, &opts),
                        }
                    };
                    let (mut sheet, kept) = match kept {
                        Some((base, path)) => match fab_elec_explore::kept_layout(&design, unit, &base, &[&path], &opts) {
                            Ok((sheet, hints)) => (sheet, Some(Ok(hints))),
                            Err(note) => (layout(Some(path)), Some(Err(note))),
                        },
                        None => (layout(lead), None),
                    };
                    let shift = keep.block
                        .and_then(|a| block_origin(&sheet, a.block).map(|now| [a.origin[0] - now[0], a.origin[1] - now[1]]))
                        .unwrap_or(keep.shift);
                    sheet.translate(shift);
                    let ms = (Cx::monotonic_now() - start) * 1000.0;
                    let SheetTrees { trees, pieces } = sheet_trees(&design, unit, &sheet);
                    Result::Layout(Laid { gen, sheet, kept, trees: SegRows::prepare(trees), pieces, shift, ms, aspect: opts.aspect })
                }
                Request::Reveal { design, unit, sheet, focus, gen } => {
                    let circuit = &design.units[unit].circuit;
                    let graph = fab_elec_explore::adapt::graph_of(circuit, &SheetHints::default());
                    let reveal = reveal_sheet(&design, unit, &graph, &sheet, focus);
                    let SheetTrees { trees, pieces } = sheet_trees(&design, unit, &reveal.sheet);
                    let trees = SegRows::prepare(trees);
                    log!("fab.explorer: reveal {}: {} parts in {:.2} ms (worker)",
                        circuit.comps[focus as usize].refdes, reveal.rels.len(),
                        (Cx::monotonic_now() - start) * 1000.0);
                    Result::Reveal(Revealed { gen, reveal, trees, pieces })
                }
                Request::Drop { design, unit, path, heads, token, opts } => {
                    Result::Drop(Dropped { token, drop: fab_elec_explore::drag_group_drop(&design, unit, &path, &heads, &opts) })
                }
                Request::Keep { design, unit, base, carried, token, opts } => {
                    let Some(circuit) = design.units.get(unit).map(|u| &u.circuit) else { return Result::Kept(Kept { token, hints: Ok(Vec::new()) }) };
                    let new = match carried.as_deref() {
                        Some(path) => vec![path],
                        None => circuit.comps.iter().map(|c| c.path.as_str()).filter(|path| base.circuit.comp(path).is_none()).collect(),
                    };
                    let hints = fab_elec_explore::kept_layout(&design, unit, &base, &new, &opts).map(|(_, hints)| hints);
                    let ms = (Cx::monotonic_now() - start) * 1000.0;
                    match &hints {
                        Ok(hints) => {
                            let free = hints.iter().filter(|(_, hint)| hint.free).count();
                            log!("fab.circuit: edit keeps the sheet: {} hints, {free} of {} added parts free, {ms:.1} ms (worker)", hints.len() - free, new.len());
                        }
                        Err(note) => log!("fab.circuit: edit refused: {note} ({ms:.1} ms, worker)"),
                    }
                    Result::Kept(Kept { token, hints })
                }
            }
        }) {
            Ok(task) => self.running = Some(task),
            Err(error) => log!("fab.explorer: layout worker unavailable: {error}"),
        }
    }
}

impl Drop for LayoutJobs {
    fn drop(&mut self) {
        if let Some(task) = &self.running {
            task.cancel();
        }
    }
}
