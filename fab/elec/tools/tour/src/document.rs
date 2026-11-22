//! The written tour subset of Splash: literals only, with source-line diagnostics, never VM execution.

use makepad_script::syntax::{self, Field, Kind, Node};
use makepad_script::units::UnitTable;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

pub struct Tour {
    pub app: String,
    pub cursor: String,
    pub record: Option<String>,
    pub events: Vec<Event>,
}

pub struct Event {
    pub at: Duration,
    pub line: usize,
    pub action: Action,
}

pub enum Action {
    Settle(Duration),
    Press(String),
    Drag { node: String, dx: f64, dy: f64, ms: u64 },
    DragTo { from: String, to: String, ms: u64 },
    Key(String),
    Grab,
}

struct Reader<'a> { source: &'a str }

impl Reader<'_> {
    fn line(&self, at: usize) -> usize { self.source.as_bytes()[..at.min(self.source.len())].iter().filter(|&&b| b == b'\n').count() + 1 }
    fn error(&self, at: usize, expected: &str) -> String { format!("line {}: expected {expected}", self.line(at)) }

    fn object<'a>(&self, node: &'a Node, allowed: &[&str]) -> Result<&'a [Field], String> {
        let Kind::Object { ty: None, fields, .. } = &node.kind else { return Err(self.error(node.span.start, "an object {field: value ...}")); };
        for (i, field) in fields.iter().enumerate() {
            let name = field.name.as_deref().unwrap_or("");
            if field.op != ":" || !allowed.contains(&name) { return Err(self.error(field.span.start, &format!("one of {} followed by ':'", allowed.join(", ")))); }
            if fields[..i].iter().any(|old| old.name == field.name) { return Err(self.error(field.span.start, &format!("only one '{name}' field"))); }
        }
        Ok(fields)
    }

    fn required<'a>(&self, node: &Node, fields: &'a [Field], name: &str) -> Result<&'a Node, String> {
        field(fields, name).ok_or_else(|| self.error(node.span.start, &format!("a '{name}' field")))
    }

    fn string(&self, node: &Node) -> Result<String, String> {
        match &node.kind {
            Kind::Str(value) if !value.trim().is_empty() => Ok(value.clone()),
            _ => Err(self.error(node.span.start, "a nonempty quoted string")),
        }
    }

    fn number(&self, node: &Node) -> Result<f64, String> {
        let value = match &node.kind {
            Kind::Number(value) => *value,
            Kind::Unary { op, operand } if op == "-" => -self.number(operand)?,
            _ => return Err(self.error(node.span.start, "a finite numeric literal")),
        };
        if !value.is_finite() { return Err(self.error(node.span.start, "a finite numeric literal")); }
        Ok(value)
    }

    fn seconds(&self, node: &Node) -> Result<Duration, String> {
        let value = self.number(node)?;
        let duration = Duration::try_from_secs_f64(value).ok().filter(|&d| Instant::now().checked_add(d).is_some());
        duration.ok_or_else(|| self.error(node.span.start, "nonnegative seconds within the wall clock's range"))
    }

    fn array<'a>(&self, node: &'a Node) -> Result<&'a [Node], String> {
        match &node.kind { Kind::Array { items, .. } => Ok(items), _ => Err(self.error(node.span.start, "an array [...]")) }
    }

    fn drag(&self, node: &Node) -> Result<Action, String> {
        let fields = self.object(node, &["node", "dx", "dy", "from", "to", "ms"])?;
        let duration = self.required(node, fields, "ms")?;
        let ms = self.number(duration)?;
        if ms < 1.0 || ms.fract() != 0.0 || ms >= u64::MAX as f64 { return Err(self.error(duration.span.start, "a positive integer number of milliseconds")); }
        if let Some(from) = field(fields, "from") {
            if fields.iter().any(|f| matches!(f.name.as_deref(), Some("node" | "dx" | "dy"))) { return Err(self.error(node.span.start, "either {from: ... to: ... ms: ...} or {node: ... dx: ... dy: ... ms: ...}")); }
            let to = self.required(node, fields, "to")?;
            Ok(Action::DragTo { from: self.string(from)?, to: self.string(to)?, ms: ms as u64 })
        } else {
            let target = self.required(node, fields, "node")?;
            if field(fields, "to").is_some() || (field(fields, "dx").is_none() && field(fields, "dy").is_none()) { return Err(self.error(node.span.start, "a node drag with dx and/or dy, without from/to")); }
            let dx = field(fields, "dx").map(|n| self.number(n)).transpose()?.unwrap_or(0.0);
            let dy = field(fields, "dy").map(|n| self.number(n)).transpose()?.unwrap_or(0.0);
            Ok(Action::Drag { node: self.string(target)?, dx, dy, ms: ms as u64 })
        }
    }

    fn beat(&self, node: &Node) -> Result<Event, String> {
        let fields = self.object(node, &["at", "settle", "press", "drag", "key"])?;
        let at = self.seconds(self.required(node, fields, "at")?)?;
        let actions: Vec<&Field> = fields.iter().filter(|f| f.name.as_deref() != Some("at")).collect();
        let [action] = actions.as_slice() else { return Err(self.error(node.span.start, "exactly one of settle, press, drag, key per beat")); };
        let action = match action.name.as_deref() {
            Some("settle") => Action::Settle(self.seconds(&action.value)?),
            Some("press") => Action::Press(self.string(&action.value)?),
            Some("key") => Action::Key(self.string(&action.value)?),
            Some("drag") => self.drag(&action.value)?,
            _ => unreachable!("object checked the beat's fields"),
        };
        Ok(Event { at, line: self.line(node.span.start), action })
    }
}

