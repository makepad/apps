//! Wire and placement input, independent of the widget and host. Hits and plans stay on the
//! geometry frozen when the gesture began.

use super::{scene::HitTable, targets::resolve};
use crate::canvas::pane::DRAG_THRESHOLD;
use crate::snapshot::{next_tag, DesignSnapshot, ViewAction};
use fab_elec_circuit::{Circuit, CircuitOp};
use fab_elec_bench::SimCmd;
use fab_elec_core::{BoardId, CompRef, NetId, Sel};
use fab_elec_design::{Design, DesignOp, Resolved};
use fab_elec_explore::{edit::Target, plan::{plan, Gesture, Planned}, plan_drop::{plan_drop, Template}, sheet::TagKind, KeepBase, SheetLayout};
use std::{collections::HashMap, sync::Arc};

/// A pointer press on a button while the run plays: the button is pressed live until the pointer
/// comes up (`SimCmd::press`), and the click that ends the press belongs to it. It does not pick the
/// part: no selection and no reveal (a click on a button while the run is stopped still picks it).
#[derive(Default)]
pub(crate) struct LivePress {
    /// The button held down.
    held: Option<CompRef>,
    /// The button just released, whose click the press takes.
    ended: Option<CompRef>,
}

impl LivePress {
    /// The pointer went down on `comp`; `live` when the run plays and `comp` is a button. The
    /// press to send.
    pub(crate) fn down(&mut self, comp: CompRef, live: bool) -> Option<SimCmd> {
        self.ended = None;
        if !live {
            return None;
        }
        self.held = Some(comp);
        Some(SimCmd::press(comp, true))
    }

    /// The pointer came up: the release to send, when a button was held.
    pub(crate) fn up(&mut self) -> Option<SimCmd> {
        let comp = self.held.take()?;
        self.ended = Some(comp);
        Some(SimCmd::press(comp, false))
    }

    /// Whether a click ends a live press, which takes it: the click does not pick.
    pub(crate) fn takes_click(&mut self, key: Option<&Sel>) -> bool {
        let ended = self.ended.take();
        matches!((key, ended), (Some(Sel::Comp(comp)), Some(ended)) if *comp == ended)
    }

    /// The pointer-owned button; the drawn state comes from SimLive.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Option<CompRef> {
        self.held
    }

    /// Any other input ends the press's claim on a click (the pointer dragged away, say).
    pub(crate) fn other(&mut self) {
        self.ended = None;
    }
}

/// What a gesture reads for hits and plans, from the press to the end of Landing (V1).
pub(super) struct Frozen {
    /// The design the shown sheet was laid out from.
    pub design: Arc<Design>,
    /// Index into `design.units`.
    pub unit: usize,
    /// `design.units[unit].circuit`.
    pub circuit: Arc<Circuit>,
    /// The committed sheet at the press.
    pub sheet: Arc<SheetLayout>,
    /// Its immutable hit lists.
    pub hits: Arc<HitTable>,
}

/// What the gesture carries.
#[derive(Clone)]
pub(super) enum Source {
    /// A pin starts a connection; a wire end carries that pin's connection.
    Wire(Target),
    /// A Library entry riding the pointer: each place under it is a drop plan (`plan_drop`).
    Carry { template: Arc<Template>, base: Arc<KeepBase> },
    /// A power rail riding the pointer (the Power flyout): a pin under it is tied to `net`, as a
    /// wire dragged from the pin onto the rail's tag would be.
    Rail { net: NetId, kind: TagKind },
}

/// Input and host acknowledgement stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    /// Nothing is held.
    Idle,
    /// Pressed, below the drag threshold.
    Armed,
    /// Sampling candidates on frozen geometry during a drag.
    Dragging,
    /// A pin click started a wire. Hover samples the frozen sheet until the next click.
    ClickWire,
    /// Released. Hold the display while the drop gets any keep hints and the host answers its tag.
    Landing,
    /// A second click sent the commit. A late refusal lets the user choose another target.
    ClickLanding,
    /// Cancelled or refused; the widget may retain the reason as a note.
    FlyBack,
}

impl Phase {
    /// Both forms of wiring use the same live target, preview and drawing.
    pub(super) fn sampling(self) -> bool { matches!(self, Self::Dragging | Self::ClickWire) }

