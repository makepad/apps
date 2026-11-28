//! Read the peer vocabulary as data. The syntax tree is stored in an arena:
//! let references share an index and never recursively own their expansion.

use super::{subset, PlainDoc, Layer, LayerKind, RectProps, EllipseProps, PathProps, TextProps, GroupProps, RepeatProps};
use crate::model::*;
use makepad_splash_syntax::{self as syntax, Kind};
use std::collections::HashMap;

type Id = usize;

#[derive(Debug)]
enum Value {
    Num(f64),
    Str(String),
    Tag(String),
    Color(u32),
    Bool(bool),
    List(Vec<Id>),
    Object { ty: Option<String>, fields: Vec<Field> },
    Call(String, Vec<Id>),
    Invalid,
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    offset: usize,
    value: Id,
}

#[derive(Debug)]
struct Node {
    value: Value,
    offset: usize,
    cost: LayerCounts,
}

#[derive(Clone, Copy, Debug, Default)]
struct LayerCounts {
    lowering: u64,
    drawing: u64,
}

impl LayerCounts {
    const ONE: Self = Self { lowering: 1, drawing: 1 };

    fn saturating_add(self, other: Self) -> Self {
        Self { lowering: self.lowering.saturating_add(other.lowering), drawing: self.drawing.saturating_add(other.drawing) }
    }
}

/// A parsed data source. Cost is computed on its shared syntax, before lowering.
#[derive(Debug)]
pub struct Source {
    nodes: Vec<Node>,
    root: Id,
    lines: Vec<usize>,
    wide_chars: Vec<(usize, usize)>,
}

/// Parse and lower one Motion document without evaluating Splash.
pub fn read(text: &str) -> Result<PlainDoc, Vec<Diagnostic>> {
    Source::parse(text)?.lower()
}

impl Source {
    fn loc(&self, offset: usize) -> Loc {
        let line = self.lines.partition_point(|start| *start <= offset) - 1;
        let extra = |pos| {
            let count = self.wide_chars.partition_point(|(at, _)| *at < pos);
            count.checked_sub(1).map_or(0, |i| self.wide_chars[i].1)
        };
        let start = self.lines[line];
        let col = offset - start - (extra(offset) - extra(start));
        Loc { line: (line + 1) as u32, col: (col + 1) as u32 }
    }

    pub fn parse(text: &str) -> Result<Self, Vec<Diagnostic>> {
        let mut extra = 0;
        let mut source = Self {
            wide_chars: text.char_indices().filter_map(|(at, ch)| {
                extra += ch.len_utf8() - 1;
                (ch.len_utf8() > 1).then_some((at, extra))
            }).collect(),
            lines: std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect(),
            nodes: Vec::new(), root: 0,
        };
        let mut errors = Vec::new();
        let parsed = syntax::parse(text);
        let first_stmt = parsed.stmts.first().map_or(text.len(), |stmt| stmt.start);
        let mut has_header = false;
        for import in &parsed.uses {
            if import.start < first_stmt && import.path == "mod.motion.*" {
                has_header = true;
            } else {
                errors.push(Diagnostic::error(source.loc(import.start), "", format!(
                    "`use {}` is not in the data subset: a data source imports only `use mod.motion.*`, at the top", import.path
                )));
            }
        }
        if !has_header {
            let start = parsed.uses.first().map_or(first_stmt, |u| u.start.min(first_stmt));
            errors.push(Diagnostic::error(source.loc(start), "", "a data source starts with `use mod.motion.*`"));
        }
        for (offset, message) in parsed.errors {
            errors.push(Diagnostic::error(source.loc(offset), "", message));
        }
        let mut bindings = HashMap::new();
        let mut references = Vec::new();
        let mut root = None;
        for stmt in parsed.stmts {
            let mut path = stmt.name.clone().unwrap_or_default();
            let bare_motion = stmt.value.ty() == Some("Motion");
            let id = source.store(stmt.value, &bindings, &mut path, &mut references, &mut errors);
            if let Some(name) = stmt.name {
                if root.is_some() {
                    errors.push(Diagnostic::error(source.loc(stmt.start), &name, "a let precedes the Motion root"));
                }
                if subset::constructor(&name).is_some() {
                    errors.push(Diagnostic::error(source.loc(stmt.start), &name, format!("`let {name}` shadows Motion's `{name}`; give it another name")));
                }
                if syntax::RESERVED.contains(&name.as_str()) || matches!(name.as_str(), "ok" | "mut" | "elif" | "and" | "or" | "is" | "do" | "try") {
                    errors.push(Diagnostic::error(source.loc(stmt.start), &name, format!("{} is a reserved Splash word", name)));
                }
                bindings.insert(name, id);
            } else {
                if root.is_some() || !bare_motion {
                    errors.push(Diagnostic::error(source.loc(stmt.start), "", "a data source ends with one bare `Motion{...}`"));
                }
                root = Some(id);
            }
        }
        let inputs = root.and_then(|id| source.field(id, "inputs")).map(|f| f.value);
        for (name, offset, path) in references {
            if inputs.and_then(|id| source.field(id, &name)).is_none() {
                errors.push(Diagnostic::error(source.loc(offset), path, format!("`in(@{name})` names no input")));
            }
        }
        if root.is_none() && errors.is_empty() {
            errors.push(Diagnostic::error(source.loc(0), "", "a data source ends with one bare `Motion{...}`"));
        }
        if !errors.is_empty() { return Err(errors); }
        source.root = root.unwrap();
        Ok(source)
    }

    /// The larger of two document-wide counts: the `Layer` values lowering builds
    /// and the layers drawn after expanding repeats. Lowering builds each Repeat
    /// and its template once, including at count zero. Both counts saturate at `u64::MAX`.
    pub fn cost(&self) -> u64 {
        let counts = self.nodes[self.root].cost;
        counts.lowering.max(counts.drawing)
    }

    fn ty(&self, id: Id) -> Option<&str> {
        match &self.nodes[id].value {
            Value::Object { ty, .. } => ty.as_deref(),
            _ => None,
        }
    }

    fn field(&self, id: Id, name: &str) -> Option<&Field> {
        match &self.nodes[id].value {
            Value::Object { fields, .. } => fields.iter().rev().find(|f| f.name == name),
            _ => None,
        }
    }

