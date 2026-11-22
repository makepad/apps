//! The source map (C5, E4 item 4): two-way between any `Sel` and the source range that made it.
//!
//! `locate` answers where a component, net, block, port, board item, hint or bench statement is
//! written; `at` answers what the text at a caret made. A range written once and run several
//! times (a block definition's `let u = e.part(..)`, a loop body) maps back to every item it made:
//! `at` gives the first, `all_at` all of them (the editor lights every instance).
//!
//! The map follows text edits by their length delta (`follow`), so a board edit that splices one
//! statement keeps every other statement's span current without evaluating again.
//!
//! Cost: `locate` is a hash lookup; `at` a binary search plus a walk back over the ranges that
//! start before the caret and end before it (siblings), at most the document's entries; `follow`
//! is linear in the edited document's entries (one pass, no re-sort).

use crate::splice::SpanEdit;
use fab_elec_core::{BoardId, CompId, CompRef, DocId, PinIx, Sel};
use fab_elec_design::{ProbeOf, TextEdit};
use std::collections::HashMap;
use std::ops::Range;

/// A byte range of one document.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SourceRange {
    pub doc: DocId,
    pub span: Range<u32>,
}

/// The native call that made a value, with for-loop iterations inside its function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Origin {
    pub call: SourceRange,
    pub loops: Vec<u32>,
}

/// A bench statement or pin field and the provenance of its evaluated value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BenchSite {
    pub at: SourceRange,
    pub origin: Option<Origin>,
}

impl From<SourceRange> for BenchSite {
    fn from(at: SourceRange) -> Self {
        Self { at, origin: None }
    }
}

/// Where a component's value is written: what a value edit changes (architect change 3). The
/// value chip says it before the drop; `SetValue` splices it.
#[derive(Clone, PartialEq, Debug)]
pub enum ValueSite {
    /// A literal (`10kohm`, `"10k"`, `@green`). `count` components read it: one is this component
    /// alone; more is a block body or a loop body, and an edit changes every one of them.
    Literal { at: SourceRange, count: usize },
    /// A block parameter (`o.c_in`), or item `item` of a list parameter (`for v in o.decoupling`):
    /// the edit is an override in the instance's call `call`, which `count` instances share (a call
    /// in a loop body); `default` is where the definition's default literal is written.
    Param { name: String, item: Option<usize>, call: SourceRange, default: Option<SourceRange>, count: usize },
    /// Written in library `lib` and not a parameter there: not edited.
    Library { lib: String },
    /// An expression and its resolved inputs. A missing leaf is a refusal.
    Derived {
        at: SourceRange,
        leaf: Option<SourceRange>,
        inputs: Vec<Input>,
        siblings: usize,
    },
}

/// Explain the origin site `SetValue` writes, including all instances sharing it.
pub fn site_text(site: Option<&ValueSite>, comp_path: &str) -> Result<Option<String>, String> {
    match site {
        Some(ValueSite::Library { lib }) => Err(format!("written in library {lib}: not edited here")),
        Some(ValueSite::Literal { count, .. }) if *count > 1 => {
            let name = comp_path.rsplit('.').next().unwrap_or(comp_path);
            let name = name.split('[').next().unwrap_or(name);
            Ok(Some(format!("{name}, {count} parts")))
        }
        Some(ValueSite::Param { name, count, .. }) => Ok(Some(if *count > 1 {
            format!("parameter {name}, {count} instances")
        } else { format!("parameter {name}") })),
        Some(ValueSite::Derived { .. }) => Ok(Some("computed: edits its inputs".into())),
        _ => Ok(None),
    }
}

/// An expression input and the site an edit to that input alone would change.
#[derive(Clone, PartialEq, Debug)]
pub struct Input {
    pub at: SourceRange,
    /// The written SI value, or None for an instance parameter.
    pub value: Option<f64>,
    pub site: ValueSite,
}

impl ValueSite {
    pub(crate) fn visit(&self, f: &mut impl FnMut(&ValueSite)) {
        f(self);
        if let Self::Derived { inputs, .. } = self {
            for input in inputs {
                input.site.visit(f);
            }
        }
    }

