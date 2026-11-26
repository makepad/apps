//! Pastes and Library drops keep the drawn sheet. The worker lays out the host's preview against
//! the original sheet and returns the hints to commit with the edit. A paste holds the display
//! from the press. A carry shows each kept layout and holds the display from release.
//!
//! A drop uses the hints of its displayed preview. When the last sample is still being evaluated
//! it waits for its tag, as a paste does. Computing the hints takes up to three worker layouts.

use super::ExplorerView;
use crate::{ElecLive, ViewAction};
use super::super::gesture::{landed, Answer};
use super::super::layout_job::KeepHints;
use crate::snapshot::{next_tag, Invocation};
use fab_elec_circuit::{Circuit, CircuitOp};
use fab_elec_core::BoardId;
use fab_elec_design::{DesignOp, Resolved};
use fab_elec_explore::KeepBase;
use makepad_widgets_core::*;
use std::sync::Arc;

/// A released edit waiting for its preview's evaluation, then for its keep hints.
pub(super) struct KeepEdit {
    tag: u64,
    edit: Resolved,
    call: Option<Invocation>,
    unit: BoardId,
    /// The committed circuit the edit was planned on. A change to it ends the wait.
    committed: Arc<Circuit>,
    base: Arc<KeepBase>,
    asked: bool,
    /// Where the edit was asked for.
    at: [f64; 2],
}

impl ExplorerView {
    /// Preview a paste and hold the display until it commits with its keep hints. A paste the
    /// view cannot keep (no sheet shown, or no committed design) commits as planned.
    pub(super) fn paste_kept(&mut self, cx: &mut Cx, edit: Resolved, at: [f64; 2]) {
        let unit = edit.ops.iter().find_map(|op| match op { DesignOp::Circuit { unit, .. } => Some(*unit), _ => None });
        let shown = self.sheet_design.as_ref().and_then(|design| design.units.get(self.unit)).map(|unit| unit.circuit.clone());
        let committed = cx.has_global::<ElecLive>().then(|| unit.and_then(|id| cx.get_global::<ElecLive>().snap.design.unit(id).map(|u| u.circuit.clone()))).flatten();
        let (Some(unit), Some(shown), Some(committed)) = (unit, shown, committed) else {
            self.send_sheet_plan(cx, fab_elec_explore::plan::Planned::Edit(edit), at);
            return;
        };
        if self.note.is_some() {
            self.clear_note();
            self.draw_bg.redraw(cx);
        }
        let seen = cx.get_global::<ElecLive>().snap.last_refusal.as_ref().map_or(0, |refusal| refusal.seq);
        self.key_sent = Some((edit.label.clone(), at, seen));
        let tag = next_tag();
        cx.widget_action(self.uid, ViewAction::Preview { notes: vec![], tag, ops: edit.ops.clone(), place: edit.place.clone(), label: edit.label.clone() });
        let base = Arc::new(KeepBase { sheet: (*self.sheet).clone(), circuit: shown });
        self.wait_keep(tag, edit, unit, committed, base, at, None);
    }

    /// Release a Library part using its last planned sample, even if the host is still evaluating it.
    pub(super) fn keep_carry(&mut self, cx: &mut Cx) -> bool {
        let Some(gesture) = &self.gesture else { return false };
        let Some((tag, edit, base)) = gesture.carry_drop() else { return false };
        self.wait_keep(tag, edit.clone(), base.circuit.unit, base.circuit.clone(), base.clone(), gesture.end_point(), gesture.invocation());
        // The layout may already be on screen with no worker signal left to wake the release.
        self.keep_snapshot(cx);
        true
    }

    /// Hold the displayed sheet until the host answers this edit's commit or refuses its preview.
    fn wait_keep(&mut self, tag: u64, edit: Resolved, unit: BoardId, committed: Arc<Circuit>, base: Arc<KeepBase>, at: [f64; 2], call: Option<Invocation>) {
        self.hold = Some((tag, unit, committed.clone()));
        self.keep_edit = Some(KeepEdit { tag, edit, call, unit, committed, base, asked: false, at });
    }

