//! Catalog-checked Splash literal arguments; no evaluation or executable expressions.

use std::collections::BTreeMap;
use fab_elec_design::action::{FieldDef, FieldKind};
use fab_elec_lang::units::{circuit_units, qty_of};
use super::actions::ACTIONS;

/// One tool invocation, with values written as Splash literals.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// Dotted catalog name.
    pub tool: String,
    /// Named arguments; duplicate and unknown names are refused.
    pub args: Vec<(String, String)>,
}

/// A checked literal in a catalog field.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Text or a choice, with quoting and symbol prefix removed.
    Text(String),
    /// Finite SI quantity.
    Num(f64),
    /// An integer checked without a floating-point conversion.
    Int(i64),
    /// A boolean literal.
    Bool(bool),
}

/// Parse one field, including its dimensional unit and integer bounds.
pub fn parse_field(field: &FieldDef, text: &str) -> Result<Value, String> {
    let text = text.trim();
    let parsed = match field.kind {
        FieldKind::Text => unquote(text).map(Value::Text),
        FieldKind::Choice(choices) => {
            let value = text.strip_prefix('@').unwrap_or(text);
            if choices.contains(&value) { Ok(Value::Text(value.into())) }
            else { Err(format!("expected {}", choices.join("|"))) }
        }
        FieldKind::Int { min, max } => text.replace('_', "").parse::<i64>()
            .ok().filter(|v| *v >= min && *v <= max).map(Value::Int)
            .ok_or_else(|| format!("expected an integer from {min} to {max}")),
        FieldKind::Num { unit } => number(text, unit).map(Value::Num),
        FieldKind::Bool => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err("expected true or false".into()),
        },
        _ => Err("unsupported literal kind".into()),
    };
    parsed.map_err(|reason| format!("{}: {reason}", field.name))
}

pub(super) fn number(text: &str, unit: &str) -> Result<f64, String> {
    let text = text.trim().replace('_', "");
    let plain = text.parse::<f64>().ok();
    // Bare numbers are SI quantities. A written suffix must have the right dimension.
    let value = if let Some(value) = plain { Some(value) } else {
        let table = circuit_units();
        table.entries().iter().find_map(|entry| {
            let digits = text.strip_suffix(entry.suffix.as_str())?;
            let coefficient = digits.parse::<f64>().ok()?;
            if !coefficient.is_finite() { return None; }
            if let Some((_, exponent)) = digits.split_once(['e', 'E']) {
                let exponent = exponent.parse::<i32>().ok()?;
                if !(-1024..=1024).contains(&exponent) { return None; }
            }
            let compatible = if unit == "%" { entry.suffix == "%" || entry.suffix == "ppm" }
                else { qty_of(unit).zip(qty_of(&entry.suffix)).is_some_and(|(a, b)| a.0 == b.0) };
            compatible.then(|| entry.literal(digits))
        })
    };
    value.filter(|v| v.is_finite()).ok_or_else(|| format!("expected a finite number in {}", if unit.is_empty() { "SI units" } else { unit }))
}

pub(super) fn unquote(text: &str) -> Result<String, String> {
    if !text.starts_with('"') {
        if text.is_empty() || text.chars().any(char::is_control) { return Err("expected text".into()); }
        return Ok(text.to_owned());
    }
    let tree = makepad_widgets_core::makepad_script::syntax::parse(text).map_err(|e| e.message)?;
    let [node] = tree.statements() else { return Err("expected one string literal".into()) };
    node.string().map(str::to_owned).ok_or_else(|| "expected a string literal".into())
}

/// Validated arguments with the catalog defaults applied.
#[derive(Clone, Debug)]
pub struct Arguments {
    values: BTreeMap<String, Value>,
}

impl Arguments {
    /// Refuse unknown tools, fields, duplicates, invalid literals and missing required fields.
    pub fn parse(call: &ToolCall) -> Result<Self, String> {
        let action = ACTIONS.iter().find(|a| a.name == call.tool).ok_or_else(|| format!("tool: unknown {}", call.tool))?;
        let mut values = BTreeMap::new();
        for (name, text) in &call.args {
            let field = action.fields.iter().find(|f| f.name == name).ok_or_else(|| format!("{name}: unknown field"))?;
            if values.contains_key(name) { return Err(format!("{name}: duplicate field")); }
            values.insert(name.clone(), parse_field(field, text)?);
        }
        for field in action.fields {
            if values.contains_key(field.name) { continue; }
            if field.required { return Err(format!("{}: required field is missing", field.name)); }
            if !field.default.is_empty() { values.insert(field.name.into(), parse_field(field, field.default)?); }
        }
        Ok(Self { values })
    }
    /// Text or choice value, if present.
    pub fn text(&self, name: &str) -> Option<&str> {
        match self.values.get(name) { Some(Value::Text(value)) => Some(value), _ => None }
    }
    /// SI number, if present.
    pub fn num(&self, name: &str) -> Option<f64> {
        match self.values.get(name) { Some(Value::Num(value)) => Some(*value), _ => None }
    }
    /// Integer, if present.
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.values.get(name) { Some(Value::Int(value)) => Some(*value), _ => None }
    }
    pub(super) fn budget(&self, cap: usize) -> usize {
        (self.int("budget").unwrap_or(4096) as u64).min(cap as u64) as usize
    }
    pub(super) fn span(&self, full: (f64, f64)) -> Result<(f64, f64), String> {
        let Some(text) = self.text("span") else { return Ok(full) };
        let text = text.trim().trim_start_matches('[').trim_end_matches(']');
        let values: Vec<_> = text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).collect();
        if values.len() != 2 { return Err("span: expected [from to]".into()); }
        let from = number(values[0], "s").map_err(|e| format!("span: {e}"))?;
        let to = number(values[1], "s").map_err(|e| format!("span: {e}"))?;
        if from >= to || from < full.0 || to > full.1 { return Err("span: expected an increasing interval within the capture".into()); }
        Ok((from, to))
    }
}