    pub(crate) fn visit_mut(&mut self, f: &mut impl FnMut(&mut ValueSite)) {
        f(self);
        if let Self::Derived { inputs, .. } = self {
            for input in inputs {
                input.site.visit_mut(f);
            }
        }
    }
}

/// Where a pin's connection is written: what `Connect` and `Disconnect` change (the value-site rule
/// applied to wires). `count` pins are connected by the same text (a loop or a block body that
/// several instances share); an edit there changes every one of them.
#[derive(Clone, PartialEq, Debug)]
pub enum PinSite {
    /// Field `pin` of the `pins: {..}` object of the part call `call`.
    Field { call: SourceRange, pin: String, count: usize },
    /// The pin's place in the part call `call`: item `index` of its `pins: [..]` list, or the
    /// two-terminal call's pin argument (`e.r(value, a, b)`: index 0 is `a`).
    Item {
        call: SourceRange,
        index: usize,
        /// Whether evaluation found a number before the two-terminal call's nets.
        /// False for a part's `pins` list. Keep this fact for computed tolerances/options.
        positional_tol: bool,
        count: usize,
    },
    /// Argument `end` (0 or 1) of the `e.connect(a, b)` call `call`.
    Connect { call: SourceRange, end: usize, count: usize },
    /// Written in library `lib`: not edited.
    Library { lib: String },
}

impl PinSite {
    pub(crate) fn key(&self) -> Option<(DocId, u32, u32, usize, &str)> {
        match self {
            PinSite::Field { call, pin, .. } => Some((call.doc, call.span.start, call.span.end, 0, pin)),
            PinSite::Item { call, index, .. } => Some((call.doc, call.span.start, call.span.end, 1 + 2 * index, "")),
            PinSite::Connect { call, end, .. } => Some((call.doc, call.span.start, call.span.end, 2 + 2 * end, "")),
            PinSite::Library { .. } => None,
        }
    }

    pub(crate) fn set_count(&mut self, n: usize) {
        match self {
            PinSite::Field { count, .. } | PinSite::Item { count, .. } | PinSite::Connect { count, .. } => *count = n,
            PinSite::Library { .. } => {}
        }
    }
}

/// An editable statement or pin field in a design bench.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum BenchKey {
    Body,
    Probe(ProbeOf),
    Expect(String),
    Stub(String, String),
    Press(String),
    Supply(String),
    Source(String, String),
}

/// Where a part's `e.body` override is written (the call), and the part's own body it replaced:
/// what a body-source switch rewrites, removes, or goes back to.
#[derive(Clone, PartialEq, Debug)]
pub struct BodySite {
    pub at: SourceRange,
    pub own: Option<fab_elec_parts::Body3d>,
}

/// The two-way map between selections and source ranges.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SourceMap {
    fwd: HashMap<Sel, SourceRange>,
    pub(crate) bench_sites: HashMap<(String, BenchKey), BenchSite>,
    /// The stub call itself, including stubs with no written pins yet.
    pub(crate) bench_stubs: HashMap<(String, String), SourceRange>,
    /// The firmware call of each component that runs firmware, by bench and component path.
    pub(crate) bench_firmware: HashMap<(String, String), SourceRange>,
    /// Evaluated values at bench sites, for ignoring unchanged splice targets.
    pub(crate) bench_expects: HashMap<(String, String), fab_elec_bench::expect::Expect>,
    pub(crate) bench_drives: HashMap<(String, String, String), fab_elec_core::drive::Drive>,
    pub(crate) bench_stimuli: HashMap<(String, BenchKey), crate::bench::Stimulus>,
    /// Editor addition orders, in component id order within each evaluated unit.
    pub(crate) added: HashMap<BoardId, Vec<(CompId, u32)>>,
    /// Where each component's value is written.
    values: HashMap<CompRef, ValueSite>,
    /// Where each connected pin's connections are written, in evaluation order.
    pins: HashMap<(CompRef, PinIx), Vec<PinSite>>,
    /// Where each unit's applied `e.body` overrides are written, by part name.
    bodies: HashMap<(BoardId, String), BodySite>,
    /// Per document, entries sorted by start, then by end descending (an outer range before the
    /// ranges it holds), then in insertion order.
    rev: HashMap<DocId, Vec<(Range<u32>, Sel)>>,
}