    /// A tagged commit is awaiting the host's answer.
    pub(super) fn landing(self) -> bool { matches!(self, Self::Landing | Self::ClickLanding) }
}

/// The chip and rubber wire's semantic look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChipLook {
    /// Opening instructions or a plan that changes nothing.
    Plain,
    /// A candidate edit.
    Edit,
    /// A planning or tagged host refusal.
    Refused,
}

/// The host's answer read from a snapshot.
#[derive(Debug, PartialEq)]
pub(super) enum Answer {
    /// The live preview shows this sample.
    Shown(u64),
    /// The host refused this sample.
    Refused,
    /// The host committed this sample, possibly creating selections.
    Committed {
        /// New selections reported by the matching commit.
        created: Vec<Sel>,
    },
    /// The frozen circuit's positional ids no longer belong to the committed circuit.
    BaseMoved,
}

/// A released edit ends only on its tagged answer or a change to its committed base.
pub(super) fn landed(snap: &DesignSnapshot, tag: u64, unit: BoardId, circuit: &Arc<Circuit>) -> Option<Answer> {
    if let Some(commit) = snap.last_commit.as_ref().filter(|commit| commit.tag == Some(tag)) {
        Some(Answer::Committed { created: commit.created.clone() })
    } else if snap.last_refusal.as_ref().is_some_and(|refusal| refusal.sample == Some(tag)) {
        Some(Answer::Refused)
    } else if !snap.design.unit(unit).is_some_and(|unit| Arc::ptr_eq(&unit.circuit, circuit)) {
        Some(Answer::BaseMoved)
    } else { None }
}

/// Frozen nodes whose old places still need ink while the live sheet reflows.
pub(super) fn moved_parts(frozen_circuit: &Circuit, frozen_sheet: &SheetLayout,
    shown_circuit: &Circuit, shown_sheet: &SheetLayout) -> Vec<bool>
{
    let mut by_path = ByPath::new(shown_circuit);
    frozen_circuit.comps.iter().enumerate().map(|(node, part)| {
        let Some(frozen) = frozen_sheet.parts.get(node).filter(|part| part.placed) else { return false };
        let shown = by_path.find(node, &part.path).and_then(|index| shown_sheet.parts.get(index));
        !shown.is_some_and(|shown| shown.placed && shown.at == frozen.at
            && shown.turn == frozen.turn && shown.mirror == frozen.mirror)
    }).collect()
}

/// A circuit's part index by path: first at the same index (an edit that adds or removes no part
/// keeps every index), else through a map built on the first miss, so a whole sheet's lookups stay
/// O(parts).
pub(super) struct ByPath<'a> {
    circuit: &'a Circuit,
    map: Option<HashMap<&'a str, usize>>,
}

