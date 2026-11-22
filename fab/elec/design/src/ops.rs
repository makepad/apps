//! Design ops and the op log (C3, E2.18): every edit from any view, tool or the AI is a
//! `DesignOp` the host applies and logs; undo is project-wide and one step per action.
//!
//! Board ops apply to the unit's board (`Board::apply`, with their exact inverse and delta);
//! system ops to the design's links, mounts and unit factory profiles; circuit and bench ops
//! are span edits on their source documents applied by the language (`fab_elec_lang::splice`),
//! so their inverse is the inverse text edit (`TextEdit`), recorded as such (E4 item 7).

use crate::{By, Design, Frame, Link, Mount};
use fab_elec_board::{Applied, BoardOp, OpError};
use fab_elec_circuit::CircuitOp;
use fab_elec_core::drive::Drive;
use fab_elec_core::{canon_enum, canon_struct, canon_tags, BoardId, DocId, LinkId, Sel, Xform3};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, PartialEq, Debug)]
pub enum SysOp {
    AddLink(Link),
    /// Insert a link at a position (the inverse of `RemoveLink`, exact order).
    InsertLink { link: Link, pos: u32 },
    RemoveLink(LinkId),
    ReplaceLink(Link),
    /// Set a unit's mount (adds one when it has none).
    SetMount(Mount),
    /// Reset a unit's mount to the world frame with the identity transform.
    ClearMount(BoardId),
    /// Set a unit's factory profile: one undo step; the unit's board engines then recompute in full.
    SetFactory { unit: BoardId, profile: Option<String> },
}

/// What a scope probe reads.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ProbeQuantity {
    V,
    I,
    P,
}

/// A scope probe's identity, by stable names (never indices: a re-evaluation renumbers).
/// `target` is the net (V), "path.PIN" (I) or the component path (P); `reference` is the second
/// net of a differential voltage probe, None otherwise.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProbeOf {
    pub quantity: ProbeQuantity,
    pub target: String,
    pub reference: Option<String>,
}

impl ProbeOf {
    pub fn v(net: impl Into<String>) -> Self {
        Self {
            quantity: ProbeQuantity::V,
            target: net.into(),
            reference: None,
        }
    }

    pub fn vdiff(a: impl Into<String>, b: impl Into<String>) -> Self {
        Self {
            quantity: ProbeQuantity::V,
            target: a.into(),
            reference: Some(b.into()),
        }
    }

    pub fn i(pin_path: impl Into<String>) -> Self {
        Self {
            quantity: ProbeQuantity::I,
            target: pin_path.into(),
            reference: None,
        }
    }

    pub fn p(comp_path: impl Into<String>) -> Self {
        Self {
            quantity: ProbeQuantity::P,
            target: comp_path.into(),
            reference: None,
        }
    }
}

impl std::fmt::Display for ProbeOf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.quantity {
            ProbeQuantity::V => match &self.reference {
                Some(reference) => write!(f, "v({}, {reference})", self.target),
                None => f.write_str(&self.target),
            },
            ProbeQuantity::I => write!(f, "i({})", self.target),
            ProbeQuantity::P => write!(f, "p({})", self.target),
        }
    }
}

/// A real probe's tip: input R and C across its two points (voltage probes only), and the
/// channel's single-pole bandwidth in Hz. All None is an ideal probe.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Tip {
    pub r: Option<f64>,
    pub c: Option<f64>,
    pub bw: Option<f64>,
}

impl Tip {
    /// ln(9) / 2π: a single pole's 10-90 % rise time times its bandwidth.
    pub const RISE_BW: f64 = 0.3496991525660598;

    /// The bandwidth of a rise time, or the rise time of a bandwidth: x = ln(9) / (2π y). Both
    /// directions use this one law.
    pub fn rise_bw(y: f64) -> f64 {
        Self::RISE_BW / y
    }

