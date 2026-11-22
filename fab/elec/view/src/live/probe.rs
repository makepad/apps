//! The probe gesture's seam (I-D). The bench starts an in-app drag of [`probe_drag_item`] and draws
//! the probe riding the pointer; a pane that shows nets calls [`probe_target`] from its
//! `handle_event` with its own hit test, and [`probe_key`] for P over a net. On a drop or a P the
//! pane emits `BenchOp::AddProbe { of, label }` through its usual op path, the label [`probe_label`]
//! on a pin and the net's name elsewhere.
//!
//! While a probe is over a net, the pane names it in the [`ProbeGesture`] Cx global. The bench reads
//! the global when it draws (the ghost flag where the drop would put the probe, the chip's net name
//! and volts) and lights the net in every pane, so the order in which widgets see a drag event
//! does not matter.
//!
//! A pane no host names a bench for (`set_bench`) probes into the bench the electronics host runs
//! ([`host_bench`]); a probe removed from any view goes from that bench's document as one undo
//! step ([`remove_probe_op`]), so it leaves every view at once.

use crate::{live::flags::{named_spot, OpenInScope, PinSpot}, snapshot::{ElecLive, ViewAction}};
use fab_elec_circuit::Circuit;
use fab_elec_core::NetId;
use fab_elec_design::{BenchOp, Design, DesignOp, ProbeOf, ProbeQuantity};
use makepad_widgets_core::*;

/// The bench new probes go to when no host names one: the bench the electronics host runs
/// ([`ElecLive::run_bench`]), by name. None without a host or a bench that
/// evaluated. One global read and O(benches); called on probe events, never per frame.
pub fn host_bench(cx: &Cx) -> Option<String> {
    let live = cx.get_global_ref::<ElecLive>()?;
    live.run_bench().map(|bench| bench.out.name.clone())
}

/// The edit that removes the probe of `net` from bench `bench`, whose scope channels are
/// `probes`: its voltage channel of the net against ground (a flag's kind), one undo step named
/// after its label. None when the bench has no such channel.
pub fn remove_probe_op(bench: &str, probes: &[fab_elec_lang::BenchProbe], net: &str) -> Option<ViewAction> {
    let probe = probes.iter().find(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() && probe.of.target == net)?;
    let of: ProbeOf = probe.of.clone();
    Some(ViewAction::Ops {
        place: vec![],
        label: format!("Remove probe {}", probe.label.lines().next().unwrap_or(net)),
        ops: vec![DesignOp::Bench { bench: bench.to_owned(), op: BenchOp::RemoveProbe { of } }],
    })
}

/// The flag of `net`'s probe carried along its net and let go at the pin `at` (`"<refdes>.<pad>"`):
/// the edit that places it there (`BenchOp::SetProbeAt`), one undo step, so the place is saved with
/// the probe and every drawing shows it there. None for a net the bench does not probe, or a flag
/// already at `at`.
pub fn place_probe_op(bench: &str, probes: &[fab_elec_lang::BenchProbe], net: &str, at: &str) -> Option<ViewAction> {
    let probe = probes.iter().find(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() && probe.of.target == net)?;
    (probe.at.as_deref() != Some(at)).then(|| ViewAction::Ops {
        place: vec![],
        label: format!("Move probe {} to {at}", probe.label.lines().next().unwrap_or(net)),
        ops: vec![DesignOp::Bench { bench: bench.to_owned(), op: BenchOp::SetProbeAt { of: probe.of.clone(), at: Some(at.to_owned()) } }],
    })
}

/// A click on a wire (or, while a run is live, copper) of `net`: the edit that probes it into bench
/// `bench`, whose scope channels are `probes`, named by the net (rename it in the Scope), one undo
/// step. None when the bench probes the net's voltage already: the click then only shows its lane.
pub fn click_probe_op(bench: &str, probes: &[fab_elec_lang::BenchProbe], net: &str) -> Option<ViewAction> {
    let probed = probes.iter().any(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() && probe.of.target == net);
    (!probed).then(|| ViewAction::Ops {
        place: vec![],
        label: format!("Probe {net}"),
        ops: vec![DesignOp::Bench { bench: bench.to_owned(), op: BenchOp::AddProbe { of: ProbeOf::v(net.to_owned()), label: net.to_owned() } }],
    })
}

