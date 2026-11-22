//! The registry: the tool table plus the implementations a host registered.
//! It checks every call (name, then arguments against the schema) before a
//! handler sees it, and answers for the tools the host has not implemented.
//!
//! The handler type is the host's: Stage registers plain functions over its
//! own state; a test registers closures. The registry only stores and finds
//! them.

use crate::catalog::TOOLS;
use crate::output::{ErrorKind, ToolError};
use crate::schema;
use crate::spec::ToolSpec;
use makepad_strict_json::Value;

pub struct Registry<H> {
    /// One slot per entry of [`TOOLS`], in its order.
    handlers: Vec<Option<H>>,
    /// Tools only this host offers (the Generate loop's `storyboard` and
    /// `finish`), after the table's.
    local: Vec<(ToolSpec, H)>,
}

/// A call that passed the checks.
pub struct Checked<'a, H> {
    pub spec: &'a ToolSpec,
    pub handler: &'a H,
    pub args: Value,
}

impl<H> Default for Registry<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> Registry<H> {
    pub fn new() -> Self {
        Self { handlers: TOOLS.iter().map(|_| None).collect(), local: Vec::new() }
    }

    /// Implement table tool `name`. Panics on a name the table does not
    /// have: that is a host bug, not a runtime condition.
    pub fn register(&mut self, name: &str, handler: H) {
        let index = TOOLS.iter().position(|tool| tool.name == name).unwrap_or_else(|| panic!("studio api: `{name}` is not in the tool table"));
        self.handlers[index] = Some(handler);
    }

    /// A tool of this host only (not in the shared table).
    pub fn add_local(&mut self, spec: ToolSpec, handler: H) {
        assert!(!TOOLS.iter().any(|tool| tool.name == spec.name), "studio api: `{}` shadows a table tool", spec.name);
        self.local.retain(|(existing, _)| existing.name != spec.name);
        self.local.push((spec, handler));
    }

    /// Every tool with whether this host implements it, table first.
    pub fn tools(&self) -> Vec<(&ToolSpec, bool)> {
        TOOLS.iter().zip(&self.handlers).map(|(spec, handler)| (spec, handler.is_some()))
            .chain(self.local.iter().map(|(spec, _)| (spec, true)))
            .collect()
    }

    pub fn available(&self, name: &str) -> bool {
        self.tools().iter().any(|(spec, available)| spec.name == name && *available)
    }

    /// Find `name`, check `args` against its schema, and hand back its
    /// handler. Arguments that are absent or `null` count as `{}`.
    pub fn check(&self, name: &str, args: Value) -> Result<Checked<'_, H>, ToolError> {
        let found = TOOLS.iter().zip(&self.handlers).map(|(spec, handler)| (spec, handler.as_ref()))
            .chain(self.local.iter().map(|(spec, handler)| (spec, Some(handler))))
            .find(|(spec, _)| spec.name == name);
        let Some((spec, handler)) = found else {
            let near: Vec<&str> = self.tools().iter().map(|(spec, _)| spec.name).filter(|candidate| candidate.contains(name) || name.contains(candidate)).collect();
            let hint = if near.is_empty() { "call stage_status or list the tools".to_string() } else { format!("did you mean {}?", near.join(" or ")) };
            return Err(ToolError::new(ErrorKind::UnknownTool, format!("there is no tool `{name}`: {hint}")));
        };
        let Some(handler) = handler else {
            return Err(ToolError::new(ErrorKind::NotAvailable, format!("`{name}` is not available yet: {} has not landed in this Stage build", spec.backed_by)));
        };
        let args = if args.is_null() { Value::Obj(Vec::new()) } else { args };
        if let Err(errors) = schema::validate(&spec.input_schema(), &args) {
            let mut message = format!("`{name}` was called with arguments that do not fit its schema:\n- {}", errors.join("\n- "));
            message.push_str(&format!("\nIts arguments: {}", compact_schema(spec)));
            return Err(ToolError::invalid(message));
        }
        Ok(Checked { spec, handler, args })
    }
}

/// `name: type (required)` per field, for an error message.
pub fn compact_schema(spec: &ToolSpec) -> String {
    let schema = spec.input_schema();
    let required: Vec<&str> = schema.get("required").and_then(Value::as_arr).unwrap_or(&[]).iter().filter_map(Value::as_str).collect();
    let Some(Value::Obj(properties)) = schema.get("properties") else { return "none".into() };
    if properties.is_empty() {
        return "none".into();
    }
    properties.iter().map(|(name, property)| {
        let ty = match property.get("type") {
            Some(Value::Str(ty)) => ty.clone(),
            Some(Value::Arr(types)) => types.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("|"),
            _ => "any".into(),
        };
        format!("{name}: {ty}{}", if required.contains(&name.as_str()) { " (required)" } else { "" })
    }).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Family;

    #[test]
    fn unknown_unavailable_and_bad_calls_say_what_to_do() {
        let mut registry: Registry<u32> = Registry::new();
        registry.register("timeline", 1);
        registry.register("trim_block", 2);
        let error = registry.check("trim", Value::Null).err().unwrap();
        assert_eq!(error.kind, ErrorKind::UnknownTool);
        assert!(error.message.contains("trim_block"), "{}", error.message);
        let error = registry.check("frame", Value::Null).err().unwrap();
        assert_eq!(error.kind, ErrorKind::NotAvailable);
        assert!(error.message.contains("not available yet: M2"), "{}", error.message);
        let error = registry.check("trim_block", crate::schema::parse(r#"{"node": "a", "start": "1"}"#).unwrap()).err().unwrap();
        assert_eq!(error.kind, ErrorKind::InvalidArgs);
        assert!(error.message.contains("`length` is missing") && error.message.contains("without quotes") && error.message.contains("node: string (required)"), "{}", error.message);
        let ok = registry.check("timeline", Value::Null).ok().unwrap();
        assert_eq!((*ok.handler, ok.args), (1, Value::Obj(Vec::new())));
    }

    #[test]
    fn local_tools_join_the_list() {
        let mut registry: Registry<u32> = Registry::new();
        registry.add_local(ToolSpec { name: "finish", family: Family::Session, description: "end the work", input: r#"{"type": "object", "properties": {}}"#, output: r#"{"type": "object", "properties": {}}"#, mutates: false, images: false, backed_by: "Generate", request: false, layout: None }, 9);
        assert!(registry.available("finish"));
        assert_eq!(*registry.check("finish", Value::Null).ok().unwrap().handler, 9);
        assert_eq!(registry.tools().len(), TOOLS.len() + 1);
    }
}