impl<'a> ByPath<'a> {
    pub(super) fn new(circuit: &'a Circuit) -> Self { Self { circuit, map: None } }

    /// The index of the part at `path`, which sat at `at` in the circuit it came from.
    pub(super) fn find(&mut self, at: usize, path: &str) -> Option<usize> {
        if self.circuit.comps.get(at).is_some_and(|part| part.path == path) { return Some(at); }
        let circuit = self.circuit;
        self.map.get_or_insert_with(|| circuit.comps.iter().enumerate().map(|(n, part)| (part.path.as_str(), n)).collect())
            .get(path).copied()
    }
}

/// The output of every input: at most one action (V2) and the phase after it.
pub(super) struct Step {
    /// The single action to send to the host, if any.
    pub action: Option<ViewAction>,
    /// The resulting phase.
    pub phase: Phase,
    /// An empty-sheet release opens the field for this source at the actual pointer point.
    pub name_end: Option<(Target, [f64; 2])>,
}

/// Pure wire input. Arc clones happen at the press only; at rest there is no work.
pub(super) struct EditGesture {
    /// The only source of hit geometry and planning ids.
    frozen: Frozen,
    /// What was pressed.
    source: Source,
    /// Current input/acknowledgement stage.
    phase: Phase,
    /// Actual press point for the screen-space threshold.
    from: [f64; 2],
    /// The source pin's committed outer end.
    origin: [f64; 2],
    /// Latest pointer point.
    at: [f64; 2],
    /// Last resolved target, including attachment geometry and hysteresis.
    target: Target,
    /// Last candidate, compared before sending an action.
    planned: Planned,
    /// Candidate choice, shared with the future Carry input.
    pick: usize,
    /// Wire has one candidate.
    count: usize,
    /// Chip shown before there is a plan.
    opening: String,
    /// Cached chip; pointer-only samples allocate nothing.
    chip: String,
    /// Cached chip styling.
    look: ChipLook,
    /// The sample currently owned by this gesture.
    tag: Option<u64>,
    /// Whether the last snapshot shows that sample.
    shown: bool,
    /// A host or layout refusal of exactly that sample.
    refusal: Option<String>,
    /// Latest press received while Landing.
    deferred: Option<[f64; 2]>,
    /// Quarter turns counter-clockwise for a carried part placed on the canvas (R turns it).
    turn: u8,
    /// Reflect the carried symbol left-right before applying its turn.
    mirror: bool,
}

impl EditGesture {
    /// Arm a pin or wire end with a hit in the frozen sheet, without sending anything (V3).
    pub(super) fn begin(frozen: Frozen, source: Source, from: [f64; 2]) -> Option<Self> {
        let Source::Wire(target) = source else { return None };
        let (comp, pin) = match target {
            Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => (comp, pin),
            _ => return None,
        };
        let origin = frozen.hits.pins.iter()
            .find(|hit| hit.node == comp.0 && hit.pin == u32::from(pin.0))?.outer;
        let opening = frozen.circuit.comps.get(comp.ix()).and_then(|part| {
            let name = &part.part.pins.get(pin.ix())?.name;
            let refdes = &part.refdes;
            match target {
                Target::WireEnd { net, .. } => frozen.circuit.nets.get(net.ix())
                    .map(|net| format!("move {} off {refdes}.{name}", net.name)),
                _ => Some(format!("wire from {refdes}.{name}")),
            }
        }).unwrap_or_default();
        Some(Self {
            frozen, source, phase: Phase::Armed, from, origin, at: from, target,
            planned: Planned::Nothing { chip: String::new() }, pick: 0, count: 1,
            chip: opening.clone(), opening, look: ChipLook::Plain,
            tag: None, shown: false, refusal: None, deferred: None, turn: 0, mirror: false,
        })
    }

    /// Carry a Library entry, titled `title`, from `at`: there is no press to arm, so it samples
    /// at once. Copy the frozen sheet once for all of this carry's worker requests.
    pub(super) fn carry(frozen: Frozen, template: Arc<Template>, title: &str, at: [f64; 2]) -> Self {
        let base = Arc::new(KeepBase { sheet: (*frozen.sheet).clone(), circuit: frozen.circuit.clone() });
        Self::carry_from(frozen, Source::Carry { template, base }, format!("Place {title} · R turns · X mirrors · Esc"), at)
    }

    /// A carried `source` at `at`, dragging from the start; `opening` is the chip before a plan.
    fn carry_from(frozen: Frozen, source: Source, opening: String, at: [f64; 2]) -> Self {
        Self {
            frozen, source, phase: Phase::Dragging, from: at, origin: at, at,
            target: Target::None, planned: Planned::Nothing { chip: String::new() }, pick: 0, count: 1,
            chip: opening.clone(), opening, look: ChipLook::Plain,
            tag: None, shown: false, refusal: None, deferred: None, turn: 0, mirror: false,
        }
    }

    /// A rail from the Power flyout, carried from world `at`: each pin under the pointer previews
    /// being tied to it, a click ties it with one commit.
    pub(super) fn rail(frozen: Frozen, net: NetId, kind: TagKind, name: &str, at: [f64; 2]) -> Self {
        Self::carry_from(frozen, Source::Rail { net, kind }, format!("Tie a pin to {name} · Esc"), at)
    }

