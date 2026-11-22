use super::*;
use crate::report::{GapKind, SourceFile};
use fab_elec_board::{Board, CustomRule, RuleCode, RuleItem, RuleProps, RuleSet, RuleSource};
use fab_elec_circuit::ViaSize;
use fab_elec_core::{Severity, Writer};

/// Conditions compiled from one rule file, indexed by the mapped rules' `RuleCode` ids.
#[derive(Clone, Debug, Default)]
pub struct KicadRuleSource {
    conditions: Vec<(RuleCode, Condition)>,
}

impl KicadRuleSource {
    pub fn len(&self) -> usize { self.conditions.len() }
    pub fn is_empty(&self) -> bool { self.conditions.is_empty() }
}

impl RuleSource for KicadRuleSource {
    fn when(&self, code: &RuleCode, a: &RuleProps, b: &RuleProps) -> bool {
        self.conditions.get(code.id as usize).is_some_and(|(stored, condition)| {
            stored == code && (condition.eval(a, b) || condition.eval(b, a))
        })
    }
}

#[derive(Clone, Debug)]
enum Condition {
    Always,
    Never,
    Class { item: Item, value: String, wildcard: bool, negate: bool },
    Kind { item: Item, value: String, wildcard: bool, negate: bool },
    HasClass { item: Item, value: String, exact: bool },
    /// `A.NetClass == B.NetClass` (or `!=`): the two items' effective class names compared.
    SameClass { negate: bool },
    Not(Box<Condition>),
    And(Box<Condition>, Box<Condition>),
    Or(Box<Condition>, Box<Condition>),
}

impl Condition {
    fn eval(&self, a: &RuleProps, b: &RuleProps) -> bool {
        let props = |item| if item == Item::A { a } else { b };
        let compare = |value: &str, expected: &str, wildcard| {
            let value = value.to_lowercase();
            if wildcard { wildcard_matches(expected, &value) } else { value == expected }
        };
        match self {
            Self::Always => true, Self::Never => false,
            Self::Class { item, value, wildcard, negate } => {
                let class = &props(*item).class_name;
                let equal = compare(class, value, *wildcard) || class.split(',').any(|part| compare(part, value, *wildcard));
                equal != *negate
            }
            Self::Kind { item, value, wildcard, negate } => {
                let name = match props(*item).item {
                    RuleItem::Pad => "pad", RuleItem::Track => "track", RuleItem::Via => "via", RuleItem::Zone => "zone",
                    RuleItem::Hole => return false,
                };
                compare(name, value, *wildcard) != *negate
            }
            Self::HasClass { item, value, exact } => {
                if value.is_empty() { return false; }
                let class = &props(*item).class_name;
                if *exact { class == value } else { class.split(',').any(|part| wildcard_matches(value, part)) }
            }
            Self::SameClass { negate } => (a.class_name.to_lowercase() == b.class_name.to_lowercase()) != *negate,
            Self::Not(value) => !value.eval(a, b),
            Self::And(left, right) => left.eval(a, b) && right.eval(a, b),
            Self::Or(left, right) => left.eval(a, b) || right.eval(a, b),
        }
    }
}

#[derive(Default)]
struct Blockers(Vec<(u8, String)>);

impl Blockers {
    fn add(&mut self, priority: u8, reason: String) {
        if !self.0.iter().any(|(_, r)| *r == reason) { self.0.push((priority, reason)); }
    }
    fn expression(&mut self, expr: &Expr) -> bool {
        match expr {
            Expr::Property { name, .. } => { self.add(1, format!("cond.{}", property_name(name))); true }
            Expr::Layer => { self.add(1, "cond.Layer".into()); true }
            Expr::Call { name, args, .. } => {
                self.add(2, format!("cond.{name}"));
                for arg in args { self.expression(arg); }
                true
            }
            Expr::Unary { value, .. } => self.expression(value),
            Expr::Binary { left, right, .. } => {
                let left = self.expression(left);
                let right = self.expression(right);
                left || right
            }
            _ => false,
        }
    }
    fn reasons(&mut self) -> Vec<String> {
        self.0.sort_by_key(|(priority, _)| *priority);
        self.0.iter().map(|(_, reason)| reason.clone()).collect()
    }
}

fn property_name(name: &str) -> String {
    for known in ["NetClass", "NetName", "Net", "Type", "ComponentClass", "Reference", "Value", "Layer", "Pad_Type", "Pad_Shape", "Orientation", "Position_X", "Position_Y", "Name", "Pin_Type"] {
        if known.replace('_', " ").eq_ignore_ascii_case(name) { return known.into(); }
    }
    name.replace(' ', "_")
}