    fn ease(&self, arg: Option<Id>) -> Result<crate::Ease, String> {
        let Some(id) = arg else { return Ok(crate::Ease::Linear) };
        let Value::Tag(tag) = &self.nodes[id].value else { return Err("expected a named easing tag; key handles are not in the data subset".into()) };
        match crate::Ease::from_id(tag) {
            Some(ease) if !matches!(ease, crate::Ease::Shaped(_)) => Ok(ease),
            _ => Err(format!("`@{tag}` is not an easing tag in the data subset")),
        }
    }

    // Validate forms even in unused bindings and overridden properties. Nothing
    // here expands a layer or constructs a track.
    fn check_call(&self, name: &str, args: &[Id], at: usize, path: &str,
                  references: &mut Vec<(String, usize, String)>, errors: &mut Vec<Diagnostic>) {
        let argument = |i: usize| args.get(i).map(|&id| &self.nodes[id].value);
        let arity = match name {
            "vec2" => 2..=2,
            "in" => 1..=1,
            "input" => 1..=3,
            "tween" => 4..=5,
            "repeat" => 2..=4,
            "keys" => 1..=usize::MAX,
            _ => return,
        };
        if !arity.contains(&args.len()) {
            errors.push(Diagnostic::error(self.loc(at), path, format!("invalid arguments to `{name}`")));
        }
        let mut fail = |id: Id, suffix: String, msg: String| {
            errors.push(Diagnostic::error(self.loc(self.nodes[id].offset), format!("{path}{suffix}"), msg));
        };
        match name {
            "in" => match argument(0) {
                Some(Value::Tag(tag)) => references.push((tag.clone(), at, path.to_owned())),
                Some(_) => fail(args[0], "[0]".into(), "`in(@name)` takes an input tag".into()),
                None => {}
            },
            "input" => match argument(0) {
                Some(Value::Tag(tag)) if !matches!(tag.as_str(), "text" | "number" | "color" | "vec2" | "bool") => {
                    fail(args[0], "[0]".into(), format!("input type `@{tag}` is not in the data subset"));
                }
                Some(Value::Tag(_)) | None => {}
                Some(_) => fail(args[0], "[0]".into(), "an input type is a tag".into()),
            },
            "keys" => for (i, &key) in args.iter().enumerate() {
                match &self.nodes[key].value {
                    Value::List(parts) if (2..=3).contains(&parts.len()) => {
                        if let Some(&ease) = parts.get(2) {
                            if let Err(msg) = self.ease(Some(ease)) {
                                fail(ease, format!("[{i}][2]"), msg);
                            }
                        }
                    }
                    _ => fail(key, format!("[{i}]"), format!("key {i} needs [time, value] or [time, value, @ease]")),
                }
            },
            "tween" => if let Some(&ease) = args.get(4) {
                if let Err(msg) = self.ease(Some(ease)) { fail(ease, "[4]".into(), msg); }
            },
            "repeat" => {
                if !args.is_empty() && !matches!(argument(0), Some(Value::Call(s,_)) if s == "keys" || s == "tween") {
                    fail(args[0], "[0]".into(), "`repeat` repeats keys or a tween".into());
                }
                if args.len() > 2 && !matches!(argument(2), Some(Value::Tag(s)) if s == "restart" || s == "pingpong") {
                    fail(args[2], "[2]".into(), "`repeat` mode is @restart or @pingpong".into());
                }
            }
            _ => {}
        }
    }