    /// One resolve per pointer sample; plan and action only on a changed candidate (V6).
    pub(super) fn sample(&mut self, at: [f64; 2], px_per_unit: f64) -> Step {
        if self.phase == Phase::Armed {
            self.at = at;
            if (at[0] - self.from[0]).hypot(at[1] - self.from[1]) * px_per_unit <= DRAG_THRESHOLD {
                return self.step(None);
            }
            self.phase = Phase::Dragging;
        }
        if !self.phase.sampling() { return self.step(None); }
        self.at = at;
        let target = resolve(&self.frozen().hits, &self.frozen().sheet, at, px_per_unit, &self.target());
        let changed = match self.source {
            // A carried part lands on the grid cell under the pointer: every cell is its own plan.
            Source::Carry { .. } => !same_place(self.target(), target),
            Source::Wire(_) => !same_aim(self.target(), target)
                || matches!(self.target, Target::Canvas { .. }) != matches!(target, Target::Canvas { .. }),
            Source::Rail { .. } => !same_aim(self.target(), target),
        };
        self.target = target;
        if !changed { return self.step(None); }
        self.pick = 0;
        self.replan()
    }

    /// Cycle candidate choice; a wire's single candidate leaves its plan and tag unchanged.
    pub(super) fn tab(&mut self, back: bool) -> Step {
        if self.phase != Phase::Dragging || self.count <= 1 { return self.step(None); }
        self.pick = if back { (self.pick + self.count - 1) % self.count } else { (self.pick + 1) % self.count };
        self.set_chip();
        self.replan()
    }

    /// A pin click starts a wire. A second click or a drag release commits the live sample, even
    /// before its preview arrives. A wire (clicked from a pin, or dragged from a pin or a wire end)
    /// that ends on empty sheet ends at an inline name field.
    pub(super) fn release(&mut self, px_per_unit: f64) -> Step {
        if let (Phase::ClickWire | Phase::Dragging, Source::Wire(from), Target::Canvas { .. }) =
            (self.phase, &self.source, self.target)
        {
            let end = (*from, self.at);
            self.phase = Phase::Idle;
            return Step { action: self.cancel(), phase: Phase::Idle, name_end: Some(end) };
        }
        let action = match self.phase {
            Phase::Armed if matches!(self.source, Source::Wire(Target::Pin { .. })) => {
                self.phase = Phase::ClickWire;
                None
            }
            Phase::Armed => { self.phase = Phase::Idle; None }
            Phase::ClickWire if self.on_start_pin(px_per_unit) => return self.escape(),
            Phase::Dragging | Phase::ClickWire if self.tag().is_some() && self.refusal().is_none() => {
                self.phase = if self.phase == Phase::ClickWire { Phase::ClickLanding } else { Phase::Landing };
                Some(ViewAction::CommitPreview)
            }
            Phase::Dragging if !self.carries() => {
                self.phase = if self.note().is_some() { Phase::FlyBack } else { Phase::Idle };
                self.cancel()
            }
            _ => None,
        };
        self.step(action)
    }

    /// R turns a carried part a quarter clockwise (Shift: counter-clockwise), as R turns a
    /// selection at rest. Only a part placed on the canvas takes a turn of its own; on a pin it
    /// follows its host.
    pub(super) fn turn(&mut self, back: bool) -> Step {
        if self.phase != Phase::Dragging || !self.carries() { return self.step(None); }
        self.turn = (self.turn + if back { 1 } else { 3 }) % 4;
        if !matches!(self.target, Target::Canvas { .. }) { return self.step(None); }
        self.replan()
    }

    /// X mirrors the carried part and Y also turns it halfway. Only a canvas drop needs replanning.
    pub(super) fn mirror(&mut self, flip_vertical: bool) -> Step {
        if self.phase != Phase::Dragging || !matches!(self.source, Source::Carry { .. }) { return self.step(None); }
        self.mirror = !self.mirror;
        if flip_vertical { self.turn = (self.turn + 2) % 4; }
        if !matches!(self.target, Target::Canvas { .. }) { return self.step(None); }
        self.replan()
    }

    /// Whether the gesture carries a Library entry or a rail (no rubber wire, no drag threshold).
    pub(super) fn carries(&self) -> bool { matches!(self.source, Source::Carry { .. } | Source::Rail { .. }) }

    /// The sheet this Library carry keeps for every preview and for its drop.
    pub(super) fn keep_base(&self) -> Option<&Arc<KeepBase>> {
        match &self.source { Source::Carry { base, .. } => Some(base), _ => None }
    }