    /// The rise time of this tip's bandwidth: the shortest decimal whose bandwidth is exactly
    /// `bw`, so a rise read in and written back keeps its number.
    pub fn rise(&self) -> Option<f64> {
        let bw = self.bw?;
        let exact = Self::rise_bw(bw);
        Some(
            (1..=17)
                .filter_map(|digits| format!("{exact:.*e}", digits - 1).parse::<f64>().ok())
                .find(|&rise| Self::rise_bw(rise) == bw)
                .unwrap_or(exact),
        )
    }
}

canon_tags!(ProbeQuantity);
canon_struct!(ProbeOf { quantity, target, reference });
canon_struct!(Tip { r, c, bw });

/// A bench's literal edits (ELANG splices each literal; the bench alone re-evaluates).
#[derive(Clone, PartialEq, Debug)]
pub enum BenchOp {
    /// An expectation's limits, by its name (`Expect.name`).
    SetRange { expect: String, min: f64, max: f64 },
    /// A stub pin's drive, by the stubbed part's path and the pin's name (as `Stub.pins` keys it).
    SetDrive { stub: String, pin: String, drive: Drive },
    /// A button part held pressed or released, by its path.
    SetPressed { comp: String, pressed: bool },
    /// A scope channel identified by what it reads, with its label. An existing probe is a no-op.
    AddProbe { of: ProbeOf, label: String },
    /// Remove the scope channel identified by what it reads. An absent probe is a no-op.
    RemoveProbe { of: ProbeOf },
    /// Explicitly write a stub pin's drive literally, replacing code or inserting an assignment.
    WriteDrive { stub: String, pin: String, drive: Drive },
    /// Set a voltage probe's reference net; None reads its target relative to ground.
    SetProbeRef { of: ProbeOf, reference: Option<String> },
    /// Set a scope channel's tip load and bandwidth; all None makes an ideal probe.
    SetProbeTip { of: ProbeOf, tip: Tip },
    /// Rename a scope channel identified by what it reads: its label, the name every view shows.
    SetProbeLabel { of: ProbeOf, label: String },
    /// Place a voltage probe's named connection on the drawings at a pin of its net
    /// (`"<refdes>.<pad>"`); None lets each drawing pick.
    SetProbeAt { of: ProbeOf, at: Option<String> },
    /// A supply's `v`, `limit` or `ramp` in volts, amperes or seconds, by net name.
    SetSupply { net: String, key: String, value: f64 },
    /// A source or load's `dc`, `v`, `low`, `width`, `every`, `at`, `edge`, `r`, `i` or `rise`
    /// in SI units, by its terminal net names in call order.
    SetSource { a: String, b: String, key: String, value: f64 },
    /// Add a supply to an unsupplied net other than ground.
    AddSupply { net: String, v: f64 },
    /// Remove the supply on a net. An absent supply is a no-op.
    RemoveSupply { net: String },
    /// Add a source or load with its terminals and numeric SI options in written order.
    AddSource { kind: String, a: String, b: String, opts: Vec<(String, f64)> },
    /// Remove the unique source or load on an ordered pair. An absent pair is a no-op.
    RemoveSource { a: String, b: String },
}

impl BenchOp {
    /// The op writes a new statement into the bench (a probe, a supply, a source): its splice
    /// says where it went in (`SpanEdit::written`), one range per op, in op order.
    pub fn inserts(&self) -> bool {
        matches!(self, BenchOp::AddProbe { .. } | BenchOp::AddSupply { .. } | BenchOp::AddSource { .. })
    }
}

// Tags never renumber; a new variant takes the next tag. Bytes are content only.
canon_enum!(BenchOp {
    0 => SetRange { expect, min, max },
    1 => SetDrive { stub, pin, drive },
    2 => SetPressed { comp, pressed },
    3 => AddProbe { of, label },
    4 => RemoveProbe { of },
    5 => WriteDrive { stub, pin, drive },
    6 => SetProbeRef { of, reference },
    7 => SetProbeTip { of, tip },
    8 => SetProbeLabel { of, label },
    9 => SetProbeAt { of, at },
    10 => SetSupply { net, key, value },
    11 => SetSource { a, b, key, value },
    12 => AddSupply { net, v },
    13 => RemoveSupply { net },
    14 => AddSource { kind, a, b, opts },
    15 => RemoveSource { a, b },
});