    // A postorder walk on the heap. Only the names in a prototype are copied;
    // its values (including arrays and layer templates) stay shared.
    fn store(&mut self, node: syntax::Node, bindings: &HashMap<String, Id>, path: &mut String, references: &mut Vec<(String, usize, String)>, errors: &mut Vec<Diagnostic>) -> Id {
        enum Pending {
            Visit(syntax::Node, String, usize),
            List(usize, usize),
            Call(String, usize, usize, usize),
            Object(Option<String>, Vec<(String, usize)>, usize, usize),
        }
        let mut pending = vec![Pending::Visit(node, String::new(), path.len())];
        let mut results = Vec::new();
        while let Some(work) = pending.pop() {
            let (value, offset) = match work {
                Pending::Visit(node, suffix, prefix) => {
                    path.truncate(prefix);
                    path.push_str(&suffix);
                    let prefix = path.len();
                    let at = node.start;
                    match node.kind {
                        Kind::Num(n) => (Value::Num(n), at),
                        Kind::Bool(b) => (Value::Bool(b), at),
                        Kind::Str(s) => (Value::Str(s), at),
                        Kind::Fn(_) => {
                            errors.push(Diagnostic::error(self.loc(at), &*path, "`fn` is code: a scripted source"));
                            (Value::Invalid, at)
                        }
                        Kind::Expr { op, .. } => {
                            let message = match op {
                                syntax::ExprOp::Subtract => "subtraction is code: put a comma before a negative data item",
                                syntax::ExprOp::Index => "indexing is code: put a comma before the next array",
                            };
                            errors.push(Diagnostic::error(self.loc(at), &*path, message));
                            (Value::Invalid, at)
                        }
                        Kind::Color(c) => (Value::Color(c), at),
                        Kind::Tag(s) => (Value::Tag(s), at),
                        Kind::Name(s) => {
                            if let Some(&id) = bindings.get(&s) {
                                results.push(id);
                                continue;
                            }
                            let msg = if s == "fn" { "`fn` is code: a scripted source".into() } else { format!("unknown name `{s}`: a value refers to an earlier let") };
                            errors.push(Diagnostic::error(self.loc(at), &*path, msg));
                            (Value::Invalid, at)
                        }
                        Kind::Nil | Kind::TooDeep(_) => {
                            errors.push(Diagnostic::error(self.loc(at), &*path, format!("{} is not in the data subset", node.what())));
                            (Value::Invalid, at)
                        }
                        Kind::Arr(items) => {
                            pending.push(Pending::List(at, items.len()));
                            for (i, child) in items.into_iter().enumerate().rev() {
                                pending.push(Pending::Visit(child, format!("[{i}]"), prefix));
                            }
                            continue;
                        }
                        Kind::Call { name, args } => {
                            if !subset::is_data_call(&name) {
                                let message = if name == "color" {
                                    "`color(...)` is not in the data subset: write the colour as `#rrggbb` or `#rrggbbaa`".into()
                                } else if subset::constructor(&name).is_none() {
                                    format!("`{name}` is unknown: a newer Work may draw it")
                                } else {
                                    format!("`{name}` is not in the data subset")
                                };
                                errors.push(Diagnostic::error(self.loc(at), &*path, message));
                            }
                            pending.push(Pending::Call(name, at, args.len(), prefix));
                            for (i, child) in args.into_iter().enumerate().rev() {
                                pending.push(Pending::Visit(child, format!("[{i}]"), prefix));
                            }
                            continue;
                        }
                        Kind::Obj { ty, open, fields, .. } => {
                            let base = ty.as_ref().and_then(|name| bindings.get(name)).copied();
                            let owner = base.and_then(|id| self.ty(id)).or(ty.as_deref());
                            if let Some(owner) = owner {
                                let classification = subset::constructor(owner);
                                if !subset::is_data_object(owner) {
                                    let msg = if classification.is_none() { format!("`{owner}` is unknown: a newer Work may draw it") } else { format!("`{owner}` is not in the data subset") };
                                    errors.push(Diagnostic::error(self.loc(at), &*path, msg));
                                }
                                for f in &fields {
                                    if owner == "Repeat" && f.name == "count" && !matches!(f.value.kind, Kind::Num(_)) {
                                        errors.push(Diagnostic::error(self.loc(f.value.start), property(path, "count"), "`Repeat.count` is a literal number"));
                                    }
                                    if subset::field(owner, &f.name) != Some(subset::Class::Admitted) {
                                        let name = &f.name;
                                        errors.push(Diagnostic::error(self.loc(f.start), property(path, name), format!("field `{name}` is not in the data subset")));
                                    }
                                }
                            }
                            pending.push(Pending::Object(ty, fields.iter().map(|f| (f.name.clone(), f.start)).collect(), open, prefix));
                            for f in fields.into_iter().rev() {
                                let suffix = if path.is_empty() { f.name } else { format!(".{}", f.name) };
                                pending.push(Pending::Visit(f.value, suffix, prefix));
                            }
                            continue;
                        }
                    }
                }
                Pending::List(at, n) => (Value::List(results.split_off(results.len()-n)), at),
                Pending::Call(name, at, n, prefix) => {
                    path.truncate(prefix);
                    let args = results.split_off(results.len() - n);
                    self.check_call(&name, &args, at, path, references, errors);
                    (Value::Call(name, args), at)
                }
                Pending::Object(ty, names, at, prefix) => {
                    path.truncate(prefix);
                    let args = results.split_off(results.len()-names.len());
                    let fields: Vec<_> = names.into_iter().zip(args).map(|((name, offset), value)| Field { name, offset, value }).collect();
                    let owner = ty.as_deref().and_then(|name| bindings.get(name).and_then(|&id| self.ty(id)).or(Some(name)));
                    for field in &fields {
                        let v = &self.nodes[field.value].value;
                        let msg = match (owner, field.name.as_str(), v) {
                            (Some("Font"), "family", Value::Str(name)) if is_font_path(name) => Some(font_path_error(name)),
                            (Some("Path"), "svg", Value::Call(name, _)) if matches!(name.as_str(), "keys" | "tween" | "repeat") => Some("a keyed `svg` is not in the data subset".into()),
                            _ => None,
                        };
                        if let Some(msg) = msg {
                            errors.push(Diagnostic::error(self.loc(field.offset), property(path, &field.name), msg));
                        }
                    }
                    // Splash assignment replaces the value at the original
                    // position. Field locations refer to its first spelling.
                    let mut positions = HashMap::new();
                    let mut unique: Vec<Field> = Vec::with_capacity(fields.len());
                    for field in fields {
                        if let Some(&i) = positions.get(&field.name) {
                            let old: &mut Field = &mut unique[i];
                            old.value = field.value;
                        } else {
                            positions.insert(field.name.clone(), unique.len());
                            unique.push(field);
                        }
                    }
                    let mut fields = unique;
                    let mut ty = ty;
                    if let Some(Value::Object { ty: original, fields: base }) = ty.as_ref().and_then(|name| bindings.get(name)).map(|&id| &self.nodes[id].value) {
                        let inherited: Vec<_> = base.iter().filter(|f| !fields.iter().any(|own| own.name == f.name)).cloned().collect();
                        fields.extend(inherited);
                        ty = original.clone();
                    }
                    (Value::Object { ty, fields }, at)
                }
            };
            let id = self.nodes.len();
            self.nodes.push(Node { value, offset, cost: LayerCounts::default() });
            let cost = match &self.nodes[id].value {
                Value::List(items) => items.iter().fold(LayerCounts::default(), |sum, &i| sum.saturating_add(self.nodes[i].cost)),
                Value::Object { ty: Some(t), .. } => {
                    let cost_of = |name| self.field(id, name).map(|f| self.nodes[f.value].cost).unwrap_or_default();
                    match t.as_str() {
                        "Motion" => cost_of("layers"),
                        "Group" => LayerCounts::ONE.saturating_add(cost_of("layers")).saturating_add(cost_of("mask")),
                        "Repeat" => {
                            let count = self.field(id, "count").and_then(|f| match self.nodes[f.value].value { Value::Num(n) => Some(n as u64), _ => None }).unwrap_or(1);
                            let template = cost_of("layer");
                            LayerCounts { lowering: template.lowering.saturating_add(1), drawing: template.drawing.saturating_mul(count) }
                        }
                        t if subset::layer(t) == Some(subset::Class::Admitted) => LayerCounts::ONE,
                        _ => LayerCounts::default(),
                    }
                }
                _ => LayerCounts::default(),
            };
            self.nodes[id].cost = cost;
            results.push(id);
        }
        results[0]
    }
}

fn font_path_error(name: &str) -> String {
    format!("font family `{name}` is a path: use a family from the document font table")
}