    /// The released Library edit and the tag of the preview to keep.
    pub(super) fn carry_drop(&self) -> Option<(u64, &Resolved, &Arc<KeepBase>)> {
        let (Phase::Landing, Planned::Edit(edit)) = (self.phase, &self.planned) else { return None };
        Some((self.tag?, edit, self.keep_base()?))
    }

    /// Follow the preview that adds keep hints to this Library drop.
    pub(super) fn retag(&mut self, old: u64, new: u64) {
        if self.carry_drop().is_some_and(|(tag, _, _)| tag == old) {
            self.tag = Some(new);
            self.shown = false;
        }
    }

    /// A Library drop with no room cancels its preview and flies back with the worker's note.
    pub(super) fn refuse_keep(&mut self, tag: u64, note: String) -> Step {
        if !self.carry_drop().is_some_and(|(old, _, _)| old == tag) { return self.step(None); }
        self.refusal = Some(note);
        self.set_chip();
        self.phase = Phase::FlyBack;
        let action = self.cancel();
        self.step(action)
    }

    /// Escape cancels input. Once a commit was sent, only the host can end Landing.
    pub(super) fn escape(&mut self) -> Step {
        let action = match self.phase {
            Phase::Armed => { self.phase = Phase::Idle; None }
            Phase::ClickWire => { self.phase = Phase::Idle; self.cancel() }
            // A carry put down leaves nothing behind: no note to fly back with.
            Phase::Dragging if self.carries() => { self.phase = Phase::Idle; self.cancel() }
            Phase::Dragging => { self.phase = Phase::FlyBack; self.cancel() }
            // The host already owns the commit. A refusal after Escape ends the wire too.
            Phase::ClickLanding => { self.phase = Phase::Landing; None }
            _ => None,
        };
        self.step(action)
    }

    /// Match host answers by tag, never by preview disappearance or a timer.
    pub(super) fn snapshot(&mut self, snap: &DesignSnapshot) -> (Step, Option<Answer>) {
        self.shown = self.tag().is_some() && snap.preview_tag == self.tag();
        if self.phase.landing() {
            let answer = self.tag.and_then(|tag| landed(snap, tag,
                self.frozen.design.units[self.frozen.unit].id, &self.frozen.circuit));
            match &answer {
                Some(Answer::Committed { .. } | Answer::BaseMoved) => self.phase = Phase::Idle,
                Some(Answer::Refused) => {
                    if let Some(refusal) = &snap.last_refusal {
                        self.refusal = Some(refusal.reason.clone());
                        self.set_chip();
                    }
                    self.phase = if self.phase == Phase::ClickLanding && self.same_base(snap) {
                        self.tag = None;
                        self.shown = false;
                        self.deferred = None;
                        Phase::ClickWire
                    } else { Phase::FlyBack };
                }
                _ => {}
            }
            return (self.step(None), answer);
        }
        if !self.phase.sampling() && self.phase != Phase::Armed {
            return (self.step(None), None);
        }
        // A drag's ids die first; in Landing its own commit/refusal wins over the base change.
        if !self.same_base(snap) {
            self.phase = Phase::Idle;
            let action = self.cancel();
            return (self.step(action), Some(Answer::BaseMoved));
        }
        if let Some(refusal) = snap.last_refusal.as_ref().filter(|refusal| self.tag.is_some() && refusal.sample == self.tag) {
            if self.refusal.as_deref() != Some(refusal.reason.as_str()) {
                self.refusal = Some(refusal.reason.clone());
                self.set_chip();
            }
            return (self.step(None), Some(Answer::Refused));
        }
        let answer = if self.shown() { self.tag().map(Answer::Shown) } else { None };
        (self.step(None), answer)
    }

    /// Retain only the latest press while the commit is landing.
    pub(super) fn defer(&mut self, from: [f64; 2]) {
        if self.phase.landing() { self.deferred = Some(from); }
    }

    /// Take that press once, only after Landing has ended.
    pub(super) fn take_deferred(&mut self) -> Option<[f64; 2]> {
        if self.phase.landing() { None } else { self.deferred.take() }
    }

    /// Current input/acknowledgement stage.
    pub(super) fn phase(&self) -> Phase { self.phase }

