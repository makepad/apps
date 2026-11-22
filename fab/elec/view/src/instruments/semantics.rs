//! Scope lookup helpers. Nodes and numeric parsing are used only for a query or an action.

use super::{acquire::TriggerState, axis::si_label, scope::Trigger};
use fab_elec_bench::trigger::Slope;
use makepad_widgets_core::{a11y_tree::{self, ActionArg}, makepad_platform::a11y, *};

pub(super) fn key(kind: &str, identity: &str, part: u64) -> u64 {
    LiveId::from_str(kind).bytes_append(identity.as_bytes()).bytes_append(&part.to_le_bytes()).0.max(1)
}

pub(super) struct Lookup<'a> {
    query: Option<&'a a11y_tree::query::Query>,
    key: Option<u64>,
}

impl<'a> Lookup<'a> {
    pub(super) fn new(lookup: &'a SemanticLookup, id_key: Option<u64>) -> Option<Self> {
        match lookup {
            SemanticLookup::Terms(q) if q.is_lookup() && (q.id_term().is_none() || id_key.is_some()) => Some(Self { query: Some(q), key: id_key }),
            SemanticLookup::Key(key) => Some(Self { query: None, key: Some(*key) }),
            _ => None,
        }
    }

    pub(super) fn wants(&self, key: u64) -> bool { self.key.is_none_or(|wanted| wanted == key) }

    pub(super) fn push(&self, key: u64, mut node: a11y_tree::Node, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if !self.wants(key) { return; }
        if let Some(q) = self.query {
            node.id = q.id_term().unwrap_or_default();
            if !q.matches(&node) { return; }
            node.id = a11y_tree::NodeId::NONE;
        }
        out.push((key, None, node));
    }
}

pub(super) fn intersect(rect: Rect, clip: Rect) -> Option<Rect> {
    let lo = dvec2(rect.pos.x.max(clip.pos.x), rect.pos.y.max(clip.pos.y));
    let hi = dvec2((rect.pos.x + rect.size.x).min(clip.pos.x + clip.size.x), (rect.pos.y + rect.size.y).min(clip.pos.y + clip.size.y));
    (hi.x > lo.x && hi.y > lo.y).then_some(Rect { pos: lo, size: hi - lo })
}

pub(super) fn bounds(node: &mut a11y_tree::Node, rect: Option<Rect>) {
    node.bounds = rect.map(a11y::semantic_rect).unwrap_or_default();
    node.states.set(a11y_tree::States::OFFSCREEN, node.bounds.is_empty());
    node.actions.insert(a11y_tree::Actions::SCROLL_INTO_VIEW);
}

/// Base units for Number; Text accepts the displayed SI prefixes, ASCII u and an implied unit.
pub(super) fn number(arg: &ActionArg, unit: &str) -> Option<f64> {
    match arg {
        ActionArg::Number(value) => value.is_finite().then_some(*value),
        ActionArg::Text(text) => parse_number(text, unit).filter(|value| value.is_finite()),
        _ => None,
    }
}

// Keep nonfinite numbers here so value entry can distinguish range errors from wrong units.
// The lookup helpers below and the entry's setter validate finiteness before applying anything.
pub(super) fn parse_number(text: &str, unit: &str) -> Option<f64> {
    let text = text.trim().replace('−', "-");
    if let Ok(value) = text.parse::<f64>() { return Some(value); }
    let text = text.strip_suffix(unit).unwrap_or(&text).trim_end();
    if let Ok(value) = text.parse::<f64>() { return Some(value); }
    let (digits, power) = match text.chars().last()? {
        'q' => (&text[..text.len() - 1], -30), 'r' => (&text[..text.len() - 1], -27),
        'y' => (&text[..text.len() - 1], -24), 'z' => (&text[..text.len() - 1], -21),
        'a' => (&text[..text.len() - 1], -18), 'f' => (&text[..text.len() - 1], -15),
        'p' => (&text[..text.len() - 1], -12), 'n' => (&text[..text.len() - 1], -9),
        'µ' | 'μ' => (&text[..text.len() - 2], -6), 'u' => (&text[..text.len() - 1], -6),
        'm' => (&text[..text.len() - 1], -3), 'k' => (&text[..text.len() - 1], 3),
        'M' => (&text[..text.len() - 1], 6), 'G' => (&text[..text.len() - 1], 9),
        'T' => (&text[..text.len() - 1], 12), 'P' => (&text[..text.len() - 1], 15),
        'E' => (&text[..text.len() - 1], 18), 'Z' => (&text[..text.len() - 1], 21),
        'Y' => (&text[..text.len() - 1], 24), 'R' => (&text[..text.len() - 1], 27),
        'Q' => (&text[..text.len() - 1], 30), _ => (text, 0),
    };
    // Parse the decimal once, avoiding a second rounding from multiplying a parsed mantissa.
    let digits = digits.trim();
    let (mantissa, exponent) = if let Some((mantissa, exponent)) = digits.split_once(['e', 'E']) {
        (mantissa, exponent.parse::<i32>().ok()?.checked_add(power)?)
    } else { (digits, power) };
    format!("{mantissa}e{exponent}").parse::<f64>().ok()
}