/// An asset's bytes, shared and never copied: the op, its preview, the undo history's steps and
/// the plate's write hold one allocation, freed with the last of them. Debug shows the length.
#[derive(Clone, PartialEq, Eq)]
pub struct AssetBytes(pub Arc<[u8]>);

impl std::fmt::Debug for AssetBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AssetBytes({} bytes)", self.0.len())
    }
}

/// A project file a commit adds beside its other ops, so an imported part's copy (its document,
/// its body mesh) and the part that uses it are one commit and one undo step. The language adds
/// them to the commit's documents before it splices the other ops (`splice_verified`).
#[derive(Clone, PartialEq, Debug)]
pub enum FileOp {
    /// A project document by its project-relative name. A present document with this name and
    /// text makes it a no-op; with other text the commit is refused: a copy is never replaced.
    AddDoc { name: String, text: Arc<str> },
    /// A project asset by its project-relative name, whose bytes the host holds under `key`; the
    /// step keeps `bytes` so an undone asset comes back on redo. A name the project lists already
    /// makes it a no-op; the host refuses a name it holds under another key.
    AddAsset { name: String, key: String, bytes: AssetBytes },
}

#[derive(Clone, PartialEq, Debug)]
pub enum DesignOp {
    Board { unit: BoardId, op: BoardOp },
    Circuit { unit: BoardId, op: CircuitOp },
    Bench { bench: String, op: BenchOp },
    Sys(SysOp),
    File(FileOp),
}

/// One text edit on a source document: replace `span` (byte offsets) by `text`.
#[derive(Clone, PartialEq, Debug)]
pub struct TextEdit {
    pub doc: DocId,
    pub span: std::ops::Range<u32>,
    pub text: String,
}

impl TextEdit {
    /// The smallest single replacement that turns `from` into `to`, trimming common UTF-8
    /// characters without overlapping. Cost: O(text) per edit.
    pub fn trimmed(doc: DocId, from: &str, to: &str) -> TextEdit {
        let start = from.chars().zip(to.chars()).take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf8()).sum::<usize>();
        let suffix = from[start..].chars().rev().zip(to[start..].chars().rev())
            .take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf8()).sum::<usize>();
        TextEdit { doc, span: start as u32..(from.len() - suffix) as u32, text: to[start..to.len() - suffix].into() }
    }
}

/// An asset's availability in one step: `name` (project-relative) names the host's bytes under
/// `key`. Undo of an added asset makes it unavailable and keeps key and bytes, so redo brings the
/// same bytes back: the history's steps own them, and dropping the entries (a commit after an
/// undo, or a restart of the history) frees them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AssetStep {
    pub name: String,
    pub key: String,
    pub bytes: AssetBytes,
    pub available: bool,
}

/// The text side of one undo step, in one direction: the language's splice result (`SpanEdit`)
/// kept below the language. Applying it is `Sources::apply(edits)`, then presence changes in
/// order, then asset availability in order, then `SourceMap::follow`,
/// which records `adds` (the statements it writes, at their ranges in the text after it) and drops
/// `removes` from the id ledger, so ids come back exactly when the step is undone or redone.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TextStep {
    /// Edits in the text as it is before them, disjoint.
    pub edits: Vec<TextEdit>,
    /// Presence set after the edits, in order. Undo of an add makes the document absent (text
    /// kept); undo of a remove makes it present again.
    pub present: Vec<(DocId, bool)>,
    /// Asset availability set after presence, in order (`FileOp::AddAsset`; undo makes it unavailable).
    pub assets: Vec<AssetStep>,
    /// Statements the step writes: the selection each one makes, its document and its range in
    /// the text after the step.
    pub adds: Vec<(Sel, DocId, Range<u32>)>,
    /// Items whose statement the step removes.
    pub removes: Vec<Sel>,
    /// The step changes what evaluation gives beyond what the model ops did (circuit, bench and
    /// typed text): the documents are evaluated again after it.
    pub reevaluate: bool,
}