    /// Source pin's committed outer end.
    pub(super) fn from_point(&self) -> [f64; 2] { self.origin }

    /// Pointer while dragging; committed target attachment while Landing or flying back.
    pub(super) fn end_point(&self) -> [f64; 2] {
        if !self.phase.landing() && self.phase != Phase::FlyBack { return self.at; }
        self.aim_point().unwrap_or(self.at)
    }

    /// The target's frozen attachment, shared by the accent and the released rubber wire.
    pub(super) fn aim_point(&self) -> Option<[f64; 2]> {
        match self.target {
            Target::Pin { comp, pin, .. } | Target::WireEnd { comp, pin, .. } => self.frozen.hits.pins.iter()
                .find(|hit| hit.node == comp.0 && hit.pin == u32::from(pin.0)).map(|hit| hit.outer),
            Target::Tag { net, kind } => self.frozen.hits.tags.iter()
                .find(|hit| hit.net == net.0 && hit.kind == kind).map(|hit| hit.at),
            Target::Wire { at, .. } => Some(at.map(f64::from)),
            _ => None,
        }
    }

    /// Frozen target, including its current attachment point.
    pub(super) fn target(&self) -> Target { self.target }

    /// Cached chip and styling, present until the widget drops the gesture.
    pub(super) fn chip(&self) -> Option<(&str, ChipLook)> { Some((&self.chip, self.look)) }

    /// A reason worth retaining after FlyBack; opening instructions and edit labels are not notes.
    pub(super) fn note(&self) -> Option<(&str, ChipLook)> {
        match &self.planned {
            _ if matches!(self.source, Source::Wire(_)) && matches!(self.target, Target::Canvas { .. }) => None,
            _ if self.refusal.is_some() => self.chip(),
            Planned::Refused { .. } => self.chip(),
            Planned::Nothing { chip } if !chip.is_empty() => self.chip(),
            _ => None,
        }
    }

    /// Current sample tag.
    pub(super) fn tag(&self) -> Option<u64> { self.tag }

    /// The last snapshot's preview is the current sample.
    pub(super) fn shown(&self) -> bool { self.shown }

    /// The current sample's host refusal, if received.
    pub(super) fn refusal(&self) -> Option<&str> { self.refusal.as_deref() }

    /// Press geometry for the draw side.
    pub(super) fn frozen(&self) -> &Frozen { &self.frozen }

    /// On a finishing click the start pin also takes a direct hit when hysteresis still holds
    /// the wire or tag over it. At most one extra frozen hit test, only on that click.
    fn on_start_pin(&self, px_per_unit: f64) -> bool {
        let Source::Wire(Target::Pin { comp, pin, .. }) = self.source else { return false };
        let starts_here = |target| matches!(target, Target::Pin { comp: c, pin: p, .. }
            | Target::WireEnd { comp: c, pin: p, .. } if (c, p) == (comp, pin));
        starts_here(self.target) || starts_here(resolve(&self.frozen.hits, &self.frozen.sheet, self.at, px_per_unit, &Target::None))
    }

    /// Frozen pin and net ids still name the committed circuit they came from.
    fn same_base(&self, snap: &DesignSnapshot) -> bool {
        self.frozen.design.units.get(self.frozen.unit)
            .and_then(|unit| snap.design.unit(unit.id))
            .is_some_and(|unit| Arc::ptr_eq(&unit.circuit, &self.frozen.circuit))
    }