pub(super) fn pair(arg: &ActionArg, unit: &str) -> Option<(f64, f64)> {
    let ActionArg::Text(text) = arg else { return None };
    let (a, b) = text.split_once("..").or_else(|| text.split_once(" to "))?;
    let (a, b) = (parse_number(a, unit).filter(|value| value.is_finite())?, parse_number(b, unit).filter(|value| value.is_finite())?);
    Some((a, b))
}

pub(super) fn trigger_value(label: &str, trigger: Trigger, unit: &str) -> String {
    let slope = match trigger.slope { Slope::Rising => "rising", Slope::Falling => "falling", Slope::Either => "either" };
    format!("{label} {slope} {}", si_label(trigger.level, unit))
}

/// Shared by the bench and its lanes. A changed answer updates this cache at its existing
/// throttled refresh; an unchanged draw only borrows text. No lane scan in semantics/draw.
pub(super) struct Summary {
    lanes: Vec<(String, String, String)>,
    trigger: Option<(String, Trigger, String)>,
    trigger_text: String,
    state: Option<TriggerState>,
    pub(super) text: String,
}

impl Default for Summary {
    fn default() -> Self { Self { lanes: Vec::new(), trigger: None, trigger_text: String::new(), state: None, text: "0 lanes".into() } }
}

impl Summary {
    pub(super) fn lanes<'a>(&mut self, lanes: impl Iterator<Item = (&'a str, &'a str)> + Clone) {
        if self.lanes.iter().map(|(net, label, _)| (net.as_str(), label.as_str()))
            .eq(lanes.clone().map(|(net, label)| (net, label.split('\n').next().unwrap_or(label)))) { return; }
        let mut old = std::mem::take(&mut self.lanes);
        self.lanes = lanes.map(|(net, label)| {
            let answer = old.iter_mut().find(|(id, ..)| id.as_str() == net).map(|(_, _, text)| std::mem::take(text)).unwrap_or_default();
            (net.into(), label.split('\n').next().unwrap_or(label).into(), answer)
        }).collect();
        self.refresh_trigger();
        self.refresh();
    }

    pub(super) fn answer(&mut self, net: &str, text: &str) {
        let Some((_, _, answer)) = self.lanes.iter_mut().find(|(id, ..)| id.as_str() == net) else { return };
        if answer.as_str() == text { return; }
        answer.clear(); answer.push_str(text);
        self.refresh();
    }

    pub(super) fn trigger(&mut self, net: Option<&str>, trigger: Trigger, unit: &str) {
        if self.trigger.as_ref().is_some_and(|(id, t, u)| Some(id.as_str()) == net && *t == trigger && u == unit) { return; }
        if net.is_none() && self.trigger.is_none() { return; }
        self.trigger = net.map(|net| (net.into(), trigger, unit.into()));
        self.refresh_trigger();
        self.refresh();
    }

    pub(super) fn state(&mut self, state: TriggerState) -> Option<&'static str> {
        if self.state == Some(state) { return None; }
        self.state = Some(state);
        self.refresh();
        Some(Self::state_word(state))
    }

    fn state_word(state: TriggerState) -> &'static str {
        match state { TriggerState::Waiting => "waiting", TriggerState::Fired => "firing", TriggerState::Automatic => "automatic" }
    }

    fn refresh_trigger(&mut self) {
        self.trigger_text = if let Some((net, trigger, unit)) = &self.trigger {
            let label = self.lanes.iter().find(|(id, ..)| id == net).map_or(net.as_str(), |(_, label, _)| label.as_str());
            trigger_value(label, *trigger, unit)
        } else { String::new() };
    }

    fn refresh(&mut self) {
        use std::fmt::Write;
        self.text.clear();
        let _ = write!(self.text, "{} lanes", self.lanes.len());
        if self.trigger.is_some() {
            self.text.push_str("; trigger "); self.text.push_str(&self.trigger_text);
            if let Some(state) = self.state { let _ = write!(self.text, ", {}", Self::state_word(state)); }
        }
        for (_, label, answer) in &self.lanes {
            self.text.push_str("; "); self.text.push_str(label);
            for word in answer.split_whitespace() { self.text.push(' '); self.text.push_str(word); }
        }
    }
}