fn is_font_path(name: &str) -> bool {
    name.contains(['/', '\\']) || name.rsplit_once('.').is_some_and(|(_, ext)| {
                matches!(ext.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc" | "woff" | "woff2" | "dfont" | "eot" | "pfa" | "pfb")
            })
}

fn property(path: &str, name: &str) -> String {
    if path.is_empty() { name.to_owned() } else { format!("{path}.{name}") }
}

struct Lower<'a> {
    src: &'a Source,
    inputs: HashMap<String, InputValue>,
    width: f64,
    errors: Vec<Diagnostic>,
}

impl Source {
    /// Validate property values and build the model after the caller admits its cost.
    pub fn lower(&self) -> Result<PlainDoc, Vec<Diagnostic>> {
        let mut reader = Lower { src: self, inputs: HashMap::new(), width: 1920., errors: Vec::new() };
        let doc = reader.document();
        if reader.errors.is_empty() { Ok(doc) } else { Err(reader.errors) }
    }
}

impl Lower<'_> {
    fn err(&mut self, id: Id, path: &str, name: &str, message: impl Into<String>) {
        let offset = self.src.field(id, name).map_or(self.src.nodes[id].offset, |f| f.offset);
        let path = if name.is_empty() { path.to_string() } else { property(path, name) };
        self.errors.push(Diagnostic::error(self.src.loc(offset), path, message));
    }

    fn field<T>(&mut self, id: Id, path: &str, name: &str, f: impl FnOnce(&Self, Id) -> Result<T, String>) -> Option<T> {
        let value = self.src.field(id, name)?.value;
        match f(self, value) {
            Ok(v) => Some(v),
            Err(e) => { self.err(id, path, name, e); None }
        }
    }

    fn slot(&self, id: Id) -> Result<Option<&InputValue>, String> {
        let Value::Call(name, args) = &self.src.nodes[id].value else { return Ok(None) };
        if name != "in" { return Ok(None); }
        let [arg] = args[..] else { return Err("`in(@name)` takes one input name".into()) };
        let Value::Tag(name) = &self.src.nodes[arg].value else { return Err("`in(@name)` takes an input tag".into()) };
        self.inputs.get(name).map(Some).ok_or_else(|| format!("`in(@{name})` names no input"))
    }

    fn declarations(&self, obj: Id) -> (Vec<InputDecl>, Vec<Diagnostic>) {
        let Value::Object { ty: None, fields } = &self.src.nodes[obj].value else {
            return (vec![], vec![Diagnostic::error(self.src.loc(self.src.nodes[obj].offset), "inputs", "`inputs` is an object of named input declarations")]);
        };
        let mut decls = Vec::new();
        let mut errors = Vec::new();
        for field in fields {
            let loc = self.src.loc(field.offset);
            match self.declaration(field.value) {
                Ok((ty, default, doc)) => decls.push(InputDecl { id: crate::LiveId::from_str(&field.name), name: field.name.clone(), ty, default, doc, loc }),
                Err(msg) => errors.push(Diagnostic::error(loc, property("inputs", &field.name), msg)),
            }
        }
        (decls, errors)
    }

    fn declaration(&self, id: Id) -> Result<(InputType, InputValue, String), String> {
        let Value::Call(name, args) = &self.src.nodes[id].value else { return Err("expected input(@type, default, doc)".into()) };
        if name != "input" || !(1..=3).contains(&args.len()) { return Err("expected input(@type, default, doc)".into()); }
        let Value::Tag(ty) = &self.src.nodes[args[0]].value else { return Err("an input type is a tag".into()) };
        let default = args.get(1).copied();
        let (ty, value) = match ty.as_str() {
            "text" => (InputType::Text, InputValue::Text(default.map(|v| self.text(v)).transpose()?.unwrap_or_default())),
            "number" => (InputType::Number, InputValue::Number(default.map(|v| self.num(v)).transpose()?.unwrap_or(0.))),
            "color" => (InputType::Color, InputValue::Color(default.map(|v| self.color(v)).transpose()?.unwrap_or([1.; 4]))),
            "vec2" => (InputType::Vec2, InputValue::Vec2(default.map(|v| self.vec2(v)).transpose()?.unwrap_or([0.; 2]))),
            "bool" => (InputType::Bool, InputValue::Bool(default.map(|v| self.boolean(v)).transpose()?.unwrap_or(false))),
            _ => return Err(format!("input type `@{ty}` is not in the data subset")),
        };
        let doc = args.get(2).map(|&v| self.text(v)).transpose()?.unwrap_or_default();
        Ok((ty, value, doc))
    }

    fn num(&self, id: Id) -> Result<f64, String> {
        if let Some(input) = self.slot(id)? {
            return match input {
                InputValue::Number(v) => Ok(*v),
                other => Err(format!("expected number, but the input is {}", other.describe())),
            };
        }
        match self.src.nodes[id].value {
            Value::Num(n) => Ok(n),
            _ => Err("expected a number".into()),
        }
    }

    fn text(&self, id: Id) -> Result<String, String> {
        if let Some(input) = self.slot(id)? {
            return match input {
                InputValue::Text(v) => Ok(v.clone()),
                other => Err(format!("expected text, but the input is {}", other.describe())),
            };
        }
        match &self.src.nodes[id].value {
            Value::Str(s) => Ok(s.clone()),
            _ => Err("expected a string".into()),
        }
    }

    fn boolean(&self, id: Id) -> Result<bool, String> {
        if let Some(input) = self.slot(id)? {
            return match input {
                InputValue::Bool(v) => Ok(*v),
                other => Err(format!("expected bool, but the input is {}", other.describe())),
            };
        }
        match self.src.nodes[id].value {
            Value::Bool(b) => Ok(b),
            _ => Err("expected true or false".into()),
        }
    }

    fn vec2(&self, id: Id) -> Result<[f64; 2], String> {
        if let Some(input) = self.slot(id)? {
            return match input {
                InputValue::Vec2(v) => Ok(v.map(|n| n as f32 as f64)),
                other => Err(format!("expected vec2, but the input is {}", other.describe())),
            };
        }
        match &self.src.nodes[id].value {
            Value::List(v) if v.len() == 2 => Ok([self.num(v[0])?, self.num(v[1])?]),
            Value::Call(name, v) if name == "vec2" && v.len() == 2 => {
                // The document VM's vec2 is a two-lane float pod.
                let pair = [self.num(v[0])? as f32 as f64, self.num(v[1])? as f32 as f64];
                if pair.iter().all(|v| v.is_finite()) { Ok(pair) } else { Err("vec2 components must be finite".into()) }
            }
            _ => Err("expected vec2(x, y) or [x, y]".into()),
        }
    }

    fn color(&self, id: Id) -> Result<Color, String> {
        if let Some(input) = self.slot(id)? {
            return match input {
                InputValue::Color(v) => Ok(*v),
                other => Err(format!("expected color, but the input is {}", other.describe())),
            };
        }
        match &self.src.nodes[id].value {
            Value::Color(c) => Ok(crate::unpack(*c)),
            _ => Err("expected a colour".into()),
        }
    }

    fn constant<const N: usize>(&self, id: Id) -> Result<[f64; N], String> {
        let mut out = [0.; N];
        match N {
            1 => out[0] = match self.src.nodes[id].value {
                Value::Bool(b) => f64::from(u8::from(b)),
                _ if matches!(self.slot(id)?, Some(InputValue::Bool(_))) => f64::from(u8::from(self.boolean(id)?)),
                _ => self.num(id)?,
            },
            2 => match self.src.nodes[id].value {
                _ if matches!(self.slot(id)?, Some(InputValue::Number(_))) => out.fill(self.num(id)?),
                Value::Num(n) => out.fill(n),
                _ => out.copy_from_slice(&self.vec2(id)?),
            },
            4 => out.copy_from_slice(&self.color(id)?),
            _ => unreachable!("Motion data property dimension"),
        }
        Ok(out)
    }

    fn track<const N: usize>(&self, id: Id) -> Result<Track<N>, String> {
        let Value::Call(name, args) = &self.src.nodes[id].value else { return Err("expected keys or tween".into()) };
        let mut keys = Vec::new();
        match name.as_str() {
            "keys" => {
                for (i, &id) in args.iter().enumerate() {
                    let Value::List(parts) = &self.src.nodes[id].value else { return Err(format!("key {i} needs [time, value, @ease]")) };
                    if !(2..=3).contains(&parts.len()) { return Err(format!("key {i} needs [time, value] or [time, value, @ease]")); }
                    keys.push(Key { t: Time::seconds(self.num(parts[0])?), v: self.constant(parts[1])?, ease: self.src.ease(parts.get(2).copied())?, bez: None });
                }
            }
            "tween" if (4..=5).contains(&args.len()) => {
                let start = self.num(args[2])?;
                let end = self.num(args[3])?;
                if end <= start { return Err(format!("`tween` ends at {end} s, not after its start at {start} s")); }
                keys.push(Key { t: Time::seconds(start), v: self.constant(args[0])?, ease: crate::Ease::Linear, bez: None });
                keys.push(Key { t: Time::seconds(end), v: self.constant(args[1])?, ease: self.src.ease(args.get(4).copied())?, bez: None });
            }
            "repeat" if (2..=4).contains(&args.len()) => {
                if !matches!(&self.src.nodes[args[0]].value, Value::Call(name, _) if name == "keys" || name == "tween") {
                    return Err("`repeat` repeats keys or a tween".into());
                }
                let track = self.track(args[0])?;
                let mode = match args.get(2).map(|&i| &self.src.nodes[i].value) {
                    None => LoopMode::Restart,
                    Some(Value::Tag(s)) if s == "restart" => LoopMode::Restart,
                    Some(Value::Tag(s)) if s == "pingpong" => LoopMode::PingPong,
                    _ => return Err("`repeat` mode is @restart or @pingpong".into()),
                };
                let count = args.get(3).map(|&id| {
                    let n = self.num(id)?;
                    if n >= 1.0 && n.fract() == 0.0 { Ok(n as u32) } else { Err("`repeat` count is a whole number from 1".to_string()) }
                }).transpose()?;
                let mut period = Dur::seconds(self.num(args[1])?);
                if period.value == 0.0 {
                    let first = track.keys.first().expect("validated nonempty track").t.value;
                    let last = track.keys.last().expect("validated nonempty track").t.value;
                    period.value = (last - first).max(1e-6);
                }
                return Track::new(track.keys, Some(Repeat { period, mode, count }), track.before, track.after);
            }
            _ => return Err(format!("invalid arguments to `{name}`")),
        }
        Track::new(keys, None, Extrap::Constant, Extrap::Constant)
    }

    fn optional_prop<const N: usize>(&mut self, id: Id, path: &str, name: &str) -> Option<Prop<N>> {
        self.field(id, path, name, |r, v| match &r.src.nodes[v].value {
            Value::Call(name, _) if matches!(name.as_str(), "keys" | "tween" | "repeat") => r.track(v).map(Prop::Track),
            _ => Prop::finite(r.constant(v)?),
        })
    }

    fn prop<const N: usize>(&mut self, id: Id, path: &str, name: &str, default: [f64; N]) -> Prop<N> {
        self.optional_prop(id, path, name).unwrap_or_else(|| Prop::finite(default).expect("finite loader default"))
    }

    fn choice<T: Copy>(&mut self, id: Id, path: &str, name: &str, choices: &[(&str, T)], default: T) -> T {
        self.field(id, path, name, |r, v| {
            if let Value::Tag(tag) | Value::Str(tag) = &r.src.nodes[v].value {
                if let Some((_, value)) = choices.iter().find(|(key, _)| *key == tag) { return Ok(*value); }
            }
            Err(format!("`{name}` is one of {}", choices.iter().map(|(key, _)| format!("@{key}")).collect::<Vec<_>>().join(", ")))
        }).unwrap_or(default)
    }

    fn document(&mut self) -> PlainDoc {
        let id = self.src.root;
        let mut inputs = Vec::new();
        if let Some(f) = self.src.field(id, "inputs") {
            // The loader evaluates once with no defaults, then supplies the
            // values successfully declared on that pass to structural reads.
            let (first, _) = self.declarations(f.value);
            self.inputs = first.into_iter().map(|d| (d.name, d.default)).collect();
            let (decls, errors) = self.declarations(f.value);
            self.errors.extend(errors);
            inputs = decls;
        }
        let input_values = inputs.iter().map(|d| d.default.clone()).collect();
        for name in ["label", "brief"] {
            self.field(id, "", name, |r, v| r.text(v));
        }
        self.field(id, "", "tags", |r, v| match &r.src.nodes[v].value {
            Value::List(a) if a.iter().all(|&v| matches!(r.src.nodes[v].value, Value::Tag(_) | Value::Str(_))) => Ok(()),
            _ => Err("`tags` is an array of strings".into()),
        });
        let size = self.field(id, "", "size", |r, v| {
            let size = r.vec2(v)?;
            if size.iter().all(|n| (1.0..=16384.0).contains(n)) { Ok(size) }
            else { Err(format!("`size` {}x{} is outside 1..16384 units a side", size[0], size[1])) }
        }).unwrap_or([1920., 1080.]);
        self.width = size[0];
        let duration = self.field(id, "", "duration", |r, v| r.num(v).and_then(|n| {
            if n > 0.0 { Ok(n) } else { Err("`duration` is a positive number of seconds".into()) }
        })).unwrap_or(1.0);
        if self.src.field(id, "duration").is_none() {
            self.err(id, "", "duration", "a Motion document declares its `duration` in seconds");
        }
        let fps = self.field(id, "", "fps", |r, v| {
            let n = r.num(v)?;
            if n > 0.0 && n <= 1000.0 { Ok(n) } else { Err("`fps` is a positive frame rate up to 1000".into()) }
        }).unwrap_or(60.);
        let bpm = self.field(id, "", "bpm", |r, v| {
            let n = r.num(v)?;
            if n > 0.0 { Ok(n) } else { Err("`bpm` is a positive tempo".into()) }
        }).unwrap_or(120.);
        let background = self.field(id, "", "background", |r, v| r.color(v));
        let layers = self.layers(id, "");
        let stops = self.field(id, "", "stops", |r, v| {
            if let Value::List(items) = &r.src.nodes[v].value {
                let stops = items.iter().map(|&id| r.num(id)).collect::<Result<Vec<_>, _>>()?;
                if stops.iter().all(|s| (0.0..=duration).contains(s)) && stops.windows(2).all(|p| p[0] < p[1]) {
                    return Ok(stops);
                }
            }
            Err("`stops` needs a strictly increasing list of seconds in the document's duration".into())
        }).unwrap_or_default();
        PlainDoc { size, duration, fps, bpm, background, layers, stops, inputs, input_values }
    }

    fn layers(&mut self, id: Id, path: &str) -> Vec<Layer> {
        let Some(Value::List(items)) = self.src.field(id, "layers").map(|f| &self.src.nodes[f.value].value) else {
            self.err(id, path, "layers", "expected `layers: [...]`");
            return Vec::new();
        };
        let path = property(path, "layers");
        items.iter().enumerate().filter_map(|(i, &v)| self.layer_tree(v, format!("{path}[{i}]"))).collect()
    }

    // Let chains can nest layers much more deeply than the written braces.
    // Build children first, using heap storage for the pending visits.
    fn layer_tree(&mut self, root: Id, path: String) -> Option<Layer> {
        enum Visit { Enter(Id, String), Finish(Id, String, usize, bool) }
        let mut tasks = vec![Visit::Enter(root, path)];
        let mut results = Vec::new();
        while let Some(task) = tasks.pop() {
            match task {
                Visit::Enter(id, path) => {
                    let mut children = Vec::new();
                    let mut mask = false;
                    match self.src.ty(id) {
                        Some("Group") => {
                            if let Some(Value::List(items)) = self.src.field(id, "layers").map(|f| &self.src.nodes[f.value].value) {
                                for (i, &child) in items.iter().enumerate() {
                                    children.push(Visit::Enter(child, format!("{path}.layers[{i}]")));
                                }
                            } else { self.err(id, &path, "layers", "a Group has `layers: [...]`"); }
                            if let Some(f) = self.src.field(id, "mask") {
                                children.push(Visit::Enter(f.value, property(&path, "mask")));
                                mask = true;
                            }
                        }
                        Some("Repeat") => {
                            if let Some(f) = self.src.field(id, "layer") {
                                children.push(Visit::Enter(f.value, property(&path, "layer")));
                            } else { self.err(id, &path, "layer", "a Repeat copies one layer"); }
                        }
                        _ => {}
                    }
                    tasks.push(Visit::Finish(id, path, children.len(), mask));
                    tasks.extend(children.into_iter().rev());
                }
                Visit::Finish(id, path, n, mask) => {
                    let mut children: Vec<Option<Layer>> = results.split_off(results.len() - n);
                    let mask = if mask { children.pop().flatten().map(Box::new) } else { None };
                    let layer = self.layer(id, &path, children.into_iter().flatten().collect(), mask);
                    results.push(layer);
                }
            }
        }
        results.pop().flatten()
    }

    fn seconds(&mut self, id: Id, path: &str, name: &str) -> f64 {
        self.field(id, path, name, |r, v| {
            let n = r.num(v)?;
            if n < 0. { Err(format!("`{name}` is a length in seconds from 0")) } else { Ok(n) }
        }).unwrap_or(0.)
    }

    fn stagger(&mut self, id: Id, path: &str) -> Stagger {
        Stagger { step: Dur::seconds(self.seconds(id, path, "stagger")), order: StaggerOrder::Forward }
    }

    fn step(&mut self, id: Id, path: &str, name: &str) -> RepeatStep {
        let Some(record) = self.src.field(id, name).map(|f| f.value) else { return RepeatStep::default() };
        let Value::Object { ty: None, fields } = &self.src.nodes[record].value else {
            self.err(id, path, name, format!("`{name}` is {{x y rotate scale opacity}} per copy"));
            return RepeatStep::default();
        };
        let path = property(path, name);
        for field in fields {
            if !matches!(field.name.as_str(), "x" | "y" | "rotate" | "scale" | "opacity") {
                self.err(record, &path, &field.name, format!("field `{}` is not in the data subset", field.name));
            }
        }
        let mut num = |name| self.field(record, &path, name, |r, v| r.num(v)).unwrap_or(0.);
        RepeatStep { x: num("x"), y: num("y"), rotate: num("rotate"), scale: num("scale"), opacity: num("opacity") }
    }

    fn path_data(&mut self, id: Id, path: &str) -> PathShape {
        let svg = self.src.field(id, "svg");
        let points = self.src.field(id, "points");
        let data = match (svg, points) {
            (Some(_), None) => self.field(id, path, "svg", |r, v| {
                if matches!(&r.src.nodes[v].value, Value::Call(name, _) if matches!(name.as_str(), "keys" | "tween" | "repeat")) {
                    return Err("a keyed `svg` is not in the data subset".into());
                }
                let data = r.text(v)?;
                crate::geometry::check_path_data(&data)?;
                let (_, numbers) = crate::geometry::PathTemplate::parse(&data)?;
                if numbers.iter().any(|n| !(*n as f32).is_finite()) {
                    return Err("SVG path data needs finite coordinates".into());
                }
                Ok(data)
            }),
            (None, Some(_)) => {
                let closed = self.field(id, path, "closed", |r, v| r.boolean(v)).unwrap_or(false);
                let smooth = self.field(id, path, "smooth", |r, v| r.boolean(v)).unwrap_or(false);
                self.field(id, path, "points", |r, v| {
                    if let Value::List(items) = &r.src.nodes[v].value {
                        if items.len() >= 2 {
                            let points = items.iter().map(|&v| r.vec2(v)).collect::<Result<Vec<_>, _>>()?;
                            return Ok(crate::geometry::points_path(&points, closed, smooth));
                        }
                    }
                    Err("`points` is an array of at least two vec2s".into())
                })
            }
            (None, None) => { self.err(id, path, "svg", "a Path needs its `svg` path data or `points`"); None }
            _ => { self.err(id, path, "points", "a Path has `svg` path data or `points`, not both"); None }
        };
        PathShape::Static(data.map(|s| crate::geometry::parse_path(&s).into()).unwrap_or_default())
    }

    fn text_width(&mut self, id: Id, path: &str, name: &str) -> Option<f64> {
        self.field(id, path, name, |r, v| {
            if matches!(&r.src.nodes[v].value, Value::Tag(s) if s == "safe") {
                return Ok(r.width * crate::SAFE_WIDTH);
            }
            let width = r.num(v)?;
            if width > 0. { Ok(width) } else { Err(format!("`{name}` is a width in document units or `@safe`")) }
        })
    }

    fn face(&mut self, id: Id, path: &str) -> (String, f64, bool) {
        let default = ("Inter".to_owned(), 400., false);
        let Some(face) = self.src.field(id, "font").map(|f| f.value) else { return default };
        if self.src.ty(face) != Some("Font") {
            self.err(id, path, "font", "`font` is a Font value");
            return default;
        }
        let path = property(path, "font");
        let family = self.field(face, &path, "family", |r, v| {
            let name = r.text(v)?;
            if is_font_path(&name) { return Err(font_path_error(&name)); }
            makepad_doc_font_names::family(&name).map(|f| f.name.to_owned()).ok_or_else(|| format!("font \"{name}\" is no document font"))
        }).unwrap_or(default.0);
        let weight = self.field(face, &path, "weight", |r, v| {
            let weight = r.num(v)?;
            if (1.0..=1000.).contains(&weight) { Ok(weight) } else { Err(format!("the Font's `weight` is 100..900 (400 regular, 700 bold), not {weight}")) }
        }).unwrap_or(default.1);
        let italic = self.field(face, &path, "italic", |r, v| r.boolean(v)).unwrap_or(false);
        (family, weight, italic)
    }

    fn text_layer(&mut self, id: Id, path: &str) -> LayerKind {
        if self.src.field(id, "text").is_none() {
            self.err(id, path, "text", "a Text needs `text: \"…\"` (or `source: voice(@vo)` for a voiceover's words)");
        }
        let text = self.field(id, path, "text", |r, v| r.text(v)).unwrap_or_default();
        let (font, weight, italic) = self.face(id, path);
        let by = self.choice(id, path, "by", &[("whole", TextBy::Whole), ("word", TextBy::Word), ("letter", TextBy::Letter),
            ("char", TextBy::Letter), ("line", TextBy::Line), ("syllable", TextBy::Syllable)], TextBy::Whole);
        if by == TextBy::Syllable {
            self.err(id, path, "by", "`by: @syllable` needs the syllables of a voiceover: `source: voice(@vo)`");
        }
        LayerKind::Text { text, props: TextProps {
            font, weight, italic, by, stagger: self.stagger(id, path),
            size: self.prop(id, path, "size", [72.]), color: self.prop(id, path, "color", [1.; 4]),
            align: self.choice(id, path, "align", &[("left", Align::Left), ("right", Align::Right), ("center", Align::Center), ("centre", Align::Center)], Align::Left),
            valign: self.choice(id, path, "valign", &[("top", VAlign::Top), ("bottom", VAlign::Bottom), ("center", VAlign::Center), ("centre", VAlign::Center), ("middle", VAlign::Center)], VAlign::Top),
            wrap: self.text_width(id, path, "wrap"), fit: self.text_width(id, path, "fit"),
            line_height: self.field(id, path, "line_height", |r, v| r.num(v)).unwrap_or(1.2),
            tracking: self.prop(id, path, "tracking", [0.]), reveal: self.optional_prop(id, path, "reveal"),
        } }
    }

    fn layer(&mut self, id: Id, path: &str, mut children: Vec<Layer>, mask: Option<Box<Layer>>) -> Option<Layer> {
        let ty = self.src.ty(id).unwrap_or("");
        let kind = match ty {
            "Text" => self.text_layer(id, path),
            "Rect" | "Ellipse" | "Path" => {
                if ty != "Path" && self.src.field(id, "size").is_none() { self.err(id, path, "size", format!("a {ty} needs its `size`")); }
                let mut fill = self.optional_prop(id, path, "fill");
                let stroke = self.optional_prop(id, path, "stroke");
                if path.ends_with(".mask") && fill.is_none() && stroke.is_none() {
                    fill = Some(Prop::finite([1.; 4]).unwrap());
                }
                let width = self.prop(id, path, "width", [2.]);
                let trim_start = self.prop(id, path, "trim_start", [0.]);
                let trim_end = self.prop(id, path, "trim_end", [1.]);
                let cap = self.choice(id, path, "cap", &[("round", Cap::Round), ("butt", Cap::Butt), ("square", Cap::Square)], if ty == "Path" { Cap::Round } else { Cap::Butt });
                if ty == "Path" {
                    let data = self.path_data(id, path);
                    let join = self.choice(id, path, "join", &[("round", Join::Round), ("miter", Join::Miter), ("bevel", Join::Bevel)], Join::Round);
                    LayerKind::Path(PathProps { data, fill, stroke, width, trim_start, trim_end, cap, join })
                } else if ty == "Rect" {
                    let size = self.prop(id, path, "size", [0.; 2]);
                    LayerKind::Rect(RectProps { size, fill, stroke, width, trim_start, trim_end, cap, radius: self.prop(id, path, "radius", [0.]) })
                } else {
                    let size = self.prop(id, path, "size", [0.; 2]);
                    LayerKind::Ellipse(EllipseProps { size, fill, stroke, width, trim_start, trim_end, cap })
                }
            }
            "Group" => {
                let size = self.optional_prop(id, path, "size");
                if matches!(&size, Some(Prop::Const(s)) if s.iter().any(|n| *n < 0.)) {
                    self.err(id, path, "size", "a Group size cannot be negative");
                }
                let mask_mode = self.choice(id, path, "mask_mode", &[("alpha", MaskMode::Alpha), ("inverse", MaskMode::Inverse),
                    ("alpha_inverse", MaskMode::Inverse), ("luma", MaskMode::Luma), ("luma_inverse", MaskMode::LumaInverse)], MaskMode::Alpha);
                LayerKind::Group { layers: children, mask, props: GroupProps { mask_mode, stagger: self.stagger(id, path), size } }
            }
            "Repeat" => {
                let count = self.field(id, path, "count", |r, v| {
                    let n = r.num(v)?;
                    if n < 0. || n.fract() != 0. { Err(format!("`count` is a whole number of copies, not {n}")) } else { Ok(n as usize) }
                }).unwrap_or(1);
                let layout = self.field(id, path, "layout", |r, v| match &r.src.nodes[v].value {
                    Value::Tag(tag) if matches!(tag.as_str(), "grid" | "radial" | "line") => Ok(tag.clone()),
                    _ => Err("`layout` is @grid, @radial or @line in the data subset".into())
                }).unwrap_or_else(|| "grid".into());
                let spacing = self.field(id, path, "spacing", |r, v| r.constant::<2>(v)).unwrap_or([100.; 2]);
                let layout = match layout.as_str() {
                    "grid" => RepeatLayout::Grid {
                        columns: self.field(id, path, "columns", |r,v| r.num(v)).unwrap_or_else(|| (count as f64).sqrt().ceil()).max(1.) as usize, spacing,
                    },
                    "radial" => RepeatLayout::Radial {
                        radius: self.field(id, path, "radius", |r,v| r.num(v)).unwrap_or(200.),
                        start: self.field(id, path, "start", |r,v| r.num(v)).unwrap_or(0.),
                        sweep: self.field(id, path, "sweep", |r,v| r.num(v)).unwrap_or(360.),
                        face: self.field(id, path, "face", |r,v| r.boolean(v)).unwrap_or(false),
                    },
                    _ => RepeatLayout::Line,
                };
                LayerKind::Repeat { layer: Box::new(children.pop()?), props: RepeatProps { count, layout,
                    delay: self.seconds(id, path, "delay"), step: self.step(id, path, "step"), jitter: self.step(id, path, "jitter"),
                    seed: self.field(id, path, "seed", |r,v| r.num(v)).unwrap_or(1.) as u32 } }
            }
            _ => {
                self.err(id, path, "", format!("expected an admitted layer; cannot lower `{ty}` here"));
                return None;
            }
        };
        let name = self.field(id, path, "name", |r, v| r.text(v));
        let span = self.field(id, path, "span", |r, v| {
            if let Value::List(items) = &r.src.nodes[v].value {
                if let [start, len] = items[..] {
                    let start = r.num(start)?;
                    let length = r.num(len)?;
                    if length <= 0. { return Err("`span: [start, length]` has a positive length".into()); }
                    return Ok(Span { start: Time::seconds(start), length: Dur::seconds(length) });
                }
            }
            Err("`span` is `[start, length]` in seconds".into())
        });
        let anchor = self.field(id, path, "anchor", |r, v| r.vec2(v)).unwrap_or([0.5; 2]);
        let transform = Transform {
            at: self.prop(id, path, "at", [0.; 2]), x: self.prop(id, path, "x", [0.]), y: self.prop(id, path, "y", [0.]),
            scale: self.prop(id, path, "scale", [1.; 2]), rotate: self.prop(id, path, "rotate", [0.]), anchor, along: None,
        };
        let opacity = self.prop(id, path, "opacity", [1.]);
        let blur = self.prop(id, path, "blur", [0.]);
        if matches!(ty, "Group" | "Repeat") && self.src.field(id, "feather").is_some() {
            self.err(id, path, "feather", format!("feather softens shape, path, text and SVG edges; use blur for {ty}"));
        }
        let feather = self.prop(id, path, "feather", [0.]);
        if matches!(&feather, Prop::Const([n]) if *n < 0.) || matches!(&feather, Prop::Track(t) if t.keys.iter().any(|k| k.v[0] < 0.)) {
            self.err(id, path, "feather", "`feather` expects a non-negative softness in document units (including every key value)");
        }
        let feather_side = self.choice(id, path, "feather_side", &[("both", FeatherSide::Both), ("inner", FeatherSide::Inner), ("outer", FeatherSide::Outer)], FeatherSide::Both);
        let blend = self.choice(id, path, "blend", &[("normal", Blend::Normal), ("add", Blend::Add), ("screen", Blend::Screen), ("multiply", Blend::Multiply)], Blend::Normal);
        let visible = self.field(id, path, "visible", |r, v| r.boolean(v)).unwrap_or(true);
        let locked = self.field(id, path, "locked", |r, v| r.boolean(v)).unwrap_or(false);
        let hard_cut = self.field(id, path, "hard_cut", |r, v| r.boolean(v)).unwrap_or(false);
        let mut layer = Layer { name, path: path.to_string(), loc: self.src.loc(self.src.nodes[id].offset), span, transform, opacity, blur, feather, feather_side, blend, kind, animated: false, visible, locked, hard_cut };
        layer.animated = layer.is_animated();
        Some(layer)
    }
}