fn field<'a>(fields: &'a [Field], name: &str) -> Option<&'a Node> { fields.iter().find(|f| f.name.as_deref() == Some(name)).map(|f| &f.value) }

pub fn read(source: &str) -> Result<Tour, String> {
    let reader = Reader { source };
    let tree = syntax::parse_with(source, &UnitTable::default()).map_err(|e| reader.error(e.at, &format!("valid Splash ({})", e.message)))?;
    let [entry] = tree.statements() else { return Err(reader.error(tree.statements().get(1).map_or(0, |n| n.span.start), "one 'tour: {...}' document")); };
    let Kind::Entry { name, op, value, .. } = &entry.kind else { return Err(reader.error(entry.span.start, "tour: {...}")); };
    if name != "tour" || op != ":" { return Err(reader.error(entry.span.start, "tour: {...}")); }
    let fields = reader.object(value, &["app", "cursor", "record", "beats", "grabs"])?;
    let app = reader.string(reader.required(value, fields, "app")?)?;
    let cursor = field(fields, "cursor").map(|n| reader.string(n)).transpose()?.unwrap_or_else(|| "app".into());
    if !["app", "arrow", "hand", "text", "crosshair"].contains(&cursor.as_str()) { return Err(reader.error(field(fields, "cursor").unwrap().span.start, "cursor style app, arrow, hand, text or crosshair")); }
    let record = field(fields, "record").map(|n| reader.string(n)).transpose()?;
    if let Some(record) = &record {
        if !Path::new(record).components().all(|c| matches!(c, Component::Normal(_))) { return Err(reader.error(field(fields, "record").unwrap().span.start, "a recording path relative to --out, without '.' or '..'")); }
    }
    let beats = reader.array(reader.required(value, fields, "beats")?)?;
    let mut events = beats.iter().map(|n| reader.beat(n)).collect::<Result<Vec<_>, _>>()?;
    if let Some(grabs) = field(fields, "grabs") {
        for grab in reader.array(grabs)? { events.push(Event { at: reader.seconds(grab)?, line: reader.line(grab.span.start), action: Action::Grab }); }
    }
    // Stable order: simultaneous beats retain document order and precede simultaneous grabs.
    events.sort_by_key(|event| event.at);
    Ok(Tour { app, cursor, record, events })
}

pub fn time(at: Duration) -> String {
    let text = at.as_secs_f64().to_string();
    if text.contains('.') { text } else { format!("{text}.0") }
}