impl SourceMap {
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    pub fn len(&self) -> usize {
        self.fwd.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fwd.is_empty()
    }

    /// Where the bench function or the statement identified by `key` is written.
    pub fn bench_site(&self, bench: &str, key: &BenchKey) -> Option<&SourceRange> {
        self.bench_sites
            .get(&(bench.to_string(), key.clone()))
            .map(|site| &site.at)
    }

    /// The native call and its enclosing for-loop iterations for a bench value.
    pub fn bench_origin(&self, bench: &str, key: &BenchKey) -> Option<&Origin> {
        self.bench_sites
            .get(&(bench.to_string(), key.clone()))?
            .origin
            .as_ref()
    }

    /// Where `sel` is written.
    pub fn locate(&self, sel: &Sel) -> Option<SourceRange> {
        self.fwd.get(sel).cloned()
    }

    /// What the text at `offset` of `doc` made: the innermost mapped range holding the offset
    /// (the first item it made, when it made several).
    pub fn at(&self, doc: DocId, offset: u32) -> Option<Sel> {
        let list = self.rev.get(&doc)?;
        let i = Self::group_start(list, Self::innermost(list, offset)?);
        Some(list[i].1.clone())
    }

    /// Every item the innermost mapped range holding `offset` made (every instance of a
    /// definition, every iteration of a loop body), in the order they were made.
    pub fn all_at(&self, doc: DocId, offset: u32) -> Vec<Sel> {
        let Some(list) = self.rev.get(&doc) else { return Vec::new() };
        let Some(i) = Self::innermost(list, offset) else { return Vec::new() };
        let span = list[i].0.clone();
        list[Self::group_start(list, i)..].iter().take_while(|(r, _)| *r == span).map(|(_, s)| s.clone()).collect()
    }

    /// The innermost range holding `offset`. Sorted by start, then end descending: walking back
    /// from the last range starting at or before the offset, the first one holding it has the
    /// greatest start and, among equal starts, the least end.
    fn innermost(list: &[(Range<u32>, Sel)], offset: u32) -> Option<usize> {
        let end = list.partition_point(|(r, _)| r.start <= offset);
        (0..end).rev().find(|&i| {
            let r = &list[i].0;
            offset < r.end || (offset == r.end && r.start == r.end)
        })
    }

    /// The first entry with the same range as entry `i` (equal ranges sit together, in insertion order).
    fn group_start(list: &[(Range<u32>, Sel)], i: usize) -> usize {
        let span = &list[i].0;
        list[..i].iter().rposition(|(r, _)| r != span).map_or(0, |p| p + 1)
    }

    /// Where `comp`'s value is written, when it has one.
    pub fn value_site(&self, comp: &CompRef) -> Option<&ValueSite> {
        self.values.get(comp)
    }

    pub(crate) fn set_value_site(&mut self, comp: CompRef, site: ValueSite) {
        self.values.insert(comp, site);
    }

    /// Where the `e.body` override of the part named `part` is written for `unit`, if one is.
    pub fn body_site(&self, unit: BoardId, part: &str) -> Option<&BodySite> {
        self.bodies.get(&(unit, part.to_string()))
    }

    pub(crate) fn set_body_site(&mut self, unit: BoardId, part: String, site: BodySite) {
        self.bodies.insert((unit, part), site);
    }