fn compile(expr: &Expr, blockers: &mut Blockers) -> Option<Condition> {
    match expr {
        Expr::Binary { op: op @ (BinaryOp::And | BinaryOp::Or), left, right } => {
            let left = compile(left, blockers);
            let right = compile(right, blockers);
            match (left, right) {
                (Some(left), Some(right)) => Some(if *op == BinaryOp::And {
                    Condition::And(Box::new(left), Box::new(right))
                } else { Condition::Or(Box::new(left), Box::new(right)) }),
                _ => None,
            }
        }
        Expr::Unary { op: UnaryOp::Not, value } => compile(value, blockers).map(|c| Condition::Not(Box::new(c))),
        Expr::Binary { op: op @ (BinaryOp::Equal | BinaryOp::NotEqual), left, right } => {
            if let (Expr::Property { item: x, name: left_name }, Expr::Property { item: y, name: right_name }) = (left.as_ref(), right.as_ref()) {
                if x != y && left_name.eq_ignore_ascii_case("NetClass") && right_name.eq_ignore_ascii_case("NetClass") {
                    return Some(Condition::SameClass { negate: *op == BinaryOp::NotEqual });
                }
            }
            if let (Expr::Property { item, name }, Expr::String { value, wildcard }) = (left.as_ref(), right.as_ref()) {
                let negate = *op == BinaryOp::NotEqual;
                if name.eq_ignore_ascii_case("NetClass") {
                    return Some(Condition::Class { item: *item, value: value.to_lowercase(), wildcard: *wildcard, negate });
                }
                if name.eq_ignore_ascii_case("Type") {
                    // A kind the resolver has no items of never equals an item's kind, so `==` with it
                    // never holds and `!=` holds for every item that has a kind.
                    return Some(Condition::Kind { item: *item, value: value.to_lowercase(), wildcard: *wildcard, negate });
                }
            }
            if !blockers.expression(expr) {
                blockers.add(3, "cond.expression".into());
            }
            None
        }
        Expr::Call { item, name, args } if *item != Item::AB && matches!(name.as_str(), "hasNetclass" | "hasExactNetclass") => {
            if args.is_empty() { return Some(Condition::Never); }
            if let [Expr::String { value, .. }] = args.as_slice() {
                return Some(Condition::HasClass { item: *item, value: value.clone(), exact: name == "hasExactNetclass" });
            }
            blockers.expression(expr);
            None
        }
        _ => {
            if !blockers.expression(expr) { blockers.add(3, "cond.expression".into()); }
            None
        }
    }
}

impl RuleFile {
    /// Append the representable rules in source order, retaining every loss in `report`.
    pub fn append_to(&self, board: &mut Board, report: &mut ImportReport) -> KicadRuleSource {
        let previous = report.current;
        report.current = SourceFile::Rules;
        report.merge(&self.report);
        let mut source = KicadRuleSource::default();
        for rule in &self.rules {
            let mut blockers = Blockers::default();
            let mut dropped = Vec::new();
            let mut set = RuleSet::default();
            let base_via = board.rules.base.via.or_else(|| RuleSet::standard().via)
                .unwrap_or(ViaSize { drill: 300_000, pad: 450_000 });
            for constraint in &rule.constraints {
                let value = constraint.min.and_then(|v| i32::try_from(v).ok());
                let mapped = if let Some(value) = value {
                    match constraint.kind {
                        ConstraintKind::Clearance => { set.clearance = Some(value); true }
                        ConstraintKind::TrackWidth => { set.track = Some(value); true }
                        ConstraintKind::ViaDiameter => { set.via.get_or_insert(base_via).pad = value; true }
                        ConstraintKind::HoleSize => { set.via.get_or_insert(base_via).drill = value; true }
                        ConstraintKind::DiffPairGap => { set.pair_gap = Some(value); true }
                        _ => false,
                    }
                } else { false };
                if !mapped {
                    let reason = format!("constraint.{}", constraint.kind.name());
                    blockers.add(0, reason.clone());
                    dropped.push(reason);
                } else {
                    for (name, present) in [("opt", constraint.opt.is_some()), ("max", constraint.max.is_some())] {
                        if present { dropped.push(format!("constraint.{}.{name}", constraint.kind.name())); }
                    }
                }
            }
            let condition = match &rule.condition {
                Some(expr) => compile(expr, &mut blockers), None => Some(Condition::Always),
            };
            let layers = rule.layers.map(|layers| layers.intersect(board.copper_set()));
            let non_copper = layers.is_some_and(|layers| layers.is_empty());
            if non_copper { blockers.add(4, "layer.non_copper".into()); }
            let hidden = matches!(rule.severity, Some(RuleSeverity::Ignore | RuleSeverity::Exclusion));
            let has_values = set.clearance.is_some() || set.track.is_some() || set.via.is_some() || set.pair_gap.is_some();
            if hidden || !has_values || condition.is_none() || non_copper {
                if !has_values && rule.constraints.is_empty() { blockers.add(0, "constraint.empty".into()); }
                let mut reasons = blockers.reasons();
                if hidden { reasons.insert(0, "severity_hidden".into()); }
                let reason = reasons.first().map(String::as_str).unwrap_or("cond.expression");
                let mut detail = format!("{}: {}", rule.name, reasons.join(", "));
                if non_copper { detail.push_str(&format!(" ({})", rule.layer_pattern.as_deref().unwrap_or_default())); }
                report.gap_value(&format!("rules.custom.{reason}"), GapKind::Dropped, rule.location, detail);
                continue;
            }
            let Some(condition) = condition else { continue; };
            let Ok(id) = u32::try_from(source.conditions.len()) else {
                report.gap_value("rules.custom.capacity", GapKind::Dropped, rule.location, rule.name.clone());
                continue;
            };
            let Ok(offset) = u32::try_from(rule.offset) else {
                report.gap_value("rules.custom.capacity", GapKind::Dropped, rule.location, rule.name.clone());
                continue;
            };
            let mut hash = Writer::hasher();
            hash.raw(rule.condition_text.as_deref().unwrap_or_default().as_bytes());
            let code = RuleCode { id, hash: hash.finish(), at: (0, offset) };
            source.conditions.push((code.clone(), condition));
            board.rules.custom.push(CustomRule {
                name: rule.name.clone(), when: code, layers, set,
                severity: if rule.severity == Some(RuleSeverity::Warning) { Severity::Warning } else { Severity::Error },
            });
            report.count_made("rules.custom");
            if !dropped.is_empty() {
                report.gap_value("rules.custom.partial", GapKind::Dropped, rule.location, format!("{}: {}", rule.name, dropped.join(", ")));
            }
        }
        report.current = previous;
        source
    }
}