/// A wire or copper of `net` clicked: the click selects it where it was made and nothing else
/// (user, 11-18: a click "now jumps me to 'scope'"); its lane in the Scope, when it has one, comes
/// into view there without the Scope coming forward ([`OpenInScope`]). It edits no bench: a probe
/// is kept on purpose (the Probe tool, `P`). O(1), on a click.
pub fn probe_on_click(cx: &mut Cx, net: &str) {
    cx.action(OpenInScope(net.to_owned()));
}

/// A lane renamed to `text` in the Scope: the edit that writes the probe's label in bench
/// `bench` (`BenchOp::SetProbeLabel`), one undo step, so the name lives in the bench source and
/// comes back with every evaluation (a reload). With `keep` (the pin its flag stands at by its old
/// name, [`rename_keeps`]) the same step writes that pin as its `at`, so the flag stays where it
/// is under its new name. None when the trimmed text is empty or unchanged.
pub fn rename_probe_op(bench: &str, of: &ProbeOf, label: &str, text: &str, keep: Option<&str>) -> Option<ViewAction> {
    let name = text.trim();
    (!name.is_empty() && name != label).then(|| {
        let mut ops = vec![DesignOp::Bench { bench: bench.to_owned(), op: BenchOp::SetProbeLabel { of: of.clone(), label: name.to_owned() } }];
        if let Some(at) = keep {
            ops.push(DesignOp::Bench { bench: bench.to_owned(), op: BenchOp::SetProbeAt { of: of.clone(), at: Some(at.to_owned()) } });
        }
        ViewAction::Ops { place: vec![], label: format!("Rename probe {label} to {name}"), ops }
    })
}

/// The pin `probe`'s flag must be pinned at (its `at`) for a rename to `text` to leave it where it
/// is: the pin its old name names on its net when it has no `at` of its own and the new name names
/// another place. None when nothing would move. O(pins of the design's components).
pub fn rename_keeps(design: &Design, probe: &fab_elec_lang::BenchProbe, text: &str) -> Option<String> {
    if probe.at.is_some() { return None; }
    let net = &probe.of.target;
    let circuit = design.units.iter().map(|unit| &unit.circuit).find(|circuit| circuit.nets.iter().any(|n| &n.name == net))?;
    let old = named_pin(circuit, net, &probe.label)?;
    (named_pin(circuit, net, text.trim()).as_ref() != Some(&old)).then_some(old)
}

/// The pin of circuit net `net` that probe name `name` names ([`named_spot`]), written as a probe's
/// `at` writes it (`"<refdes>.<pad>"`); None when it names none of the net's pins or parts.
pub fn named_pin(circuit: &Circuit, net: &str, name: &str) -> Option<String> {
    let id = circuit.nets.iter().position(|n| n.name == net)?;
    let spots: Vec<PinSpot> = circuit.pins_of(NetId(id as u32)).into_iter().filter_map(|(comp, pin)| {
        let comp = circuit.comps.get(comp.ix())?;
        Some(PinSpot { refdes: &comp.refdes, number: comp.part.pins.get(pin.ix())?.pads.first()?, part_pins: comp.part.pins.len() })
    }).collect();
    named_spot(name, &spots).map(|k| spots[k].name())
}

/// The internal id every probe drag carries. A pane takes a drag only when one of its items has it.
pub const PROBE_DRAG: LiveId = live_id!(elec_probe);

/// The drag item the bench starts a probe drag with.
pub fn probe_drag_item() -> DragItem {
    DragItem::String { value: "probe".into(), internal_id: Some(PROBE_DRAG) }
}

/// Whether a drag's items carry a probe.
pub fn carries_probe(items: &[DragItem]) -> bool {
    items.iter().any(|item| matches!(item, DragItem::String { internal_id: Some(id), .. } if *id == PROBE_DRAG))
}

/// The probe gesture as the panes report it: `cx.global::<ProbeGesture>()`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeGesture {
    /// The lowered name of the net under a dragged probe, and the pane that reported it.
    pub hover: Option<(String, WidgetUid)>,
    /// A capability refusal from the target under the carried probe.
    pub refusal: Option<String>,
}

impl ProbeGesture {
    /// The net under the dragged probe.
    pub fn hovered(&self) -> Option<&str> {
        self.hover.as_ref().map(|(net, _)| net.as_str())
    }
}

/// Name `net` as the net under the probe in `pane`. Clearing only clears what `pane` reported, so
/// a pane the pointer leaves never clears the pane it enters. Panes are told apart by widget uid:
/// an `Area` changes with every redraw.
fn report(cx: &mut Cx, pane: WidgetUid, net: Option<String>) {
    let gesture = cx.global::<ProbeGesture>();
    match net {
        Some(net) => gesture.hover = Some((net, pane)),
        None => {
            if gesture.hover.as_ref().is_some_and(|(_, from)| *from == pane) {
                gesture.hover = None;
            }
        }
    }
}