impl TextStep {
    /// Nothing to apply: a model-only step (system ops, gap G1) or a step made before the
    /// documents were on the strand.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty() && self.present.is_empty() && self.assets.is_empty() && self.adds.is_empty() && self.removes.is_empty()
    }
}

/// How an entry's text and model follow one another during undo and redo.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum EntryKind {
    /// Board ops spliced into documents: model ops at matching epochs, otherwise text and evaluation.
    #[default]
    Board,
    /// Circuit/bench ops or typed circuit, board, part or bench text: text then pool evaluation.
    Eval,
    /// System ops stay model-driven; typed system text is text-only until lowering lands (G1).
    System,
}

/// One undo step: what was done and what undoes it. The text is the authority: undo and redo
/// apply `text_inverse` / `text` and follow the map; `inverse` / `ops` are the model's fast path,
/// taken when every unit in `epochs` still has that epoch, else the board is re-lowered from the
/// restored text.
#[derive(Clone, PartialEq, Debug)]
pub struct OpEntry {
    /// The author of the original edit, retained through undo, redo and merged typing.
    pub by: By,
    /// The path undo and redo use to bring the model along with the text.
    pub kind: EntryKind,
    /// Typed text of one editor gesture merges into one entry.
    pub group: Option<u64>,
    pub seq: u64,
    pub label: String,
    pub ops: Vec<DesignOp>,
    /// Model ops that undo the model part, in order (board and system ops).
    pub inverse: Vec<DesignOp>,
    /// The text step that made this entry (redo applies it again).
    pub text: TextStep,
    /// The text step that undoes it.
    pub text_inverse: TextStep,
    /// The epoch of each unit the entry's model ops touch (sorted by unit), as it was when the
    /// entry was made. A unit's epoch moves when its circuit's board-facing names change, which
    /// renumbers the `CompId`s and `NetId`s the ops hold.
    pub epochs: Vec<(BoardId, u64)>,
}

/// The project's history; `head` is the number of applied entries (redo above it).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct OpLog {
    pub entries: Vec<OpEntry>,
    pub head: usize,
}

impl OpLog {
    /// Record a step, dropping redo above the head; `e.seq` comes from the shared counter, possibly reserved earlier.
    pub fn push(&mut self, e: OpEntry) {
        self.entries.truncate(self.head);
        self.entries.push(e);
        self.head = self.entries.len();
    }
    pub fn can_undo(&self) -> bool {
        self.head > 0
    }
    pub fn can_redo(&self) -> bool {
        self.head < self.entries.len()
    }
}

/// What applying a design op did.
#[derive(Clone, PartialEq, Debug)]
pub enum DesignApplied {
    Board { unit: BoardId, applied: Applied },
    Sys { inverse: SysOp },
}

impl DesignApplied {
    /// The inverse as a design op.
    pub fn inverse(&self) -> Vec<DesignOp> {
        match self {
            DesignApplied::Board { unit, applied } => applied.inverse.0.iter().map(|op| DesignOp::Board { unit: *unit, op: op.clone() }).collect(),
            DesignApplied::Sys { inverse } => vec![DesignOp::Sys(inverse.clone())],
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum DesignOpError {
    NoUnit(BoardId),
    NoLink(LinkId),
    DuplicateLink(LinkId),
    /// The first mounted unit whose parent is this link.
    LinkInUse { link: LinkId, unit: BoardId },
    /// A mount names itself or a missing unit or link as its parent.
    BadMountParent { unit: BoardId },
    Board(OpError),
    /// Circuit ops are span edits; apply them through the language.
    CircuitIsText,
    /// Bench ops are span edits; apply them through the language.
    BenchIsText,
    /// File ops add documents and assets; apply them through the language (`splice_verified`).
    FileIsText,
}

impl std::fmt::Display for DesignOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LinkInUse { link, unit } => write!(f, "link {link:?} is the mount parent of unit {unit:?}"),
            Self::BadMountParent { unit } => write!(f, "mount of unit {unit:?} names itself or a missing unit or link as parent"),
            _ => write!(f, "{self:?}"),
        }
    }
}