    /// The overrides of `unit`, by part name.
    pub fn body_sites_of(&self, unit: BoardId) -> Vec<(String, BodySite)> {
        let mut out: Vec<(String, BodySite)> = self.bodies.iter().filter(|((u, _), _)| *u == unit).map(|((_, p), s)| (p.clone(), s.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Where `pin` of `comp` is connected, in evaluation order (empty: not connected in text).
    pub fn pin_sites(&self, comp: &CompRef, pin: PinIx) -> &[PinSite] {
        self.pins.get(&(*comp, pin)).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn set_pin_sites(&mut self, comp: CompRef, pin: PinIx, sites: Vec<PinSite>) {
        self.pins.insert((comp, pin), sites);
    }

    /// The pin sites of `unit`'s components.
    pub fn pin_sites_of(&self, unit: fab_elec_core::BoardId) -> Vec<((CompRef, PinIx), Vec<PinSite>)> {
        let mut out: Vec<_> = self.pins.iter().filter(|((c, _), _)| c.board == unit).map(|(k, s)| (*k, s.clone())).collect();
        out.sort_by_key(|((c, p), _)| (c.comp, *p));
        out
    }

    /// The value sites of `unit`'s components.
    pub fn value_sites_of(&self, unit: fab_elec_core::BoardId) -> Vec<(CompRef, ValueSite)> {
        let mut out: Vec<(CompRef, ValueSite)> = self.values.iter().filter(|(c, _)| c.board == unit).map(|(c, s)| (*c, s.clone())).collect();
        out.sort_by_key(|(c, _)| c.comp);
        out
    }

    /// Every entry written in `doc`, in range order.
    pub fn of_doc(&self, doc: DocId) -> Vec<(Sel, SourceRange)> {
        let Some(list) = self.rev.get(&doc) else { return Vec::new() };
        list.iter().map(|(span, sel)| (sel.clone(), SourceRange { doc, span: span.clone() })).collect()
    }

    /// Record that `sel` is written at `range` (replacing an earlier range of `sel`).
    pub(crate) fn insert(&mut self, sel: Sel, range: SourceRange) {
        self.remove(&sel);
        let list = self.rev.entry(range.doc).or_default();
        let key = |r: &Range<u32>| (r.start, std::cmp::Reverse(r.end));
        let at = list.partition_point(|(r, _)| key(r) <= key(&range.span));
        list.insert(at, (range.span.clone(), sel.clone()));
        self.fwd.insert(sel, range);
    }

    pub(crate) fn remove(&mut self, sel: &Sel) {
        if let Some(old) = self.fwd.remove(sel) {
            if let Some(list) = self.rev.get_mut(&old.doc) {
                if let Some(i) = list.iter().position(|(r, s)| *r == old.span && s == sel) {
                    list.remove(i);
                }
            }
        }
    }

    /// A kept lowering may refer to a removed document; none of its sites remain editable.
    pub(crate) fn retain_docs(&mut self, present: impl Fn(DocId) -> bool) {
        self.fwd.retain(|_, at| present(at.doc));
        self.rev.retain(|doc, _| present(*doc));
        self.bench_sites.retain(|_, site| {
            present(site.at.doc) && site.origin.as_ref().is_none_or(|origin| present(origin.call.doc))
        });
        self.bench_stubs.retain(|_, at| present(at.doc));
        self.bench_firmware.retain(|_, at| present(at.doc));
        self.values.retain(|_, site| {
            let mut keep = true;
            site.visit(&mut |site| {
                keep &= match site {
                    ValueSite::Literal { at, .. } => present(at.doc),
                    ValueSite::Param { call, default, .. } => {
                        present(call.doc) && default.as_ref().is_none_or(|at| present(at.doc))
                    }
                    ValueSite::Derived { at, leaf, inputs, .. } => {
                        present(at.doc) && leaf.as_ref().is_none_or(|at| present(at.doc))
                            && inputs.iter().all(|input| present(input.at.doc))
                    }
                    ValueSite::Library { .. } => true,
                };
            });
            keep
        });
        self.pins.retain(|_, sites| {
            sites.retain(|site| match site {
                PinSite::Field { call, .. } | PinSite::Item { call, .. } | PinSite::Connect { call, .. } => present(call.doc),
                PinSite::Library { .. } => true,
            });
            !sites.is_empty()
        });
    }

    /// Follow a splice: shift every range by the length delta of the edits before it, drop what
    /// the splice removed and learn what it wrote (`SpanEdit::adds`, in the text after the edit).
    pub fn follow(&mut self, edit: &SpanEdit) {
        self.shift(&edit.edits);
        for sel in &edit.removes {
            self.remove(sel);
        }
        for (sel, range) in &edit.adds {
            self.insert(sel.clone(), range.clone());
        }
    }

    /// Shift ranges past text edits (spans in the text before them). A start moves with edits
    /// that end at or before it (an insertion at a statement's start goes before it); an end
    /// moves with edits that start before it (an insertion at a statement's end goes after it).
    fn shift(&mut self, edits: &[TextEdit]) {
        let mut docs: Vec<DocId> = edits.iter().map(|e| e.doc).collect();
        docs.sort();
        docs.dedup();
        for doc in docs {
            let mut list: Vec<(u32, u32, i64)> = edits.iter().filter(|e| e.doc == doc).map(|e| (e.span.start, e.span.end, e.text.len() as i64 - (e.span.end - e.span.start) as i64)).collect();
            list.sort();
            let start_of = |p: u32| -> u32 {
                let mut d = 0i64;
                for &(a, b, delta) in &list {
                    if b <= p {
                        d += delta;
                    } else if a < p {
                        // inside a replaced region: to the region's start
                        return (a as i64 + d).max(0) as u32;
                    }
                }
                (p as i64 + d).max(0) as u32
            };
            let end_of = |p: u32| -> u32 {
                let mut d = 0i64;
                for &(a, b, delta) in &list {
                    if b < p || (b == p && a < p) {
                        d += delta;
                    } else if a < p {
                        // inside a replaced region: to the end of the text that replaced it
                        return (b as i64 + d + delta).max(0) as u32;
                    }
                }
                (p as i64 + d).max(0) as u32
            };
            let follow = |r: &mut SourceRange| {
                if r.doc == doc {
                    r.span = start_of(r.span.start)..end_of(r.span.end).max(start_of(r.span.start));
                }
            };
            for site in self.bench_sites.values_mut() {
                follow(&mut site.at);
                if let Some(origin) = &mut site.origin {
                    follow(&mut origin.call);
                }
            }
            for at in self.bench_stubs.values_mut().chain(self.bench_firmware.values_mut()) {
                follow(at);
            }
            for site in self.values.values_mut() {
                site.visit_mut(&mut |site| match site {
                    ValueSite::Literal { at, .. } => follow(at),
                    ValueSite::Derived {
                        at, leaf, inputs, ..
                    } => {
                        follow(at);
                        if let Some(leaf) = leaf {
                            follow(leaf);
                        }
                        for input in inputs {
                            follow(&mut input.at);
                        }
                    }
                    ValueSite::Param { call, default, .. } => {
                        follow(call);
                        if let Some(d) = default {
                            follow(d);
                        }
                    }
                    ValueSite::Library { .. } => {}
                });
            }
            for site in self.pins.values_mut().flatten() {
                match site {
                    PinSite::Field { call, .. } | PinSite::Item { call, .. } | PinSite::Connect { call, .. } => follow(call),
                    PinSite::Library { .. } => {}
                }
            }
            if let Some(entries) = self.rev.get_mut(&doc) {
                entries.retain(|(r, sel)| {
                    let deleted = matches!(sel, Sel::Item(..) | Sel::Link(..)) && edits.iter().any(|e| {
                        e.doc == doc && e.span.start <= r.start && r.end <= e.span.end
                            && (e.text.is_empty() || e.span != *r)
                    });
                    if deleted { self.fwd.remove(sel); }
                    !deleted
                });
                for (r, sel) in entries.iter_mut() {
                    let new = start_of(r.start)..end_of(r.end).max(start_of(r.start));
                    if let Some(f) = self.fwd.get_mut(sel) {
                        if f.doc == doc && f.span == *r {
                            f.span = new.clone();
                        }
                    }
                    *r = new;
                }
            }
        }
    }
}