/// One call from a pane's `handle_event` for the area that shows its nets; `pane` is the pane's
/// widget uid. `net_at` is the pane's own hit test: the lowered name of the net at an absolute
/// position. Keeps [`ProbeGesture`] current, takes a probe over a net (`DragResponse::Copy`), and
/// returns the net a probe is dropped on. Drags and drops of anything else are left alone for the
/// pane's own handling.
pub fn probe_target(
    cx: &mut Cx,
    event: &Event,
    area: Area,
    pane: WidgetUid,
    mut net_at: impl FnMut(DVec2) -> Option<String>,
) -> Option<String> {
    match event {
        Event::Drag(drag) if carries_probe(&drag.items) => {
            if let DragHit::Drag(hit) = event.drag_hits(cx, area) {
                let net = match hit.state {
                    DragState::In | DragState::Over => net_at(hit.abs),
                    DragState::Out => None,
                };
                *hit.response.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    if net.is_some() { DragResponse::Copy } else { DragResponse::None };
                report(cx, pane, net);
            }
            None
        }
        Event::Drop(drop) if carries_probe(&drop.items) => {
            let dropped = match event.drag_hits(cx, area) {
                DragHit::Drop(hit) => net_at(hit.abs),
                _ => None,
            };
            report(cx, pane, None);
            dropped
        }
        Event::DragEnd => {
            report(cx, pane, None);
            None
        }
        _ => None,
    }
}

/// P, pressed while `area` has the key focus, probes `net`: the net under the pointer, or the net
/// the keyboard has focused in the pane. Without a net, P does nothing and says nothing. Repeats
/// and presses with a modifier are not P.
pub fn probe_key(cx: &Cx, event: &Event, area: Area, net: Option<&str>) -> Option<String> {
    let Event::KeyDown(key) = event else { return None };
    let m = key.modifiers;
    if key.key_code != KeyCode::KeyP || key.is_repeat || m.shift || m.control || m.alt || m.logo || !cx.has_key_focus(area) {
        return None;
    }
    net.map(str::to_owned)
}

/// [`probe_key`] for a pane that also calls [`super::peek::peek_target`]: P probes on its release,
/// and not at all when it was held for a peek (a press while it was down). Such a pane passes the
/// net under the pointer on the key release.
pub fn probe_key_release(cx: &Cx, event: &Event, area: Area, net: Option<&str>) -> Option<String> {
    let Event::KeyUp(key) = event else { return None };
    let m = key.modifiers;
    if key.key_code != KeyCode::KeyP || m.shift || m.control || m.alt || m.logo || !cx.has_key_focus(area) {
        return None;
    }
    if cx.get_global_ref::<super::peek::PeekGesture>().is_some_and(|gesture| gesture.state.p_used) { return None; }
    net.map(str::to_owned)
}

/// A probe channel's label on a component's pin, the same in every pane (schematic and board):
/// "<refdes>.<pin number>" (R1.1, U1.14), the pin's first pad as its number. A pin with a name of
/// its own (not its number) adds it on a second line, "U1.14\nPB5"; the bench card splits there.
/// `None` for a pin the part does not have.
pub fn probe_label(comp: &fab_elec_circuit::Component, pin: usize) -> Option<String> {
    let def = comp.part.pins.get(pin)?;
    let number = def.pads.first().map_or_else(|| (pin + 1).to_string(), Clone::clone);
    let name = def.name.trim();
    Some(if name.is_empty() || name == "~" || name == number {
        format!("{}.{number}", comp.refdes)
    } else {
        format!("{}.{number}\n{name}", comp.refdes)
    })
}

/// A typed carried probe; hosts omit the current affordance when `Caps.powers.current` is false.
pub fn current_probe_drag_item() -> DragItem {
    DragItem::String { value: "current probe".into(), internal_id: Some(PROBE_DRAG) }
}

/// Refuse a carried capability at a source before committing a drop.
pub fn source_refusal(caps: &fab_elec_bench::source::Caps, items: &[DragItem]) -> Option<&'static str> {
    if !caps.powers.current && items.iter().any(|item| matches!(item, DragItem::String { value, internal_id: Some(id) } if *id == PROBE_DRAG && value == "current probe")) {
        Some("It cannot measure current.")
    } else if !caps.powers.probe_any_net && carries_probe(items) { Some("It cannot probe a net without an input connection.") }
    else { None }
}