    /// Re-plan once per aim/choice change, sending at most one action for a different plan.
    fn replan(&mut self) -> Step {
        let planned = match &self.source {
            Source::Wire(_) if matches!(self.target, Target::Canvas { .. }) =>
                Planned::Nothing { chip: "finish here to name the net".into() },
            Source::Wire(from) => plan(&self.frozen.circuit, &Gesture::Wire { from: *from, to: self.target }),
            Source::Carry { template, .. } => {
                // A drop on a wire needs the drawn net tree's two sides (`plan_drop`); the frozen
                // hits have none, so a two-terminal part plans nothing there.
                let dropped = plan_drop(&self.frozen.circuit, template, self.target, self.pick, None);
                self.count = dropped.count.max(1);
                let mut planned = dropped.planned;
                if let Planned::Edit(resolved) = &mut planned {
                    for op in &mut resolved.ops {
                        if let DesignOp::Circuit { op: CircuitOp::AddPart { hint: Some(hint), .. }, .. } = op {
                            hint.turn = self.turn;
                            hint.mirror = self.mirror;
                        }
                    }
                }
                planned
            }
            // Only a pin (or a connected pin's end) takes a rail; the canvas plans nothing.
            Source::Rail { net, kind } => match self.target {
                Target::Pin { .. } | Target::WireEnd { .. } =>
                    plan(&self.frozen.circuit, &Gesture::Wire { from: self.target, to: Target::Tag { net: *net, kind: *kind } }),
                _ => Planned::Nothing { chip: String::new() },
            },
        };
        if planned == self.planned { return self.step(None); }
        self.planned = planned;
        self.refusal = None;
        self.shown = false;
        self.set_chip();
        let action = match &self.planned {
            Planned::Edit(r) => {
                let tag = next_tag();
                self.tag = Some(tag);
                Some(ViewAction::Preview { notes: vec![], tag, ops: r.ops.clone(), place: r.place.clone(), label: r.label.clone() })
            }
            _ => self.cancel(),
        };
        self.step(action)
    }

    /// Clear the host slot only when this gesture owns a live sample.
    fn cancel(&mut self) -> Option<ViewAction> {
        self.shown = false;
        self.tag.take().map(|_| ViewAction::CancelPreview)
    }

    /// Cache text only on a candidate change or its host refusal, never on pointer-only samples.
    fn set_chip(&mut self) {
        let (text, look) = if let Some(refusal) = &self.refusal {
            (refusal.as_str(), ChipLook::Refused)
        } else {
            match &self.planned {
                Planned::Edit(_) if self.carries() && matches!(self.target, Target::Canvas { .. }) =>
                    (self.opening.as_str(), ChipLook::Edit),
                Planned::Edit(r) => (r.label.as_str(), ChipLook::Edit),
                Planned::Nothing { .. } if self.phase == Phase::ClickWire && self.target == Target::None =>
                    ("click a pin, wire or tag", ChipLook::Plain),
                Planned::Nothing { chip } => (if chip.is_empty() { self.opening.as_str() } else { chip.as_str() }, ChipLook::Plain),
                Planned::Refused { chip, .. } => (chip.as_str(), ChipLook::Refused),
            }
        };
        self.chip.clear();
        self.chip.push_str(text);
        if self.count > 1 {
            use std::fmt::Write;
            let _ = write!(self.chip, " · {} of {}", self.pick + 1, self.count);
        }
        self.look = look;
    }

    /// Finish one input without hiding extra actions in the state machine.
    fn step(&self, action: Option<ViewAction>) -> Step { Step { action, phase: self.phase, name_end: None } }
}

/// A carry's places: the same pin, the same net, or the same canvas cell of the same block.
fn same_place(a: Target, b: Target) -> bool {
    match (a, b) {
        (Target::Canvas { block: a, cell: ac }, Target::Canvas { block: b, cell: bc }) => a == b && ac == bc,
        (Target::None, Target::None) => true,
        (Target::Canvas { .. } | Target::None, _) | (_, Target::Canvas { .. } | Target::None) => false,
        _ => same_aim(a, b),
    }
}

/// Wire aims ignore approach direction and geometry within a net; empty aims are equivalent.
fn same_aim(a: Target, b: Target) -> bool {
    match (a, b) {
        (Target::Pin { comp: a, pin: ap, .. }, Target::Pin { comp: b, pin: bp, .. })
        | (Target::WireEnd { comp: a, pin: ap, .. }, Target::WireEnd { comp: b, pin: bp, .. }) => a == b && ap == bp,
        (Target::Wire { net: a, .. } | Target::Tag { net: a, .. }, Target::Wire { net: b, .. } | Target::Tag { net: b, .. }) => a == b,
        (Target::Canvas { .. } | Target::None, Target::Canvas { .. } | Target::None) => true,
        _ => false,
    }
}

#[cfg(test)]
#[path = "gesture_tests.rs"]
pub(in crate::explorer) mod tests;