    /// Use the shown carry layout's result or ask the worker once the released tag has evaluated.
    pub(super) fn keep_snapshot(&mut self, cx: &mut Cx) {
        let Some(keep) = &mut self.keep_edit else { return };
        if !cx.has_global::<ElecLive>() { return; }
        let snap = cx.get_global::<ElecLive>().snap.clone();
        if let Some(answer) = landed(&snap, keep.tag, keep.unit, &keep.committed) {
            let why = match answer {
                Answer::Refused => snap.last_refusal.as_ref().map(|r| r.reason.clone()),
                Answer::BaseMoved => Some("the sheet changed before this edit could land".into()),
                _ => None,
            };
            if let Some((call, why)) = keep.call.take().zip(why) {
                Invocation::send(cx, self.uid, call.attach(ViewAction::PlanRefused(why)));
            }
            self.keep_edit = None;
            return;
        }
        if keep.asked || snap.preview_tag != Some(keep.tag) { return; }
        let Some(design) = snap.preview.clone() else { return };
        let Some(unit) = design.units.iter().position(|unit| unit.id == keep.unit) else { return };
        keep.asked = true;
        let (base, token) = (keep.base.clone(), keep.tag);
        let carried = if self.gesture.is_some() {
            if self.sheet_design.as_ref().is_some_and(|shown| Arc::ptr_eq(shown, &design)) {
                if let Some(hints) = self.shown_keep.take() {
                    self.kept(cx, token, hints);
                    return;
                }
            }
            // A quick release may have reached a different block since the last displayed sample.
            // Only a carry adding one part uses a kept preview.
            let Some((_, path)) = self.carry_layout(&design) else {
                self.kept(cx, token, Ok(Vec::new()));
                return;
            };
            Some(path)
        } else { None };
        let opts = self.layout_opts();
        self.layout_jobs.keep_hints(cx, design, unit, base, carried, token, opts);
    }

    /// Commit the edit and its hints as one step. A no-room note ends it at the drop point.
    pub(super) fn kept(&mut self, cx: &mut Cx, token: u64, hints: KeepHints) {
        let Some(keep) = self.keep_edit.take_if(|keep| keep.tag == token) else { return };
        // CommitPreview and CancelPreview address the host's current slot. Another preview
        // can take it while the worker is finding room for this edit.
        let owns_preview = cx.get_global::<ElecLive>().snap.preview_tag == Some(token);
        let hints = if owns_preview { hints } else { Err("The preview changed before this edit could land".into()) };
        let hints = match hints {
            Ok(hints) => hints,
            Err(note) => {
                if let Some(call) = keep.call {
                    Invocation::send(cx, self.uid, call.attach(ViewAction::PlanRefused(note.clone())));
                }
                self.hold = None;
                if let Some(gesture) = &mut self.gesture {
                    let mut step = gesture.refuse_keep(token, note);
                    if !owns_preview { step.action = None; }
                    self.wire_step(cx, step);
                } else {
                    if owns_preview { cx.widget_action(self.uid, ViewAction::CancelPreview); }
                    self.show_note(cx, note, keep.at, true);
                }
                return;
            }
        };
        let mut tag = token;
        if !hints.is_empty() {
            let mut ops = keep.edit.ops;
            ops.extend(hints.into_iter().map(|(path, hint)| DesignOp::Circuit { unit: keep.unit, op: CircuitOp::Hint { path, hint: Some(hint) } }));
            tag = next_tag();
            if let Some(gesture) = &mut self.gesture { gesture.retag(token, tag); }
            cx.widget_action(self.uid, ViewAction::Preview { notes: vec![], tag, ops, place: keep.edit.place, label: keep.edit.label });
            if let Some(hold) = &mut self.hold { hold.0 = tag; }
        }
        let action = keep.call.map_or(ViewAction::CommitPreview, |call| call.attach(ViewAction::Land(tag)));
        Invocation::send(cx, self.uid, action);
        self.own_edit = Some(self.gen);
    }
}