impl Design {
    /// Apply a board or system op (circuit and bench ops go through the language). `host` applies board
    /// ops without the edit rules (undo, redo, the host's own stages).
    pub fn apply(&mut self, op: &DesignOp, host: bool) -> Result<DesignApplied, DesignOpError> {
        let r = match op {
            DesignOp::Board { unit, op } => {
                let u = self.units.iter_mut().find(|u| u.id == *unit).ok_or(DesignOpError::NoUnit(*unit))?;
                let b = u.board.as_mut().ok_or(DesignOpError::NoUnit(*unit))?;
                let b = Arc::make_mut(b);
                let applied = if host { b.apply_host(op) } else { b.apply(op) }.map_err(DesignOpError::Board)?;
                DesignApplied::Board { unit: *unit, applied }
            }
            DesignOp::Circuit { .. } => return Err(DesignOpError::CircuitIsText),
            DesignOp::Bench { .. } => return Err(DesignOpError::BenchIsText),
            DesignOp::File(_) => return Err(DesignOpError::FileIsText),
            DesignOp::Sys(s) => DesignApplied::Sys { inverse: self.apply_sys(s)? },
        };
        self.generation += 1;
        Ok(r)
    }

    /// Apply one change's ops, all or nothing: each run of board ops for one unit applies as one
    /// batch (`Board::apply_batch`), so the change lands as its ops wrote it; the others apply
    /// one by one. On an error every applied op is undone (batches as batches) and the
    /// generations are restored; the error names the index of the op it comes from.
    /// Cost: the batches' and system ops' own costs; the board ops of a run are borrowed.
    pub fn apply_all(&mut self, ops: &[DesignOp], host: bool) -> Result<Vec<DesignApplied>, (usize, DesignOpError)> {
        let generation = self.generation;
        let generations: Vec<_> = self.units.iter().map(|u| u.board.as_ref().map(|b| b.generation)).collect();
        let mut done: Vec<DesignApplied> = Vec::new();
        let mut i = 0;
        while i < ops.len() {
            let start = i;
            let r = match &ops[i] {
                DesignOp::Board { unit, .. } => {
                    let unit = *unit;
                    while matches!(ops.get(i), Some(DesignOp::Board { unit: u, .. }) if *u == unit) {
                        i += 1;
                    }
                    let run = ops[start..i].iter().map(|op| match op {
                        DesignOp::Board { op, .. } => op,
                        _ => unreachable!("a run holds board ops only"),
                    });
                    self.apply_board(unit, run, host).map_err(|(k, e)| (start + k, e))
                }
                op => {
                    i += 1;
                    self.apply(op, host).map_err(|e| (start, e))
                }
            };
            match r {
                Ok(applied) => done.push(applied),
                Err(error) => {
                    for applied in done.iter().rev() {
                        self.revert(applied);
                    }
                    self.generation = generation;
                    for (unit, generation) in self.units.iter_mut().zip(generations) {
                        if let (Some(board), Some(generation)) = (&mut unit.board, generation) {
                            if board.generation != generation {
                                Arc::make_mut(board).generation = generation;
                            }
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(done)
    }

    /// One unit's run of board ops as one batch.
    fn apply_board<'a>(&mut self, unit: BoardId, ops: impl ExactSizeIterator<Item = &'a BoardOp>, host: bool) -> Result<DesignApplied, (usize, DesignOpError)> {
        let count = ops.len() as u64;
        let u = self.units.iter_mut().find(|u| u.id == unit).ok_or((0, DesignOpError::NoUnit(unit)))?;
        let b = Arc::make_mut(u.board.as_mut().ok_or((0, DesignOpError::NoUnit(unit)))?);
        let applied = b.apply_batch_seen(ops, host, |_, _| {}).map_err(|(k, e)| (k, DesignOpError::Board(e)))?;
        self.generation += count;
        Ok(DesignApplied::Board { unit, applied })
    }

    /// Undo an applied op or batch (a batch's inverse as one batch).
    fn revert(&mut self, applied: &DesignApplied) {
        match applied {
            DesignApplied::Board { unit, applied } => {
                let u = self.units.iter_mut().find(|u| u.id == *unit).expect("an applied unit is there");
                let b = Arc::make_mut(u.board.as_mut().expect("an applied board is there"));
                b.undo(&applied.inverse).expect("an applied batch's inverse applies");
            }
            DesignApplied::Sys { inverse } => {
                self.apply_sys(inverse).expect("an applied system op's inverse applies");
            }
        }
    }

    fn apply_sys(&mut self, s: &SysOp) -> Result<SysOp, DesignOpError> {
        let find = |links: &[Link], id: LinkId| links.iter().position(|l| l.id == id);
        Ok(match s {
            SysOp::AddLink(l) | SysOp::InsertLink { link: l, .. } => {
                if find(&self.links, l.id).is_some() {
                    return Err(DesignOpError::DuplicateLink(l.id));
                }
                let pos = if let SysOp::InsertLink { pos, .. } = s { (*pos as usize).min(self.links.len()) } else { self.links.len() };
                self.links.insert(pos, l.clone());
                SysOp::RemoveLink(l.id)
            }
            SysOp::RemoveLink(id) => {
                let i = find(&self.links, *id).ok_or(DesignOpError::NoLink(*id))?;
                if let Some(m) = self.assembly.mounts.iter().find(|m| m.parent == Frame::Mate(*id)) {
                    return Err(DesignOpError::LinkInUse { link: *id, unit: m.unit });
                }
                let link = self.links.remove(i);
                SysOp::InsertLink { link, pos: i as u32 }
            }
            SysOp::ReplaceLink(l) => {
                let i = find(&self.links, l.id).ok_or(DesignOpError::NoLink(l.id))?;
                SysOp::ReplaceLink(std::mem::replace(&mut self.links[i], l.clone()))
            }
            SysOp::SetMount(m) => {
                if self.unit(m.unit).is_none() {
                    return Err(DesignOpError::NoUnit(m.unit));
                }
                let valid = match &m.parent {
                    Frame::World | Frame::Body { .. } => true,
                    Frame::Unit(b) => *b != m.unit && self.unit(*b).is_some(),
                    Frame::Mate(l) => self.link(*l).is_some(),
                };
                if !valid {
                    return Err(DesignOpError::BadMountParent { unit: m.unit });
                }
                let m = Mount { xform: m.xform.snapped(), ..m.clone() };
                match self.assembly.mounts.iter().position(|x| x.unit == m.unit) {
                    Some(i) => SysOp::SetMount(std::mem::replace(&mut self.assembly.mounts[i], m)),
                    None => {
                        let at = self.assembly.mounts.partition_point(|x| x.unit < m.unit);
                        let unit = m.unit;
                        self.assembly.mounts.insert(at, m);
                        SysOp::ClearMount(unit)
                    }
                }
            }
            SysOp::ClearMount(u) => {
                if self.unit(*u).is_none() {
                    return Err(DesignOpError::NoUnit(*u));
                }
                let m = Mount { unit: *u, parent: Frame::World, xform: Xform3::IDENTITY };
                match self.assembly.mounts.iter().position(|x| x.unit == *u) {
                    Some(i) => SysOp::SetMount(std::mem::replace(&mut self.assembly.mounts[i], m)),
                    None => {
                        let at = self.assembly.mounts.partition_point(|x| x.unit < *u);
                        self.assembly.mounts.insert(at, m.clone());
                        SysOp::SetMount(m)
                    }
                }
            }
            SysOp::SetFactory { unit, profile } => {
                let u = self.units.iter_mut().find(|u| u.id == *unit).ok_or(DesignOpError::NoUnit(*unit))?;
                let old = std::mem::replace(&mut u.factory, profile.clone());
                SysOp::SetFactory { unit: *unit, profile: old }
            }
        })
    }
}
