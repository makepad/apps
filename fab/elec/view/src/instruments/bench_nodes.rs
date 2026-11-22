//! The bench owns trigger summaries and cross-lane delay lookup/actions.

use super::*;
use crate::instruments::{axis::si_label, semantics::{self, Lookup}};
use a11y_tree::{ActionResult, Actions as A, Node, Role};

impl BenchInstruments {
    pub(super) fn refresh_trigger_summary(&self) {
        let lane = self.trigger_net.as_ref().and_then(|net| self.cards.get(net)).and_then(|entry| entry.widget.borrow::<SignalLane>());
        let unit = lane.as_ref().and_then(|lane| lane.settings().channels.first()).map_or("V", |channel| channel.unit.as_str());
        self.semantic_summary.borrow_mut().trigger(self.trigger_net.as_deref(), self.scope_settings.trigger, unit);
    }

    fn delay_key(kept: &KeptDelay, part: u64) -> u64 {
        LiveId::from_str("scope delay").bytes_append(&kept.key.to_le_bytes()).bytes_append(&part.to_le_bytes()).0.max(1)
    }

    pub(super) fn lookup_delays(&self, cx: &Cx, lookup: &Lookup, out: &mut Vec<(u64, Option<u64>, Node)>) {
        for (index, kept) in self.delays.iter().enumerate() {
            for part in 0..3 {
                let key = Self::delay_key(kept, part);
                if !lookup.wants(key) { continue; }
                let end = if part == 2 { End::Finish } else { End::Start };
                let endpoint = if end == End::Start { &kept.bracket.from } else { &kept.bracket.to };
                let lane = self.cards.get(&endpoint.0).and_then(|entry| entry.widget.borrow::<SignalLane>());
                let mut node = Node {
                    role: Role::Slider,
                    name: if part == 0 { "delay bracket".into() } else { format!("{} delay {}", endpoint.0, if end == End::Start { "start" } else { "end" }) },
                    value: if part == 0 { format!("{} .. {}", si_label(kept.bracket.from.2, "s"), si_label(kept.bracket.to.2, "s")) } else { si_label(endpoint.2, "s") },
                    description: kept.label.clone(),
                    actions: A::FOCUS | A::PRESS | A::SET_VALUE,
                    states: a11y_tree::States::FOCUSABLE,
                    ..Default::default()
                };
                semantics::bounds(&mut node, lane.and_then(|lane| lane.delay_lookup_rect(cx, index, end)));
                lookup.push(key, node, out);
            }
        }
    }

    pub(super) fn delay_lookup_action(&mut self, cx: &mut Cx, key: u64, request: &a11y_tree::ActionRequest) -> ActionResult {
        let found = self.delays.iter().enumerate().find_map(|(index, kept)| (0..3).find(|&part| Self::delay_key(kept, part) == key).map(|part| (index, part)));
        let Some((index, part)) = found else { return ActionResult::Unhandled };
        if request.action == A::SET_VALUE {
            let kept = &mut self.delays[index];
            let values = if part == 0 { semantics::pair(&request.arg, "s") }
                else { semantics::number(&request.arg, "s").map(|v| if part == 1 { (v, kept.bracket.to.2) } else { (kept.bracket.from.2, v) }) };
            let Some((from, to)) = values else { return ActionResult::Unsupported };
            // Typed endpoints are exact trigger-relative times. Keep them fixed across captures;
            // dragging this endpoint again resumes the existing feature-following gesture.
            if part != 2 {
                kept.bracket.from.2 = from; kept.typed[0] = true;
                if let Some(edge) = &mut kept.edges[0] { edge.t = from; }
            }
            if part != 1 {
                kept.bracket.to.2 = to; kept.typed[1] = true;
                if let Some(edge) = &mut kept.edges[1] { edge.t = to; }
            }
            let (from, to) = (&kept.bracket.from, &kept.bracket.to);
            write_delay_label(&mut kept.label, &from.0, Some(from.1), &to.0, Some(to.1), Some(to.2 - from.2));
            self.push_delays(cx);
        } else if request.action == A::FOCUS || request.action == A::PRESS || request.action == A::SCROLL_INTO_VIEW {
            let kept = &self.delays[index];
            let end = if part == 2 { End::Finish } else { End::Start };
            let endpoint = if end == End::Start { &kept.bracket.from } else { &kept.bracket.to };
            if let Some(mut lane) = self.cards.get(&endpoint.0).and_then(|entry| entry.widget.borrow_mut::<SignalLane>()) {
                lane.focus_delay_lookup(cx, index, end, request.action == A::SCROLL_INTO_VIEW);
                if request.action == A::SCROLL_INTO_VIEW { self.scroll_bars.scroll_into_view_abs(cx, lane.area().rect(cx)); }
            }
        } else { return ActionResult::Unsupported; }
        self.redraw(cx);
        ActionResult::Handled
    }
}
