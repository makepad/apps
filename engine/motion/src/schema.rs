//! What every value written in a Motion document is, for editors: the
//! document read as **sites**, each one value's bytes, who owns it (a
//! layer's field, a key of it, an input's default, a `let`) and its
//! [`ValueKind`], which picks the editor (a number scrubs, a colour has a
//! swatch, an ease its curve, a `@name` its choices, words a text box).
//!
//! One model for every view of an opened graphic: the Source view's inline
//! controls, the inspector, the layer timeline and the canvas all read
//! sites and write a site back with [`splice`], which replaces exactly its
//! bytes (the patcher's rule: the author's layout and comments stay).
//!
//! A value's kind comes from what it is written as (`#ff5a36` a colour,
//! `@ease_out` an ease, `vec2(..)` two numbers, a string words), and a
//! `@name`'s choices from every name that field is written with in the
//! layer catalogue and the document. Nothing here is a table per layer
//! type: every field of every component gets an editor.

use crate::patch::Document;
use makepad_script::syntax::{self, Kind, Node};
use std::ops::Range;
use std::sync::Arc;

/// What a value is, for its editor.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueKind {
    /// A number, or `vec2`/`vec3`/a list of numbers: `dims` components.
    Number { dims: usize },
    Color,
    /// Words; `multiline` when they may run over lines (a text, a caption).
    Text { multiline: bool },
    /// A font name.
    Font,
    /// `@name`: one of `choices` (as written, without `@`).
    Enum { choices: Vec<String> },
    /// `@ease_out`, `spring(k, d)`, `cubic(..)`.
    Ease,
    /// A time on the layer's clock (a key's time, a tween's ends).
    Time,
    Bool,
    /// An asset or media reference (`src: "…"`, `asset(..)`, `icon: "…"`).
    Media,
    /// A list or table of plain values (a chart's `data:`).
    Data,
    /// `in(@name)`: the value is the document input `name`'s.
    Input(String),
    /// A binding to the beat, a signal or the audio (`beat()`, `signal(..)`).
    Signal,
    /// Code: an expression, a function, a condition. Shown, not edited here.
    Code,
}

impl ValueKind {
    /// Whether an editor can change the value in place.
    pub fn editable(&self) -> bool {
        !matches!(self, ValueKind::Code | ValueKind::Input(_) | ValueKind::Signal)
    }
}

/// Who a value belongs to.
#[derive(Clone, Debug, PartialEq)]
pub enum Owner {
    /// A field of the document's `Motion{..}` (`duration`, `background`).
    Doc { field: String },
    /// A layer's field (the model's layer path, e.g. `layers[2].layers[0]`).
    /// `part` names a value inside it: `""` the whole value, else
    /// `x`/`y`/`z` of a vector, an index path of a list (`2`, `1.3`), a
    /// sub-field (`data.title`).
    Field { layer: String, field: String, part: String },
    /// Key `index` of a layer's animated field (`keys(..)` or `tween(..)`).
    Key { layer: String, field: String, index: usize, what: KeyPart },
    /// An input's default (in `inputs: {..}`).
    Input { name: String },
    /// A top-level `let`.
    Let { name: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyPart {
    Time,
    Value,
    /// The ease of the segment arriving at the key.
    Ease,
}

/// One value as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Site {
    /// The value's bytes in the source.
    pub range: Range<usize>,
    pub owner: Owner,
    pub kind: ValueKind,
    /// The value as written.
    pub text: String,
}

impl Site {
    /// The layer path, when a layer owns it.
    pub fn layer(&self) -> Option<&str> {
        match &self.owner {
            Owner::Field { layer, .. } | Owner::Key { layer, .. } => Some(layer),
            _ => None,
        }
    }

    /// The field name it is written under (`text`, `fill`, an input's name).
    pub fn field(&self) -> &str {
        match &self.owner {
            Owner::Doc { field } | Owner::Field { field, .. } | Owner::Key { field, .. } => field,
            Owner::Input { name } | Owner::Let { name } => name,
        }
    }

    /// Words a string site says (unescaped).
    pub fn words(&self) -> Option<String> {
        match syntax::parse_value(&self.text).ok()?.kind {
            Kind::Str(text) => Some(text),
            _ => None,
        }
    }

    /// Numbers a numeric site says.
    pub fn numbers(&self) -> Option<Vec<f64>> {
        crate::patch::literal_numbers(&self.text)
    }

    /// The colour a colour site says, 0..1 RGBA.
    pub fn color(&self) -> Option<[f64; 4]> {
        crate::patch::literal_color(&self.text)
    }
}

/// Every value written in `source`, in source order.
pub fn sites(source: &str) -> Result<Vec<Site>, String> {
    let syntax = syntax::parse(source).map_err(|error| {
        let (line, col) = syntax::line_col(source, error.at);
        format!("{line}:{col}: {}", error.message)
    })?;
    let tree = Document::of(&syntax);
    let mut reader = Reader { source, tree: &tree, out: Vec::new(), ty: None };
    for (name, value) in &tree.lets {
        // A `let` holding a layer list or a component is read where it is
        // used; a plain value is a site of its own.
        if is_plain(value) {
            reader.value(value, Owner::Let { name: name.to_string() }, name);
        }
    }
    if let Some(motion) = tree.motion() {
        reader.motion(motion);
    }
    // A `let` used by several layers is one value with several owners:
    // a site each (same bytes), so every owner finds its value.
    let mut out = reader.out;
    out.sort_by_key(|site| (site.range.start, site.range.end));
    Ok(out)
}

/// The site whose bytes hold `at` (the innermost).
pub fn site_at(sites: &[Site], at: usize) -> Option<&Site> {
    sites.iter().filter(|site| site.range.start <= at && at <= site.range.end).min_by_key(|site| site.range.len())
}

/// `source` with `range` replaced by `text`: one value's bytes. Refused
/// when `text` is not one value or the document no longer reads.
pub fn splice(source: &str, range: Range<usize>, text: &str) -> Result<String, String> {
    if range.end > source.len() || range.start > range.end || !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err("the value moved: the source changed under it".into());
    }
    syntax::parse_value(text).map_err(|error| format!("`{text}` is not a value: {}", error.message))?;
    let mut next = String::with_capacity(source.len() + text.len());
    next.push_str(&source[..range.start]);
    next.push_str(text);
    next.push_str(&source[range.end..]);
    syntax::parse(&next).map_err(|error| error.message)?;
    Ok(next)
}

/// `source` with `range` replaced by `text` when both read as Splash
/// tokens with their brackets balanced (code, a module, a function body:
/// what [`splice`]'s document reader does not take apart).
pub fn splice_code(source: &str, range: Range<usize>, text: &str) -> Result<String, String> {
    if range.end > source.len() || range.start > range.end || !source.is_char_boundary(range.start) || !source.is_char_boundary(range.end) {
        return Err("the value moved: the source changed under it".into());
    }
    fn balanced(text: &str) -> bool {
        use makepad_script::tokenizer::{lex, ScriptTokenTag};
        let mut depth: Vec<u8> = Vec::new();
        for t in lex(text) {
            match t.token().kind() {
                ScriptTokenTag::OpenCurly => depth.push(b'}'),
                ScriptTokenTag::OpenRound => depth.push(b')'),
                ScriptTokenTag::OpenSquare => depth.push(b']'),
                ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseSquare => {
                    let want = match t.token().kind() {
                        ScriptTokenTag::CloseCurly => b'}',
                        ScriptTokenTag::CloseRound => b')',
                        _ => b']',
                    };
                    if depth.pop() != Some(want) {
                        return false;
                    }
                }
                ScriptTokenTag::StringUnfinished => return false,
                _ => {}
            }
        }
        depth.is_empty()
    }
    if text.trim().is_empty() || !balanced(text) {
        return Err(format!("`{text}` is not a value"));
    }
    let mut next = String::with_capacity(source.len() + text.len());
    next.push_str(&source[..range.start]);
    next.push_str(text);
    next.push_str(&source[range.end..]);
    if !balanced(&next) {
        return Err("the code's brackets no longer match".into());
    }
    Ok(next)
}

/// What a layer type's field takes, as its component declares it
/// (`layers::field_specs`), per type once.
pub fn field_type(layer_type: &str, field: &str) -> Option<crate::layers::FieldType> {
    static SPECS: std::sync::OnceLock<std::sync::Mutex<Vec<(String, Vec<crate::layers::FieldSpec>)>>> = std::sync::OnceLock::new();
    let specs = SPECS.get_or_init(Default::default);
    let mut specs = specs.lock().ok()?;
    if !specs.iter().any(|(t, _)| t == layer_type) {
        specs.push((layer_type.to_string(), crate::layers::field_specs(layer_type)));
    }
    specs.iter().find(|(t, _)| t == layer_type)?.1.iter().find(|f| f.name == field).map(|f| f.ty.clone())
}

/// A value's kind from what its field takes, when the value is written as
/// that (else the written form decides).
fn kind_from_type(ty: &crate::layers::FieldType, node: &Node) -> Option<ValueKind> {
    use crate::layers::FieldType as T;
    let call = node.as_call().map(|(name, _)| name);
    let numeric = matches!(&node.kind, Kind::Number(_)) || call.is_some_and(|name| name.starts_with("vec")) || matches!(&node.kind, Kind::Array { items, .. } if items.iter().all(|i| i.number().is_some()));
    Some(match (ty, &node.kind) {
        (T::Number { dims }, _) if numeric => ValueKind::Number { dims: *dims },
        (T::Color, Kind::Color(_)) => ValueKind::Color,
        (T::Color, _) if matches!(call, Some("rgb" | "rgba")) => ValueKind::Color,
        (T::Text { multiline }, Kind::Str(_)) => ValueKind::Text { multiline: *multiline },
        (T::Font, Kind::Str(_)) => ValueKind::Font,
        (T::Enum { choices }, Kind::Name(_)) => ValueKind::Enum { choices: choices.iter().map(|c| c.to_string()).collect() },
        (T::Ease, Kind::Name(_)) => ValueKind::Ease,
        (T::Ease, _) if matches!(call, Some("spring" | "cubic" | "bezier")) || call.is_some_and(|c| makepad_tween::named::Shape::from_id(c).is_some()) => ValueKind::Ease,
        (T::Bool, Kind::Ident(word)) if word == "true" || word == "false" => ValueKind::Bool,
        (T::Media, Kind::Str(_)) => ValueKind::Media,
        (T::Media, _) if call.is_some() => ValueKind::Media,
        (T::Time | T::Length, Kind::Number(_)) => ValueKind::Number { dims: 1 },
        (T::Data, Kind::Array { .. }) => ValueKind::Data,
        (T::Code, _) => ValueKind::Code,
        _ => return None,
    })
}

/// A value with nothing to evaluate in it: literals, vectors, lists and
/// objects of them, calls like `rgba(..)`.
fn is_plain(node: &Node) -> bool {
    match &node.kind {
        Kind::Number(_) | Kind::Str(_) | Kind::Name(_) | Kind::Color(_) => true,
        Kind::Array { items, .. } => items.iter().all(is_plain),
        _ => matches!(node.as_call(), Some(("vec2" | "vec3" | "vec4" | "rgb" | "rgba", args)) if args.iter().all(is_plain)),
    }
}

/// Words fields: a text that may run over lines.
fn is_words_field(field: &str) -> bool {
    matches!(field, "text" | "caption" | "title" | "subtitle" | "label" | "body" | "lines" | "words" | "quote" | "sign" | "note")
}

fn is_media_field(field: &str) -> bool {
    matches!(field, "src" | "icon" | "image" | "video" | "model" | "audio" | "media")
}

struct Reader<'a> {
    source: &'a str,
    tree: &'a Document<'a>,
    out: Vec<Site>,
    /// The type of the layer being read (its fields' kinds).
    ty: Option<String>,
}

impl<'a> Reader<'a> {
    fn text(&self, node: &Node) -> String {
        self.source[node.span.clone()].to_string()
    }

    fn let_value(&self, name: &str) -> Option<&'a Node> {
        self.tree.let_value(name)
    }

    /// A value, following a bare name to its `let`.
    fn resolve(&self, node: &'a Node) -> &'a Node {
        let mut node = node;
        for _ in 0..16 {
            match &node.kind {
                Kind::Ident(name) => match self.let_value(name) {
                    Some(value) => node = value,
                    None => return node,
                },
                _ => return node,
            }
        }
        node
    }

    fn push(&mut self, node: &Node, owner: Owner, kind: ValueKind) {
        let text = self.text(node);
        self.out.push(Site { range: node.span.clone(), owner, kind, text });
    }

    fn motion(&mut self, motion: &'a Node) {
        let Some((_, fields)) = motion.object() else { return };
        for field in fields {
            let Some(name) = field.name.as_deref() else { continue };
            let value = self.resolve(&field.value);
            match name {
                "layers" => self.layer_list(value, "layers"),
                "inputs" => self.inputs(value),
                _ => self.value(&field.value, Owner::Doc { field: name.to_string() }, name),
            }
        }
    }

    fn inputs(&mut self, node: &'a Node) {
        let Some((_, fields)) = node.object() else { return };
        for field in fields {
            let Some(name) = field.name.clone() else { continue };
            let owner = Owner::Input { name: name.clone() };
            match field.value.call("input") {
                // `input(@kind, default, "about")`: the default, as its kind says.
                Some(args) => {
                    let kind = args.first().and_then(Node::name).unwrap_or("");
                    if let Some(default) = args.get(1) {
                        let value_kind = match kind {
                            "color" => ValueKind::Color,
                            "text" => ValueKind::Text { multiline: true },
                            "number" => ValueKind::Number { dims: 1 },
                            "font" => ValueKind::Font,
                            "media" | "image" | "video" | "asset" => ValueKind::Media,
                            _ => self.kind_of(default, &name),
                        };
                        self.push(default, owner, value_kind);
                    }
                }
                None => self.value(&field.value, owner, &name),
            }
        }
    }

    fn layer_list(&mut self, list: &'a Node, prefix: &str) {
        let Some(items) = list.items() else { return };
        for (index, item) in items.iter().enumerate() {
            self.layer(self.resolve(item), &format!("{prefix}[{index}]"));
        }
    }

    fn layer(&mut self, node: &'a Node, path: &str) {
        let Some((Some(ty), fields)) = node.object() else { return };
        let outer = self.ty.replace(ty.rsplit('.').next().unwrap_or(ty).to_string());
        self.layer_fields(fields, path);
        self.ty = outer;
    }

    fn layer_fields(&mut self, fields: &'a [syntax::Field], path: &str) {
        for field in fields {
            let Some(name) = field.name.as_deref() else { continue };
            let value = self.resolve(&field.value);
            match name {
                "layers" => self.layer_list(value, &format!("{path}.layers")),
                "mask" if value.object().is_some() => self.layer(value, &format!("{path}.mask")),
                "layer" if value.object().is_some_and(|(ty, _)| ty.is_some()) => self.layer(value, &format!("{path}.layer")),
                _ => self.field(path, name, &field.value),
            }
        }
    }

    /// A layer's field: plain, keyed, or code.
    fn field(&mut self, layer: &str, field: &str, written: &'a Node) {
        let node = self.resolve(written);
        // A value named by a `let` is edited where the `let` defines it
        // (as the patcher writes it): the site is the let's value.
        let owner = |part: &str| Owner::Field { layer: layer.to_string(), field: field.to_string(), part: part.to_string() };
        match &node.kind {
            _ if node.call("keys").is_some() => {
                let args = node.call("keys").unwrap();
                for (index, key) in args.iter().enumerate() {
                    let Some(items) = key.items() else { continue };
                    let key_owner = |what| Owner::Key { layer: layer.to_string(), field: field.to_string(), index, what };
                    if let Some(t) = items.first() {
                        self.push(t, key_owner(KeyPart::Time), if t.number().is_some() { ValueKind::Time } else { ValueKind::Code });
                    }
                    if let Some(v) = items.get(1) {
                        let kind = self.kind_of(v, field);
                        self.push(v, key_owner(KeyPart::Value), kind);
                    }
                    for extra in items.iter().skip(2) {
                        if extra.name() != Some("pin") {
                            self.push(extra, key_owner(KeyPart::Ease), ValueKind::Ease);
                        }
                    }
                }
            }
            _ if node.call("tween").is_some_and(|args| (4..=5).contains(&args.len())) => {
                for (i, arg) in node.call("tween").unwrap().iter().enumerate() {
                    let (index, what) = match i {
                        0 => (0, KeyPart::Value),
                        1 => (1, KeyPart::Value),
                        2 => (0, KeyPart::Time),
                        3 => (1, KeyPart::Time),
                        _ => (1, KeyPart::Ease),
                    };
                    let kind = match what {
                        KeyPart::Value => self.kind_of(arg, field),
                        KeyPart::Time if arg.number().is_some() => ValueKind::Time,
                        KeyPart::Time => ValueKind::Code,
                        KeyPart::Ease => ValueKind::Ease,
                    };
                    self.push(arg, Owner::Key { layer: layer.to_string(), field: field.to_string(), index, what }, kind);
                }
            }
            Kind::Object { ty: Some(ty), fields, .. } if ty == "Font" => {
                // A face (`font: Font{family: "Inter" weight: 800}`): its
                // family a font site, its weight and axes numbers, under
                // `font.family`, `font.weight`, ….
                for entry in fields {
                    let Some(name) = entry.name.as_deref() else { continue };
                    self.field(layer, &format!("{field}.{name}"), &entry.value);
                }
            }
            Kind::Object { ty: None, fields, .. } => {
                // A record (`walk: {start: .. steps: ..}`, a chart's
                // `data: {..}`): each entry under `field.entry`.
                for entry in fields {
                    let Some(name) = entry.name.as_deref() else { continue };
                    self.field(layer, &format!("{field}.{name}"), &entry.value);
                }
            }
            Kind::Array { items, .. } if !(items.len() <= 4 && items.iter().all(|item| item.number().is_some())) => {
                // A list (rows of data, labels, a pose track): each plain
                // entry its own site.
                for (index, item) in items.iter().enumerate() {
                    self.part(item, &owner, &index.to_string(), field);
                }
            }
            _ => self.value(written, owner(""), field),
        }
    }

    /// The plain values inside a list's item, under `part`.
    fn part(&mut self, node: &'a Node, owner: &dyn Fn(&str) -> Owner, part: &str, field: &str) {
        match &node.kind {
            Kind::Array { items, .. } => {
                for (index, item) in items.iter().enumerate() {
                    self.part(item, owner, &format!("{part}.{index}"), field);
                }
            }
            Kind::Object { fields, .. } => {
                for entry in fields {
                    if let Some(name) = entry.name.as_deref() {
                        self.part(&entry.value, owner, &format!("{part}.{name}"), name);
                    }
                }
            }
            _ => {
                let kind = self.kind_of(node, field);
                self.push(node, owner(part), kind);
            }
        }
    }

    /// A plain value under `owner`, following a `let` it names.
    fn value(&mut self, written: &'a Node, owner: Owner, field: &str) {
        let node = self.resolve(written);
        let kind = self.kind_of(node, field);
        self.push(node, owner, kind);
    }

    /// What `node`, written under `field`, is.
    fn kind_of(&self, node: &Node, field: &str) -> ValueKind {
        // What the component says the field takes, when written as that.
        if let Some(kind) = self.ty.as_deref().filter(|_| !field.contains('.')).and_then(|ty| field_type(ty, field)).and_then(|t| kind_from_type(&t, node)) {
            return kind;
        }
        let leaf = field.rsplit('.').next().unwrap_or(field);
        match &node.kind {
            Kind::Number(_) => ValueKind::Number { dims: 1 },
            Kind::Color(_) => ValueKind::Color,
            Kind::Str(_) if leaf == "font" || leaf == "family" => ValueKind::Font,
            Kind::Str(_) if is_media_field(leaf) => ValueKind::Media,
            Kind::Str(text) => ValueKind::Text { multiline: is_words_field(leaf) || text.contains('\n') },
            Kind::Name(name) if crate::Ease::from_id(name).is_some() || name == "linear" => ValueKind::Ease,
            Kind::Name(name) => ValueKind::Enum { choices: vec![name.clone()] },
            Kind::Ident(word) if word == "true" || word == "false" => ValueKind::Bool,
            Kind::Call { .. } => match node.as_call() {
                Some(("vec2" | "vec3" | "vec4", args)) if args.iter().all(|a| a.number().is_some()) => ValueKind::Number { dims: args.len() },
                Some(("rgb" | "rgba", _)) => ValueKind::Color,
                Some(("spring" | "cubic" | "bezier", _)) => ValueKind::Ease,
                Some((call, _)) if makepad_tween::named::Shape::from_id(call).is_some() => ValueKind::Ease,
                Some(("in", args)) => ValueKind::Input(args.first().and_then(Node::name).unwrap_or("").to_string()),
                Some(("asset" | "media" | "image" | "video", _)) => ValueKind::Media,
                Some(("beat" | "beats" | "bar" | "bars" | "signal" | "audio" | "band" | "onset" | "section", _)) => ValueKind::Signal,
                _ => ValueKind::Code,
            },
            Kind::Array { items, .. } if !items.is_empty() && items.iter().all(|item| item.number().is_some()) && items.len() <= 4 => ValueKind::Number { dims: items.len() },
            Kind::Array { .. } if is_plain(node) => ValueKind::Data,
            _ => ValueKind::Code,
        }
    }
}


/// Which text a code location is in: the document, or an imported module
/// (`id@rev` for a library revision, the name of a built-in, the path a
/// tree gave it).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CodeFile {
    Doc,
    Module(String),
}

/// A place in a document's code (1-based line and column, characters).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CodeAt {
    pub file: CodeFile,
    pub line: u32,
    pub col: u32,
    /// The line as it was evaluated (a module's kernels are lowered before
    /// it runs, which moves lines): found again in the written text.
    pub line_text: Option<String>,
}

impl CodeAt {
    /// Its byte offset in `text` (that file's source): on the written line
    /// that reads like the evaluated one, nearest the line it was on.
    pub fn byte_in(&self, text: &str) -> Option<usize> {
        let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let line_at = |i: usize| -> &str {
            let s = starts[i];
            &text[s..text[s..].find('\n').map_or(text.len(), |n| s + n)]
        };
        let mut row = (self.line.max(1) as usize - 1).min(starts.len().saturating_sub(1));
        let mut col = self.col.saturating_sub(1) as usize;
        if let Some(evaluated) = self.line_text.as_deref() {
            let want = evaluated.trim();
            if !want.is_empty() && line_at(row).trim() != want {
                let found = (0..starts.len()).filter(|&i| line_at(i).trim() == want).min_by_key(|&i| i.abs_diff(row));
                if let Some(i) = found {
                    let indent = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
                    col = (col + indent(line_at(i))).saturating_sub(indent(evaluated));
                    row = i;
                }
            }
        }
        let line = line_at(row);
        let at = line.char_indices().nth(col).map_or(line.len(), |(b, _)| b);
        Some(starts[row] + at)
    }
}

/// A Draw2D text run's box in its own units (`[x0, y0, x1, y1]`): its
/// measured width, the em around its baseline.
pub fn text_op_box(op: &crate::draw2d::TextOp) -> [f64; 4] {
    let n = op.text.chars().count() as f64;
    let width = crate::type_fns::text_width(&op.text, &op.font, op.px, op.weight, op.italic, &op.axes).unwrap_or(op.px * 0.55 * n) + op.ls * n;
    let x0 = op.x - op.align * width;
    let (top, bottom) = match op.baseline {
        crate::draw2d::Baseline::Alphabetic => (-0.8, 0.25),
        crate::draw2d::Baseline::Top => (0.0, 1.05),
        crate::draw2d::Baseline::Middle => (-0.525, 0.525),
        crate::draw2d::Baseline::Bottom => (-1.05, 0.0),
    };
    [x0, op.y + top * op.px, x0 + width, op.y + bottom * op.px]
}

/// What a `let` bound, as an editor shows it: short, and the times in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Watched {
    /// The value shown short (`4.734`, `"thread"`, `{…}`).
    pub text: String,
    pub number: Option<f64>,
    /// Spans in it: a `{start, end}` (a sung word: labelled by its word), a
    /// `vec2(a, b)` with a before b, an object or list of those.
    pub spans: Vec<(f64, f64, String)>,
    /// A list of rising numbers (beats, onsets).
    pub ticks: Vec<f64>,
    /// A vector (`vec2`..`vec4`): its components.
    pub vector: Option<Vec<f32>>,
    /// A colour, 0..1 RGBA.
    pub color: Option<[f32; 4]>,
    /// How many times it was bound while watched (a loop's lets: its
    /// iterations).
    pub binds: u32,
}

impl Watched {
    /// A vector or a colour, read cheaply (what a frame's `let`s bind
    /// often: positions, tints), else `None`.
    fn small(heap: &makepad_script::ScriptHeap, value: makepad_script::ScriptValue) -> Option<Watched> {
        if let Some(c) = value.as_color() {
            let ch = |s: u32| ((c >> s) & 0xff) as f32 / 255.0;
            let color = [ch(24), ch(16), ch(8), ch(0)];
            let text = format!("#{:06x}", c >> 8);
            return Some(Watched { text, color: Some(color), ..Default::default() });
        }
        let pod = value.as_pod()?;
        let (ty, data) = heap.pod_data(pod);
        use makepad_script::pod::{ScriptPodVec};
        let lanes = match ty.ty().as_vec() {
            Some(ScriptPodVec::Vec2f) => 2,
            Some(ScriptPodVec::Vec3f) => 3,
            Some(ScriptPodVec::Vec4f) => 4,
            _ => return None,
        };
        let v: Vec<f32> = data.iter().take(lanes).map(|b| f32::from_bits(*b)).collect();
        let text = format!("({})", v.iter().map(|x| show_number(*x as f64)).collect::<Vec<_>>().join(", "));
        // A rising pair inside the film may be a span: the host decides.
        let spans = if v.len() == 2 && v[0] < v[1] { vec![(v[0] as f64, v[1] as f64, String::new())] } else { Vec::new() };
        Some(Watched { text, vector: Some(v), spans, ..Default::default() })
    }
}

impl Watched {
    fn read(heap: &makepad_script::ScriptHeap, value: makepad_script::ScriptValue) -> Watched {
        use makepad_script::{LiveId, NoTrap, ScriptValue};
        let num = |v: ScriptValue| v.as_number();
        let words = |v: ScriptValue| -> Option<String> {
            if let Some(s) = v.as_string() {
                return Some(heap.string(s).to_string());
            }
            v.as_inline_string(|s| s.to_string())
        };
        let field = |o: makepad_script::ScriptObject, name: &str| heap.value(o, LiveId::from_str(name).into(), NoTrap);
        // `{start, end}` (labelled by `w` or `text`), or a rising pair.
        let span = |v: ScriptValue| -> Option<(f64, f64, String)> {
            if let Some(o) = v.as_object() {
                if heap.is_fn(o) {
                    return None;
                }
                if let (Some(a), Some(b)) = (num(field(o, "start")), num(field(o, "end"))) {
                    let label = words(field(o, "w")).or_else(|| words(field(o, "text"))).unwrap_or_default();
                    return (a <= b).then_some((a, b, label));
                }
                if let (Some(a), Some(b)) = (num(field(o, "x")), num(field(o, "y"))) {
                    return (a < b).then_some((a, b, String::new()));
                }
            }
            if let Some(pod) = v.as_pod() {
                let (ty, data) = heap.pod_data(pod);
                if matches!(ty.ty().as_vec(), Some(makepad_script::pod::ScriptPodVec::Vec2f)) && data.len() >= 2 {
                    let (a, b) = (f32::from_bits(data[0]) as f64, f32::from_bits(data[1]) as f64);
                    return (a < b).then_some((a, b, String::new()));
                }
            }
            if let Some(arr) = v.as_array() {
                if heap.array_len(arr) == 2 {
                    let (a, b) = (num(heap.array_index_unchecked(arr, 0))?, num(heap.array_index_unchecked(arr, 1))?);
                    return (a < b).then_some((a, b, String::new()));
                }
            }
            None
        };
        let mut out = Watched { number: num(value), ..Default::default() };
        if let Some(n) = out.number {
            out.text = show_number(n);
            return out;
        }
        if let Some(one) = span(value) {
            out.spans.push(one);
        } else if let Some(arr) = value.as_array() {
            let n = heap.array_len(arr).min(4096);
            let items: Vec<ScriptValue> = (0..n).map(|i| heap.array_index_unchecked(arr, i)).collect();
            let numbers: Vec<f64> = items.iter().filter_map(|v| num(*v)).collect();
            if numbers.len() == items.len() && numbers.len() >= 2 && numbers.windows(2).all(|w| w[0] <= w[1]) {
                out.ticks = numbers;
            } else {
                out.spans = items.iter().filter_map(|v| span(*v)).collect();
            }
        } else if let Some(o) = value.as_object() {
            // An object of spans (the words of a line, by name), and its
            // rising list of times (a song's beats).
            let rising = |v: ScriptValue| -> Option<Vec<f64>> {
                let arr = v.as_array()?;
                let n = heap.array_len(arr);
                if n < 4 {
                    return None;
                }
                let items: Option<Vec<f64>> = (0..n.min(8192)).map(|i| num(heap.array_index_unchecked(arr, i))).collect();
                items.filter(|items| items.windows(2).all(|w| w[0] <= w[1]))
            };
            let beats = rising(field(o, "beats"));
            heap.object_map_iter_ordered(o, |_, v| {
                if let Some(one) = span(v) {
                    out.spans.push(one);
                }
            });
            if let Some(beats) = beats {
                out.ticks = beats;
            }
        }
        // Said short: its words (a sung line), a string, how many items.
        let said = value.as_object().filter(|o| !heap.is_fn(*o)).and_then(|o| words(field(o, "text")).map(|t| format!("“{t}”")))
            .or_else(|| words(value).map(|t| format!("“{t}”")))
            .or_else(|| value.as_array().map(|a| {
                let n = heap.array_len(a);
                let nums: Vec<f64> = (0..n.min(8192)).filter_map(|i| heap.array_index_unchecked(a, i).as_number()).collect();
                if !nums.is_empty() && nums.len() == n.min(8192) {
                    let (lo, hi) = nums.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
                    format!("[{n}: {}…{}]", show_number(lo), show_number(hi))
                } else {
                    format!("[{n} items]")
                }
            }));
        let mut text = said.unwrap_or_else(|| {
            let mut text = String::new();
            heap.to_debug_string(value, &mut Vec::new(), &mut text, false, 0);
            // The heap's numbers for objects (`<817>`) say nothing.
            let mut clean = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(at) = rest.find('<') {
                let tail = &rest[at + 1..];
                let digits = tail.chars().take_while(char::is_ascii_digit).count();
                clean.push_str(&rest[..at]);
                if digits > 0 && tail[digits..].starts_with('>') {
                    rest = &tail[digits + 1..];
                } else {
                    clean.push('<');
                    rest = tail;
                }
            }
            clean.push_str(rest);
            text = clean;
            text
        });
        if text.chars().count() > 40 {
            text = text.chars().take(39).collect::<String>() + "…";
        }
        out.text = text;
        out
    }
}

/// A number shown short: three places at most, no trailing zeros.
pub fn show_number(n: f64) -> String {
    let text = format!("{n:.3}");
    let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text };
    if text == "-0" { "0".to_string() } else { text }
}

thread_local! {
    /// What `let`s bound while watched (a load, a frame), innermost watch
    /// last (a load evaluates its style and type modules inside).
    static LETS: std::cell::RefCell<Vec<std::collections::BTreeMap<makepad_script::ScriptIp, (makepad_script::LiveId, Watched)>>> = const { std::cell::RefCell::new(Vec::new()) };
}

// The VM's `let` hook while watched. A number keeps its last value (its
// text is written once, at `watch_end`: a loop binds it many times, and
// formatting each bind was a third of a document's load); any other value
// its first (read once).
thread_local! {
    /// An editor's load: the top-level `let`s' names, and the records and
    /// lists they bind (kept, rooted, after the load: where a drawn thing
    /// came from is read from them, see [`crate::MotionDoc::text_origin`]).
    static TOPS: std::cell::RefCell<Option<(std::collections::HashSet<makepad_script::LiveId>, Vec<(makepad_script::LiveId, makepad_script::ScriptValue)>)>> = const { std::cell::RefCell::new(None) };
}

/// Keep what the top-level `let`s named in `source` bind (an editor's load).
pub(crate) fn tops_begin(source: &str) {
    let names = source.lines().filter_map(|l| l.strip_prefix("let ")).map(|rest| rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>()).filter(|n| !n.is_empty()).map(|n| makepad_script::LiveId::from_str(&n)).collect();
    TOPS.with(|t| *t.borrow_mut() = Some((names, Vec::new())));
}

/// What the top-level `let`s bound that is a record or a list.
pub(crate) fn tops_end() -> Vec<(makepad_script::LiveId, makepad_script::ScriptValue)> {
    TOPS.with(|t| t.borrow_mut().take().map(|(_, v)| v).unwrap_or_default())
}

pub(crate) fn let_bound(heap: &makepad_script::ScriptHeap, ip: makepad_script::ScriptIp, id: makepad_script::LiveId, value: makepad_script::ScriptValue) {
    TOPS.with(|t| {
        if let Some((names, values)) = t.borrow_mut().as_mut() {
            if names.contains(&id) && (value.as_array().is_some() || value.as_object().is_some_and(|o| !heap.is_fn(o))) {
                match values.iter_mut().find(|(n, _)| *n == id) {
                    Some(slot) => slot.1 = value,
                    None => values.push((id, value)),
                }
            }
        }
    });
    LETS.with(|lets| {
        let mut lets = lets.borrow_mut();
        let Some(lets) = lets.last_mut() else { return };
        if let Some(n) = value.as_number() {
            let (_, w) = lets.entry(ip).or_insert_with(|| (id, Watched::default()));
            *w = Watched { number: Some(n), binds: w.binds + 1, ..Default::default() };
            return;
        }
        let binds = lets.get(&ip).map_or(0, |(_, w)| w.binds) + 1;
        // Vectors and colours keep their last value too.
        if let Some(mut small) = Watched::small(heap, value) {
            small.binds = binds;
            lets.insert(ip, (id, small));
            return;
        }
        if let Some((_, w)) = lets.get_mut(&ip) {
            w.binds = binds;
            return;
        }
        if value.as_object().is_some_and(|o| heap.is_fn(o)) {
            return;
        }
        let mut read = Watched::read(heap, value);
        read.binds = binds;
        lets.insert(ip, (id, read));
    });
}

/// Start watching what `let`s bind on this thread (the VM's hook is the
/// caller's to set: [`let_bound`]).
pub(crate) fn watch_begin() {
    LETS.with(|lets| lets.borrow_mut().push(std::collections::BTreeMap::new()));
}

/// Stop watching: what was bound (where, name, value).
pub(crate) fn watch_end() -> Vec<(makepad_script::ScriptIp, String, Watched)> {
    let lets = LETS.with(|lets| lets.borrow_mut().pop()).unwrap_or_default();
    lets.into_iter()
        .map(|(ip, (id, mut value))| {
            // A number's text, from its last value.
            if let (Some(n), true) = (value.number, value.text.is_empty()) {
                value.text = show_number(n);
            }
            (ip, id.as_string(|s| s.map(str::to_string)).unwrap_or_default(), value)
        })
        .collect()
}

/// [`crate::MotionDoc::patch_literal`] on a document's VM.
/// `value`: `None` only reads what a patch would reach (every place is
/// left holding what it holds).
pub(crate) fn patch_in_vm(docvm: &crate::value::DocVm, file: &CodeFile, text: &str, literal: Range<usize>, value: Option<Literal>) -> makepad_script::literal::LiteralReach {
    use makepad_script::literal::{load_use, LiteralReach, LiteralUse};
    let Some(site) = literal_site(file, text, literal.clone()) else { return LiteralReach::default() };
    let line_start = text[..literal.start].rfind('\n').map_or(0, |n| n + 1);
    let line_end = text[literal.start..].find('\n').map_or(text.len(), |n| literal.start + n);
    let line_text = text[line_start..line_end].trim().to_string();
    let col = site.col as usize;
    let line_no = site.line as usize;
    let lead = text[line_start..line_end].chars().count() - text[line_start..line_end].trim_start().chars().count();
    let dry = value.is_none();
    let is_color = text[literal.clone()].trim_start_matches('-').starts_with('#');
    // A number's code pushes its digits (a sign is an operator after it).
    let pushed = match value {
        Some(Literal::Number(v)) => makepad_script::ScriptValue::from_f64(if site.negative { -v } else { v }),
        Some(Literal::Color(c)) => makepad_script::ScriptValue::from_color(color_u32(c)),
        None => makepad_script::NIL,
    };
    let bound = match value {
        Some(Literal::Number(v)) => makepad_script::ScriptValue::from_f64(v),
        _ => pushed,
    };
    // (A read sets each place to what it holds.)
    let set = |was: makepad_script::ScriptValue| if dry { was } else { bound };
    let name = {
        let before = text[line_start..literal.start].trim_end();
        before.strip_suffix('=').and_then(|b| b.trim().strip_prefix("let ")).map(|n| n.trim().to_string())
            .filter(|_| text[literal.end..line_end].trim().is_empty() || text[literal.end..line_end].trim_start().starts_with("//"))
    };
    docvm.with(|evaluated| {
        // The file the literal's body evaluated under: a document or module
        // a tree gave a path to evaluated under that path, not under the
        // code file's name (`motion.splash`, `module:<key>`). A module is
        // named by its key or by that path.
        let want_file = match file {
            CodeFile::Doc => match &evaluated.source {
                crate::model::DocSource::File(name, _) => name.to_string(),
                crate::model::DocSource::Image(_) => site.file.clone(),
            },
            CodeFile::Module(key) => evaluated.module_sources.iter().find(|m| m.0 == site.file || m.1 == *key).map_or_else(|| site.file.clone(), |m| m.1.clone()),
        };
        let trace = evaluated.load_trace.take();
        let reach = evaluated.with_vm(|vm| {
            let mut changed = 0;
            let mut scope = None;
            let mut let_body: Option<u16> = None;
            // The immediates patched, and the records this literal built
            // (a field written as it): where built, the field, what it holds.
            let mut pushed_at: Vec<(u16, u32, bool)> = Vec::new();
            // The bodies run each frame from the same text that hold it.
            let mut copies: Vec<u16> = Vec::new();
            let mut records: Vec<(makepad_script::ScriptIp, makepad_script::LiveId, makepad_script::ScriptValue)> = Vec::new();
            {
                let mut bodies = vm.bx.code_ref().modules_mut();
                // A body compiled from the evaluated text with every
                // character kept where it was (`<file>#live`: a document's
                // modulated lets, run each frame) holds the literal at the
                // same place as the text it was made from.
                let same_place = format!("{want_file}#live");
                let mut found: Option<usize> = None;
                for body_index in 0..bodies.len() {
                    let Some(m) = bodies.module(body_index) else { continue };
                    let file = unrevised(m.view().file);
                    let copy = file == same_place;
                    if file != want_file && !copy {
                        continue;
                    }
                    let start_char = if copy {
                        let Some(at) = found else { continue };
                        at
                    } else {
                        // The same line in what was evaluated (a prefix or a
                        // lowering may have moved it), nearest where it was.
                        let lines: Vec<&str> = bodies.effective_code(body_index).split('\n').collect();
                        // (The first line may carry the evaluator's prefix
                        // before what was written: the line ends the same.)
                        let Some(at) = (0..lines.len()).filter(|&i| lines[i].trim_end().ends_with(&line_text)).min_by_key(|&i| i.abs_diff(line_no)) else { continue };
                        let eff = lines[at].trim_end();
                        let eff_lead = eff.chars().count() - line_text.chars().count();
                        lines[..at].iter().map(|l| l.chars().count() + 1).sum::<usize>() + col - lead + eff_lead
                    };
                    found = found.or(Some(start_char));
                    let tokens: Vec<usize> = bodies.tokens(body_index).iter().enumerate().filter(|(_, t)| t.span().start == start_char).map(|(i, _)| i).collect();
                    let field = tokens.first().and_then(|&t| record_field_of(bodies.tokens(body_index), t));
                    for k in 0..bodies.source_map(body_index).len() {
                        let source = bodies.source_map(body_index)[k];
                        let op = bodies.opcode_value(body_index, k);
                        let kind_matches = if is_color { op.as_color().is_some() } else { op.as_number().is_some() };
                        if source.is_some_and(|t| tokens.contains(&(t as usize))) && kind_matches {
                            let mut recorded = false;
                            if let Some((key, key_token)) = field {
                                // What the field holds now: what this code
                                // pushed (a sign is applied after).
                                let was = match op.as_number() {
                                    Some(n) if site.negative => makepad_script::ScriptValue::from_f64(-n),
                                    _ => op,
                                };
                                let sites = vm.bx.heap_ref().made_at_sites(body_index as u16);
                                let built = sites.into_iter().rev().filter(|ip| (ip.index as usize) < k).find(|ip| record_writes(bodies.tokens(body_index), bodies.source_map(body_index), ip.index as usize, key_token));
                                if let Some(ip) = built {
                                    records.push((ip, key, was));
                                    recorded = true;
                                }
                            }
                            if !dry {
                                bodies.set_opcode_value(body_index, k, pushed);
                            }
                            pushed_at.push((body_index as u16, k as u32, recorded));
                            if copy && !copies.contains(&(body_index as u16)) {
                                copies.push(body_index as u16);
                            }
                            changed += 1;
                        }
                    }
                    if name.is_some() && !copy {
                        scope = Some(bodies.end_or_initial_scope(body_index));
                        let_body = Some(body_index as u16);
                    }
                }
            }
            let mut reach = LiteralReach::default();
            // A literal compiled into a kernel (its fns, or a `let` it read
            // as a constant) shows once the document loads again.
            #[cfg(feature = "scene3d")]
            if crate::kernel2d::compiles(vm, &pushed_at, name.as_deref()) {
                reach.add_reload("compiled into a kernel");
            }
            // (The records patched, by where built and field: a load that read
            // one copied its value out.)
            let mut built: Vec<(makepad_script::ScriptIp, makepad_script::LiveId, makepad_script::ScriptValue)> = Vec::new();
            for (ip, key, was) in records {
                changed += vm.bx.heap_mut().poke_made_at_field(ip, key, was, set(was));
                built.push((ip, key, was));
            }
            #[cfg(feature = "scene3d")]
            if built.iter().any(|(ip, _, _)| crate::kernel2d::setup_reads(vm, *ip)) {
                reach.add_reload(crate::kernel2d::SETUP_READ);
            }
            let mut let_set = false;
            let mut let_was = makepad_script::NIL;
            if let (Some(name), Some(scope)) = (&name, scope) {
                let id = makepad_script::LiveId::from_str(name);
                let now = vm.bx.heap_mut().scope_value(scope, id, makepad_script::NoTrap);
                let kind_matches = if is_color { now.as_color().is_some() } else { now.as_number().is_some() };
                if kind_matches && vm.bx.heap_mut().poke_scope_value(scope, id, set(now)) {
                    changed += 1;
                    let_set = true;
                    let_was = now;
                    // A record the same text built from the name as it was
                    // (a module's exports, `{BONE: BONE}`): its field too.
                    if let Some(body) = let_body {
                        for ip in vm.bx.heap_ref().made_at_sites(body) {
                            let n = vm.bx.heap_mut().poke_made_at_field(ip, id, now, set(now));
                            if n > 0 {
                                built.push((ip, id, now));
                            }
                            changed += n;
                        }
                    }
                }
            }
            // The copies the load made of it, followed: wherever the load read
            // the `let` or a patched record field and stored the value as is
            // (another record's field, another top-level `let`), that place
            // is set too, and its own reads followed in turn. A read that
            // computed with it is only made again by a load.
            if let Some(trace) = &trace {
                let mut sources: Vec<CopySource> = Vec::new();
                if let (true, Some(name), Some(body)) = (let_set, &name, let_body) {
                    sources.push(CopySource::Name(makepad_script::LiveId::from_str(name), body, let_was));
                }
                sources.extend(built.iter().map(|(ip, key, was)| CopySource::Field(*ip, *key, *was)));
                let (places, why) = follow_copies(vm, trace, sources, (!dry).then_some(bound));
                changed += places;
                for why in why {
                    reach.add_reload(&why);
                }
            }
            // What the load made of each immediate it ran: stored as is
            // (patched above) or computed with (a reload makes it again).
            if let Some(trace) = &trace {
                // (A literal a body run each frame holds: what the load made
                // of it is made again each frame from that body.)
                for (body, k, recorded) in pushed_at.into_iter().filter(|(body, _, _)| copies.is_empty() || copies.contains(body)) {
                    let why = match load_use(&vm.bx.code_ref(), trace, body, k) {
                        LiteralUse::Code => None,
                        LiteralUse::Let if let_set => None,
                        LiteralUse::Let => Some("bound by a `let` the load ran (not found to set)"),
                        LiteralUse::Field if recorded => None,
                        LiteralUse::Field => Some("stored in a record the load built (not found to set)"),
                        other => other.reload_reason(),
                    };
                    if let Some(why) = why {
                        reach.add_reload(why);
                    }
                }
            }
            reach.set_places(changed);
            reach
        });
        evaluated.load_trace = trace;
        reach
    })
}

/// An editor's document as a sink of the literal-site map: a site in the
/// document (`DOC_CODE_FILE`) or a module (`module:<key>`) is set in its VM
/// ([`patch_in_vm`]), in the text the document was loaded from.
pub struct DocLiterals {
    vm: std::sync::Weak<crate::value::DocVm>,
    source: Arc<str>,
    /// The name its own code's sites are given ([`crate::editable_as`]).
    file: String,
}

impl std::fmt::Debug for DocLiterals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DocLiterals({} bytes)", self.source.len())
    }
}

impl DocLiterals {
    /// For a document loaded as an editor's (its load traced): registered
    /// with the map for as long as the document lives.
    pub(crate) fn register(source: &str, vm: &Arc<crate::value::DocVm>) -> Option<Arc<DocLiterals>> {
        if !vm.with(|evaluated| evaluated.load_trace.is_some()) {
            return None;
        }
        let sink = Arc::new(DocLiterals { vm: Arc::downgrade(vm), source: source.into(), file: crate::value::doc_code_file() });
        let weak: std::sync::Weak<dyn makepad_script::literal::LiteralSink> = Arc::downgrade(&(sink.clone() as Arc<dyn makepad_script::literal::LiteralSink>));
        makepad_script::literal::add_sink(weak);
        Some(sink)
    }
}

impl DocLiterals {
    /// The name its own code's literal sites are given.
    pub fn file(&self) -> &str {
        &self.file
    }
}

impl makepad_script::literal::LiteralSink for DocLiterals {
    fn patch(&self, site: &makepad_script::literal::LiteralSite, value: makepad_script::literal::LiteralValue) -> makepad_script::literal::LiteralReach {
        let Some(vm) = self.vm.upgrade() else { return Default::default() };
        let (file, text): (CodeFile, Arc<str>) = if site.file() == self.file {
            (CodeFile::Doc, self.source.clone())
        } else if let Some(key) = site.file().strip_prefix("module:") {
            match vm.with(|evaluated| evaluated.module_sources.iter().find(|m| m.0 == site.file()).map(|m| m.2.clone())) {
                Some(text) => (CodeFile::Module(key.to_string()), text),
                None => return Default::default(),
            }
        } else {
            return Default::default();
        };
        let Some((range, negative)) = literal_at(&text, site.line(), site.col()) else { return Default::default() };
        let value = if let Some(v) = value.as_number() { Literal::Number(if negative { -v } else { v }) } else { Literal::Color(value.as_color().unwrap()) };
        patch_in_vm(&vm, &file, &text, range, Some(value))
    }
}

/// The bytes of the literal whose first character (a number's digits, a
/// colour's `#`) is at zero-based `line`, `col` (characters) of `text`,
/// with a unary `-` written right before it, and whether there is one.
pub fn literal_at(text: &str, line: u32, col: u32) -> Option<(Range<usize>, bool)> {
    let line_start = if line == 0 { 0 } else { text.match_indices('\n').nth(line as usize - 1)? .0 + 1 };
    let start = line_start + text[line_start..].chars().take(col as usize).map(char::len_utf8).sum::<usize>();
    let rest = text.get(start..)?;
    let len = if rest.starts_with('#') {
        1 + rest[1..].chars().take_while(|c| c.is_ascii_hexdigit()).count()
    } else {
        let digits = rest.char_indices().take_while(|(i, c)| c.is_ascii_digit() || *c == '.' || ((*c == 'e' || *c == 'E') && *i > 0)).count();
        digits
    };
    if len == 0 || (rest.starts_with('#') && len == 1) {
        return None;
    }
    // A unary minus: right before, after an operator, an opener or a separator.
    let before = text[..start].chars().rev();
    let mut it = before;
    let negative = it.next() == Some('-') && it.find(|c| !c.is_whitespace()).is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == ')' || c == ']'));
    let from = if negative { start - 1 } else { start };
    Some((from..start + len, negative))
}

impl crate::MotionDoc {
    /// An editor's document: the text module `key` (`CodeFile::Module`) had
    /// when the document loaded, the one its literal sites are in (what an
    /// edited module is diffed against for a literal-only edit).
    pub fn loaded_module_text(&self, key: &str) -> Option<Arc<str>> {
        let file = format!("module:{key}");
        self.vm.with(|evaluated| evaluated.module_sources.iter().find(|m| m.0 == file || m.1 == key).map(|m| m.2.clone()))
    }
}

/// The site and value an edit of the literal at `literal` (its bytes in
/// `file`'s text `text`, the one the running document was loaded from)
/// patches through `Cx::patch_literal`: a number's digits as written (a
/// sign before them stays the code's), or a colour.
pub fn site_value(file: &CodeFile, text: &str, literal: Range<usize>, value: Literal) -> Option<(makepad_script::literal::LiteralSite, makepad_script::literal::LiteralValue)> {
    let site = literal_site(file, text, literal)?;
    let value = match value {
        Literal::Number(v) => makepad_script::literal::LiteralValue::number(if site.negative { -v } else { v }),
        Literal::Color(c) => makepad_script::literal::LiteralValue::color(c),
    };
    Some((makepad_script::literal::LiteralSite::new(&site.shader_file, site.line, site.col), value))
}

/// A literal's new value, as an editor writes it: a number, or a colour
/// (`#rrggbbaa` as its four channels, 0..1, as written).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Literal {
    Number(f64),
    Color([f32; 4]),
}

/// Where a literal is written, the way the running document's code and its
/// compiled shaders name it: the file name its code carries (the document,
/// or `module:<key>`), the zero-based line and the column (characters) of
/// its first character (a number's digits, past a sign; a colour's `#`),
/// and whether a sign precedes it (the code pushes the digits and negates).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteralSite {
    pub file: String,
    /// The name compiled shaders' sites give the file
    /// ([`crate::DOC_CODE_FILE`] for the document).
    pub shader_file: String,
    pub line: u32,
    pub col: u32,
    pub negative: bool,
}

/// The site of the literal at bytes `literal` of `file`'s text `text`.
pub fn literal_site(file: &CodeFile, text: &str, literal: Range<usize>) -> Option<LiteralSite> {
    let written = text.get(literal.clone())?;
    let negative = written.trim_start().starts_with('-');
    let line_start = text[..literal.start].rfind('\n').map_or(0, |n| n + 1);
    let digits_at = literal.start + written.len() - written.trim_start_matches(['-', ' ']).len();
    let (file, shader_file) = match file {
        CodeFile::Doc => (crate::value::DOC_FILE.to_string(), crate::value::DOC_CODE_FILE.to_string()),
        CodeFile::Module(key) => (format!("module:{key}"), format!("module:{key}")),
    };
    Some(LiteralSite {
        file,
        shader_file,
        line: text[..line_start].matches('\n').count() as u32,
        col: text[line_start..digits_at].chars().count() as u32,
        negative,
    })
}

/// A literal written as a record's field (`alpha: 0.55`, `-0.2`, `#ff4d12`),
/// read from its body's tokens (`literal` its first): the field, and its
/// key's token. `None` when its value is computed from it (`0.025s /
/// SHEET`), or it is not a field's (an argument, a list's item).
fn record_field_of(tokens: &[makepad_script::tokenizer::ScriptTokenPos], literal: usize) -> Option<(makepad_script::LiveId, usize)> {
    use makepad_script::tokenizer::ScriptToken;
    let op_is = |t: &ScriptToken, s: &str| t.operator_or_separator() == Some(makepad_script::LiveId::from_str(s));
    if tokens.get(literal + 1).is_some_and(|t| t.token().is_operator() && t.token().operator() != makepad_script::LiveId::from_str(",")) {
        return None;
    }
    let mut k = literal.checked_sub(1)?;
    if op_is(tokens[k].token(), "-") {
        k = k.checked_sub(1)?;
    }
    if !op_is(tokens[k].token(), ":") {
        return None;
    }
    let key = k.checked_sub(1)?;
    tokens[key].token().as_identifier().map(|id| (id, key))
}

/// Whether the object literal begun at opcode `begin` writes the field
/// whose key is token `key_token` (at its own depth, not a nested one's).
fn record_writes(tokens: &[makepad_script::tokenizer::ScriptTokenPos], source_map: &[Option<u32>], begin: usize, key_token: usize) -> bool {
    use makepad_script::tokenizer::ScriptTokenTag;
    let at = begin.min(source_map.len().saturating_sub(1));
    let Some(first) = (0..=at).rev().find_map(|i| source_map.get(i).copied().flatten()).or_else(|| (at + 1..source_map.len()).find_map(|i| source_map[i])) else { return false };
    let Some(open) = tokens[first as usize..].iter().position(|t| matches!((t.token()).kind(), ScriptTokenTag::OpenCurly)).map(|p| p + first as usize) else { return false };
    if open > key_token {
        return false;
    }
    let mut depth = 0usize;
    for k in open..tokens.len() {
        match tokens[k].token().kind() {
            ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare => depth += 1,
            ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseSquare => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            _ if k == key_token => return depth == 1,
            _ => {}
        }
    }
    false
}

/// A place a patched value was stored in while the document loaded, whose
/// own reads by the load are followed ([`follow_copies`]): a top-level `let`
/// (its name, its body) or a record field (where the record was built, the
/// field), each with the value it held.
#[derive(Clone, Copy, PartialEq)]
enum CopySource {
    Name(makepad_script::LiveId, u16, makepad_script::ScriptValue),
    Field(makepad_script::ScriptIp, makepad_script::LiveId, makepad_script::ScriptValue),
}

/// Follow the copies a load made of a patched value (see the trace's
/// `name_reads`, `field_reads`): each read whose value was stored as is into
/// a record's field or a top-level `let` sets that place to `now` and is
/// followed from there; a read that computed with it is a reason to load.
/// How many places it set, and the reasons.
/// `now`: `None` only reads where the copies are.
fn follow_copies(vm: &mut makepad_script::ScriptVm, trace: &makepad_script::literal::LoadTrace, sources: Vec<CopySource>, now: Option<makepad_script::ScriptValue>) -> (usize, Vec<String>) {
    use makepad_script::opcode::Opcode;
    let mut places = 0;
    let mut why: Vec<String> = Vec::new();
    let mut seen: Vec<CopySource> = Vec::new();
    let mut work = sources;
    while let Some(source) = work.pop() {
        if seen.contains(&source) {
            continue;
        }
        seen.push(source);
        // Where the copied value was consumed: (body, instruction, value).
        let consumers: Vec<(u16, u32, makepad_script::ScriptValue)> = match source {
            CopySource::Name(id, _, was) => trace.read_sites(id, None).into_iter().map(|(b, i)| (b, i, was)).collect(),
            CopySource::Field(site, key, was) => trace.read_sites(key, Some(site)).into_iter().map(|(b, i)| (b, i + 1, was)).collect(),
        };
        for (body, at, was) in consumers {
            let now = now.unwrap_or(was);
            let (op, store) = {
                let b = vm.bx.body_module_read(body as usize);
                if !b.exists() { continue; }
                let op = b.opcode(at as usize);
                let tok = b.token_index(at as usize).map(|t| t as usize);
                (op, tok.and_then(|t| copy_target(b.tokens(), t)))
            };
            match (op, store, source) {
                // `look.BLUR`: the name navigated to a field (that read is
                // followed as a field's).
                (Some(Opcode::FIELD), _, CopySource::Name(..)) => {}
                (Some(op), Some((key, key_token)), _) if op == Opcode::ASSIGN_ME || op == Opcode::ASSIGN_ME_VEC => {
                    let site = {
                        let b = vm.bx.body_module_read(body as usize);
                        vm.bx.heap_ref().made_at_sites(body).into_iter().rev().filter(|ip| ip.index < at).find(|ip| record_writes(b.tokens(), b.source_map(), ip.index as usize, key_token))
                    };
                    match site {
                        Some(site) => {
                            let n = vm.bx.heap_mut().poke_made_at_field(site, key, was, now);
                            #[cfg(feature = "scene3d")]
                            if n > 0 && crate::kernel2d::setup_reads(vm, site) && !why.iter().any(|w| w == crate::kernel2d::SETUP_READ) {
                                why.push(crate::kernel2d::SETUP_READ.into());
                            }
                            if n > 0 {
                                places += n;
                                work.push(CopySource::Field(site, key, was));
                            }
                        }
                        None => why.push("copied into a record the load built (not found to set)".into()),
                    }
                }
                (Some(Opcode::LET_DYN | Opcode::LET_TYPED), Some((key, _)), _) => {
                    let scope = {
                        vm.bx.body_module_read(body as usize).end_or_initial_scope()
                    };
                    let Some(scope) = scope else { continue };
                    if vm.bx.heap_mut().scope_value(scope, key, makepad_script::NoTrap) == was && vm.bx.heap_mut().poke_scope_value(scope, key, now) {
                        places += 1;
                        work.push(CopySource::Name(key, body, was));
                    } else {
                        why.push("copied into a `let` the load bound (not found to set)".into());
                    }
                }
                _ => {
                    let reason = "computed with (or passed on) while the program loaded".to_string();
                    if !why.contains(&reason) {
                        why.push(reason);
                    }
                }
            }
        }
    }
    (places, why)
}

/// Where the value an instruction at token `tok` stores goes: a record's
/// field (`key: value`, the key and its token) or a `let` (`let name =`).
fn copy_target(tokens: &[makepad_script::tokenizer::ScriptTokenPos], tok: usize) -> Option<(makepad_script::LiveId, usize)> {

    let is_op = |t: Option<&makepad_script::tokenizer::ScriptTokenPos>, s: &str| t.and_then(|t| t.token().operator_or_separator()) == Some(makepad_script::LiveId::from_str(s));
    let ident = |k: usize| tokens.get(k).and_then(|t| t.token().as_identifier());
    // `key: …` with the instruction on the `:` or on the key.
    if is_op(tokens.get(tok), ":") {
        return ident(tok.checked_sub(1)?).map(|id| (id, tok - 1));
    }
    if is_op(tokens.get(tok + 1), ":") {
        return ident(tok).map(|id| (id, tok));
    }
    // `let name = …`: back from the instruction to its `let`.
    let mut k = tok;
    for _ in 0..64 {
        if ident(k) == Some(makepad_script::LiveId::from_str("let")) {
            return ident(k + 1).map(|id| (id, k + 1));
        }
        k = k.checked_sub(1)?;
    }
    None
}

/// A colour's channels as the script's packed `#rrggbbaa`.
/// Where a running document holds a colour as data ([`crate::MotionDoc::colour_data`]).
pub type ColourData = Vec<makepad_script::object_heap::ScriptValuePlace>;

fn color_u32(c: [f32; 4]) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c[0]) << 24) | (byte(c[1]) << 16) | (byte(c[2]) << 8) | byte(c[3])
}

impl crate::MotionDoc {
    /// The frame at `t` with each Draw2D list recording where its draw
    /// function was when it drew ([`crate::draw2d::Draw2dList::marks`]):
    /// what an editor maps the picture back to its code with. Other
    /// evaluations (renders, other threads) record nothing.
    pub fn eval_traced(&self, t: f64) -> crate::MotionFrame {
        self.eval_traced_pointer(t, t, None)
    }

    /// Editor picking at the host's exact story, ambient and pointer state.
    pub fn eval_traced_pointer(&self, t: f64, live: f64, pointer: Option<[f64; 2]>) -> crate::MotionFrame {
        crate::draw2d::TRACED.with(|traced| traced.set(true));
        let frame = self.eval_pointer(t, live, pointer);
        crate::draw2d::TRACED.with(|traced| traced.set(false));
        frame
    }

    /// [`Self::eval_traced`], also telling what each `let` bound on the
    /// way (its last value, if bound more than once): (where, name, value).
    /// What an editor writes beside the code as its live values; functions
    /// are left out.
    pub fn eval_watched(&self, t: f64) -> (crate::MotionFrame, Vec<(makepad_script::ScriptIp, String, Watched)>) {
        let set = |hook: Option<makepad_script::ScriptLetHook>| self.vm.with(|evaluated| evaluated.with_vm(|vm| vm.set_let_hook(hook)));
        set(Some(let_bound));
        watch_begin();
        let frame = self.eval_traced(t);
        let lets = watch_end();
        set(None);
        (frame, lets)
    }

    /// Set the literal written at `literal` (its bytes in `file`'s text
    /// `text`, the one the document was loaded from: a number or a
    /// `#colour`) to `value` in the running document, without loading it
    /// again: what its code pushes there, and what the load stored it in as
    /// is (a `let`, a record's field). The next frame evaluates with it;
    /// what the model copied from it at load (a layer's static field, a post
    /// pass's uniform, the motion blur) follows once the host reads the
    /// model again ([`Self::reread`]). What it reached, and why a
    /// reload is still needed when the load also computed with it or read
    /// it into a kernel. Shaders and kernels compiled from it are other
    /// sinks of the same site (`Cx::patch_literal` reaches them all).
    pub fn patch_literal(&self, file: &CodeFile, text: &str, literal: Range<usize>, value: Literal) -> makepad_script::literal::LiteralReach {
        patch_in_vm(&self.vm, file, text, literal, Some(value))
    }

    /// What a new value for the literal at `literal` (its bytes in `file`'s
    /// text `text`, the one the document was loaded from) would reach in
    /// the running document, without changing it: the places a patch sets,
    /// and why a reload is still needed where the load computed with it
    /// (the traced reach; an editor marks a value `RELOAD` by it before
    /// anything is dragged). `None` where `literal` is not one literal.
    pub fn literal_reach(&self, file: &CodeFile, text: &str, literal: Range<usize>) -> Option<makepad_script::literal::LiteralReach> {
        // The same walk as a patch, setting nothing: a value already
        // patched (dragged) stays what it is.
        makepad_script::literal::literal_value(text.get(literal.clone())?.trim_start_matches('-'))?;
        Some(patch_in_vm(&self.vm, file, text, literal, None))
    }

    /// Where the running document holds colour `was` as data: what its
    /// records, lists and `let`s copied while it loaded (`{sung: BONE}`
    /// holds BONE's colour). A recolour drag finds them once and sets them
    /// each step ([`Self::recolour_data`]); with [`Self::patch_literal`] on
    /// each place the colour is written, the next frame shows it without a
    /// load. A colour computed from it at load (mixed, split into
    /// channels) is another colour and is not among them.
    /// `field`: only records' fields of that name (a recolour of one
    /// record's field, `singing:`, not of every place the colour is).
    pub fn colour_data(&self, was: [f32; 4], field: Option<&str>) -> ColourData {
        let was = makepad_script::ScriptValue::from_color(color_u32(was));
        let key: Option<makepad_script::ScriptValue> = field.map(|f| makepad_script::LiveId::from_str(f).into());
        let mut places = self.vm.with(|evaluated| evaluated.with_vm(|vm| vm.bx.heap_ref().places_holding(was)));
        if let Some(key) = key {
            places.retain(|p| p.field_key() == Some(key));
        }
        places
    }

    /// Set the places [`Self::colour_data`] found that still hold `was` to
    /// `now`: how many changed.
    pub fn recolour_data(&self, places: &ColourData, was: [f32; 4], now: [f32; 4]) -> usize {
        let (was, now) = (makepad_script::ScriptValue::from_color(color_u32(was)), makepad_script::ScriptValue::from_color(color_u32(now)));
        self.vm.with(|evaluated| evaluated.with_vm(|vm| vm.bx.heap_mut().set_places(places, was, now)))
    }

    /// What the document's (and its modules') `let`s bound as it loaded:
    /// its constants and what they compute (a word's span, a beat's time).
    pub fn bound_at_load(&self) -> Vec<(makepad_script::ScriptIp, String, Watched)> {
        self.vm.with(|evaluated| evaluated.load_lets.clone())
    }

    /// Where each call site is in the code (the document's own lines, or a
    /// module's).
    /// Where character `char_index` of a drawn text came from, found by
    /// what built it, not by its words: row `row` of a list a top-level
    /// `let` holds (each of `lists`, tried in order: the chain the text's
    /// code reads through), a record; in it, a list of records whose words
    /// (their one text field), joined by spaces, run over the text; the
    /// record the character falls in. Its place is where that record was
    /// written (its construction site: the song's timing module's
    /// `{w: "see" start: …}`), and its words. An editor's document only
    /// (its records are kept from its load).
    pub fn text_origin(&self, lists: &[String], row: usize, char_index: usize) -> Option<(CodeAt, String, Option<f64>)> {
        let found = self.vm.with(|evaluated| {
            let tops = evaluated.tops.clone();
            evaluated.with_vm(|vm| {
                let heap = vm.heap();
                for name in lists {
                    let Some((_, value)) = tops.iter().find(|(n, _)| n == name) else { continue };
                    let Some(list) = value.as_array() else { continue };
                    if row >= heap.array_len(list) {
                        continue;
                    }
                    let item = heap.array_index_unchecked(list, row);
                    if let Some((record, words)) = record_of_char(heap, item, char_index) {
                        // A record made from another (`{word: w text: w.text}`):
                        // the one it was made from, where the words were
                        // first written.
                        let record = first_written(heap, record, &words);
                        let ip = heap.made_at(record);
                        if ip != makepad_script::ScriptIp::UNKNOWN {
                            // Its time (`start:`): a record copied field by
                            // field (`{text: w.w start: w.start}`) keeps it,
                            // and the host finds the first by it.
                            let start = fields_of(heap, record).into_iter().find(|(k, _)| k.as_id().is_some_and(|id| id == makepad_script::LiveId::from_str("start") || id == makepad_script::LiveId::from_str("t0"))).and_then(|(_, v)| v.as_number());
                            return Some((ip, words, start));
                        }
                    }
                }
                None
            })
        })?;
        let at = self.code_at(&[found.0]).into_iter().next().flatten()?;
        Some((at, found.1, found.2))
    }

    /// The records a top-level `let` record holds, field by field, as
    /// words with their time: `W.see` → ("see", the word "see", its start).
    /// An editor's document only (its records are kept from its load).
    pub fn record_words_of(&self, name: &str) -> Vec<(String, Option<String>, Option<f64>)> {
        self.vm.with(|evaluated| {
            let tops = evaluated.tops.clone();
            evaluated.with_vm(|vm| {
                let heap = vm.heap();
                let Some((_, value)) = tops.iter().find(|(n, _)| n.as_str() == name) else { return Vec::new() };
                let Some(record) = value.as_object() else { return Vec::new() };
                fields_of(heap, record).into_iter().filter_map(|(k, v)| {
                    let key = k.as_id()?.as_string(|s| s.map(str::to_string))?;
                    let inner = v.as_object();
                    let words = inner.and_then(|o| record_words(heap, o));
                    let start = inner.and_then(|o| fields_of(heap, o).into_iter().find(|(k, _)| k.as_id().is_some_and(|id| id == makepad_script::LiveId::from_str("start") || id == makepad_script::LiveId::from_str("t0"))).and_then(|(_, v)| v.as_number()));
                    Some((key, words, start))
                }).collect()
            })
        })
    }

    /// The post passes a generator in the document's `post` made (the whole
    /// `post` when it is one call, else the list's entry `entry`), written
    /// out as code: each kit or Pass with the fields it was given. A value
    /// that is a function is copied from where the document wrote it; one
    /// made inside the generator cannot be written here, and says so.
    pub fn post_expansion(&self, entry: Option<usize>) -> Result<Vec<String>, String> {
        let source: Arc<str> = self.source.text().into();
        // Functions' places first (the VM is borrowed once per call).
        let made = self.vm.with(|evaluated| {
            let root = self.run.root;
            evaluated.with_vm(|vm| -> Result<Vec<(String, Vec<(String, ScriptVal)>)>, String> {
                let heap = vm.heap();
                let root = root.as_object().ok_or("the document did not load")?;
                let post = fields_of(heap, root).into_iter().find(|(k, _)| k.as_id().is_some_and(|id| id == makepad_script::LiveId::from_str("post"))).map(|(_, v)| v).ok_or("the scene has no post")?;
                let list = match entry {
                    None => post,
                    Some(i) => {
                        let arr = post.as_array().ok_or("`post` is not a list")?;
                        if i >= heap.array_len(arr) {
                            return Err(format!("post has no entry {i}"));
                        }
                        heap.array_index_unchecked(arr, i)
                    }
                };
                fn flat(heap: &makepad_script::ScriptHeap, v: makepad_script::ScriptValue, out: &mut Vec<makepad_script::ScriptValue>) {
                    match v.as_array() {
                        Some(a) => {
                            for k in 0..heap.array_len(a) {
                                flat(heap, heap.array_index_unchecked(a, k), out);
                            }
                        }
                        None => out.push(v),
                    }
                }
                let mut items = Vec::new();
                flat(heap, list, &mut items);
                let mut out = Vec::new();
                for item in items {
                    let Some(kind) = makepad_render_graph::script::post_kind(vm, item) else { return Err("an entry it made is not a pass or a kit".into()) };
                    let ty = if kind == "pass" { "Pass".to_string() } else { makepad_render_graph::kits::kit(&kind).map(|k| k.name.to_string()).ok_or_else(|| format!("`{kind}` is no kit"))? };
                    let o = item.as_object().ok_or("an entry it made is not a record")?;
                    let fields = fields_of(heap, o).into_iter().filter_map(|(k, v)| {
                        let key = k.as_id()?.as_string(|s| s.map(str::to_string))?;
                        (!key.starts_with("__")).then(|| (key, script_val(heap, v)))
                    }).collect();
                    out.push((ty, fields));
                }
                Ok(out)
            })
        })?;
        let mut fn_ips = Vec::new();
        fn collect(v: &ScriptVal, out: &mut Vec<makepad_script::ScriptIp>) {
            match v {
                ScriptVal::Fn(ip) => out.push(*ip),
                ScriptVal::List(items) => items.iter().for_each(|i| collect(i, out)),
                ScriptVal::Record(fields) => fields.iter().for_each(|(_, i)| collect(i, out)),
                _ => {}
            }
        }
        for (_, fields) in &made {
            for (_, v) in fields {
                collect(v, &mut fn_ips);
            }
        }
        let places = self.code_at(&fn_ips);

        let fn_text: Vec<Option<String>> = places.into_iter().map(|at| {
            let at = at.filter(|a| a.file == CodeFile::Doc)?;
            let line_start: usize = source.split_inclusive('\n').take(at.line.saturating_sub(1) as usize).map(str::len).sum();
            let from = line_start + at.col.saturating_sub(1) as usize;
            fn_source(&source, from)
        }).collect();
        let mut k = 0;
        fn write(v: &ScriptVal, fns: &[Option<String>], k: &mut usize) -> Result<String, String> {
            Ok(match v {
                ScriptVal::Code(t) => t.clone(),
                ScriptVal::Fn(_) => {
                    let t = fns.get(*k).cloned().flatten();
                    *k += 1;
                    t.ok_or("made inside the generator, not written in the scene")?
                }
                ScriptVal::List(items) => format!("[{}]", items.iter().map(|i| write(i, fns, k)).collect::<Result<Vec<_>, _>>()?.join(", ")),
                ScriptVal::Record(fields) => format!("{{{}}}", fields.iter().map(|(n, i)| Ok(format!("{n}: {}", write(i, fns, k)?))).collect::<Result<Vec<String>, String>>()?.join(" ")),
            })
        }
        made.iter().map(|(ty, fields)| {
            let body = fields.iter().map(|(n, v)| Ok(format!("{n}: {}", write(v, &fn_text, &mut k).map_err(|e| format!("its {ty}'s `{n}`: {e}"))?))).collect::<Result<Vec<String>, String>>()?;
            // (Through its module: the text needs no `use` line.)
            Ok(format!("mod.post.{ty}{{{}}}", body.join(" ")))
        }).collect()
    }

    pub fn code_at(&self, ips: &[makepad_script::ScriptIp]) -> Vec<Option<CodeAt>> {
        self.vm.with(|evaluated| {
            let prefix = evaluated.map.clone();
            evaluated.with_vm(|vm| {
                ips.iter().map(|ip| {
                    let loc = vm.bx.code_ref().ip_to_loc(*ip)?;
                    let line_text = vm.bx.code_ref().ip_line_text(*ip);
                    if loc.file() == crate::value::DOC_FILE {
                        let at = prefix.map(loc.line(), loc.col());
                        // The first line carries the evaluator's prefix.
                        let line_text = line_text.map(|l| if loc.line() == 0 { l.chars().skip(prefix.prefix_cols as usize).collect() } else { l });
                        Some(CodeAt { file: CodeFile::Doc, line: at.line, col: at.col, line_text })
                    } else {
                        let key = loc.file().strip_prefix("module:")?.to_string();
                        Some(CodeAt { file: CodeFile::Module(key), line: loc.line(), col: loc.col() + 1, line_text })
                    }
                }).collect()
            })
        })
    }
}

/// A script value as code (what an expansion writes).
enum ScriptVal {
    Code(String),
    /// A function: where it was made.
    Fn(makepad_script::ScriptIp),
    List(Vec<ScriptVal>),
    Record(Vec<(String, ScriptVal)>),
}

fn script_val(heap: &makepad_script::ScriptHeap, v: makepad_script::ScriptValue) -> ScriptVal {
    if let Some(n) = v.as_number() {
        return ScriptVal::Code(crate::patch::fmt_num((n * 1e6).round() / 1e6));
    }
    if let Some(b) = v.as_bool() {
        return ScriptVal::Code(b.to_string());
    }
    if let Some(c) = v.as_color() {
        return ScriptVal::Code(format!("#{c:08x}"));
    }
    if let Some(t) = text_value(heap, v) {
        return ScriptVal::Code(crate::patch::fmt_text(&t));
    }
    if let Some(id) = v.as_id() {
        return ScriptVal::Code(format!("@{}", id.as_string(|s| s.map(str::to_string)).unwrap_or_default()));
    }
    if let Some(a) = v.as_array() {
        return ScriptVal::List((0..heap.array_len(a)).map(|k| script_val(heap, heap.array_index_unchecked(a, k))).collect());
    }
    if let Some(o) = v.as_object() {
        if heap.is_fn(o) {
            // Where its body is (the closure's own code).
            return ScriptVal::Fn(heap.as_fn(o).and_then(|f| f.script_ip()).unwrap_or_else(|| heap.made_at(o)));
        }
        return ScriptVal::Record(fields_of(heap, o).into_iter().filter_map(|(k, v)| Some((k.as_id()?.as_string(|s| s.map(str::to_string))?, script_val(heap, v)))).collect());
    }
    ScriptVal::Code("nil".into())
}

/// The text of a `fn(…) {…}` (or `fn(…) -> t {…}`) written at `from`.
fn fn_source(text: &str, from: usize) -> Option<String> {
    // The `fn` it starts with: at `from`, or just before (a body's place).
    let lo = from.saturating_sub(300);
    let hi = (from + 3).min(text.len());
    let window = text.get(lo..hi)?;
    let start = lo + window.rfind("fn(").or_else(|| window.rfind("fn "))?;
    let open = start + text[start..].find('{')?;
    let mut depth = 0i32;
    for (j, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..open + j + 1].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// A value's words, when it is text.
fn text_value(heap: &makepad_script::ScriptHeap, v: makepad_script::ScriptValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(heap.string(s).to_string());
    }
    v.as_inline_string(|s| s.to_string())
}

/// A record's fields (its map, then its list part), as key and value.
fn fields_of(heap: &makepad_script::ScriptHeap, o: makepad_script::ScriptObject) -> Vec<(makepad_script::ScriptValue, makepad_script::ScriptValue)> {
    let mut out: Vec<(makepad_script::ScriptValue, makepad_script::ScriptValue)> = heap.map_entries(o).iter().map(|e| (e.key, e.value)).collect();
    out.extend(heap.vec_ref(o).iter().map(|kv| (kv.key, kv.value)));
    out
}

/// A record's words: its one text field (`w:`, `text:`, `word:` first).
fn record_words(heap: &makepad_script::ScriptHeap, o: makepad_script::ScriptObject) -> Option<String> {
    let fields = fields_of(heap, o);
    let named = |k: &str| fields.iter().find(|(key, _)| key.as_id().is_some_and(|id| id == makepad_script::LiveId::from_str(k))).and_then(|(_, v)| text_value(heap, *v));
    named("w").or_else(|| named("text")).or_else(|| named("word")).or_else(|| fields.iter().find_map(|(_, v)| text_value(heap, *v)))
}

/// The record `record`'s words were first written in: a record it holds
/// (a field) with the same words, followed down while there is one.
fn first_written(heap: &makepad_script::ScriptHeap, record: makepad_script::ScriptObject, words: &str) -> makepad_script::ScriptObject {
    let mut at = record;
    for _ in 0..8 {
        let next = fields_of(heap, at).into_iter().filter_map(|(_, v)| v.as_object()).find(|o| *o != at && !heap.is_fn(*o) && record_words(heap, *o).is_some_and(|w| w == words));
        match next {
            Some(o) => at = o,
            None => break,
        }
    }
    at
}

/// The record a character of `item`'s text falls in: in a list of records
/// `item` holds whose words, joined by spaces, run over the text; else
/// `item` itself when it is a record with words.
fn record_of_char(heap: &makepad_script::ScriptHeap, item: makepad_script::ScriptValue, char_index: usize) -> Option<(makepad_script::ScriptObject, String)> {
    let record = item.as_object()?;
    for (_, v) in fields_of(heap, record) {
        let Some(list) = v.as_array() else { continue };
        let n = heap.array_len(list);
        let words: Vec<(makepad_script::ScriptObject, String)> = (0..n).filter_map(|i| {
            let o = heap.array_index_unchecked(list, i).as_object()?;
            Some((o, record_words(heap, o)?))
        }).collect();
        if words.is_empty() || words.len() != n {
            continue;
        }
        let mut at = 0usize;
        for (o, w) in &words {
            let len = w.chars().count();
            if char_index < at + len + 1 {
                return Some((*o, w.clone()));
            }
            at += len + 1;
        }
    }
    record_words(heap, record).map(|w| (record, w))
}

/// A source read as code: every literal written anywhere (in a layer, a
/// `let`, an object, a function body, a call's arguments) and the regions
/// that hold them (`let`s, functions, objects, the calls inside function
/// bodies), from Splash's own tokens. What the folded code editor shows a
/// control for, what a click on the picture jumps to, and what the
/// inspector lists for the region it lands in.
pub mod code {
    use super::ValueKind;
    use makepad_script::tokenizer::{lex, LexedToken, ScriptToken, ScriptTokenTag};
    use makepad_script::LiveId;
    use std::ops::Range;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RegionKind {
        /// `let name = …` at the top level.
        Let,
        /// `fn(…) {…}` (named by its `let`, its field or its own name).
        Fn,
        /// `Type{…}` or `{…}` written as a value.
        Object,
        /// `name(…)` inside a function body: a draw call, a helper.
        Call,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Region {
        pub kind: RegionKind,
        /// `prompt`, `draw_type`, `Text`, `c.text`, `note`.
        pub name: String,
        /// Its bytes, from its first token to its last.
        pub range: Range<usize>,
        pub parent: Option<usize>,
        pub depth: usize,
        /// Its own literals (not its sub-regions'), indices into `literals`.
        pub literals: Vec<usize>,
        pub children: Vec<usize>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Literal {
        pub range: Range<usize>,
        pub kind: ValueKind,
        /// What it is written as: its field (`size`), a call's argument
        /// (`note #1`), a `let`'s name.
        pub label: String,
        pub text: String,
        /// The innermost region holding it.
        pub region: Option<usize>,
        /// The call it is an argument of, and which (0-based): its name
        /// is the callee's parameter's.
        pub callee: Option<(String, usize)>,
        /// Which call that is (the literals of one call share it).
        pub call_id: usize,
        /// The field or `let` it is written under.
        pub key: Option<String>,
        /// Written in a loop's header (`for i in 0..24`): plumbing.
        pub in_loop: bool,
        /// An item of a list (a record of a table, a glyph of a run): its
        /// list is data, not a value to tweak on its own.
        pub in_list: bool,
        /// A list of numbers (`Data`): each item's bytes.
        pub elements: Vec<Range<usize>>,
    }

    impl Literal {
        /// A string literal's words.
        pub fn words(&self) -> Option<String> {
            match super::syntax::parse_value(&self.text).ok()?.kind {
                super::Kind::Str(text) => Some(text),
                _ => None,
            }
        }
    }

    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct CodeOutline {
        pub regions: Vec<Region>,
        pub literals: Vec<Literal>,
        /// Where time is written: every `keys(…)` and `tween(…)`.
        pub times: Vec<TimeSite>,
        /// The functions it defines and their parameters' names.
        pub fns: Vec<(String, Vec<String>)>,
    }

    /// A `keys([t, v, ease]…)` or `tween(from, to, t0, t1, ease)`: its
    /// bytes, and its keys' times and values as written (byte ranges; the
    /// time's number when it is one).
    #[derive(Clone, Debug, PartialEq)]
    pub struct TimeSite {
        pub range: Range<usize>,
        pub tween: bool,
        pub keys: Vec<TimeKey>,
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct TimeKey {
        pub t: Range<usize>,
        pub t_value: Option<f64>,
        pub value: Option<Range<usize>>,
    }

    impl CodeOutline {
        /// Name each call's arguments by the called function's parameters:
        /// `fns` (this code's and the modules' it imports), then Draw2D's
        /// methods; what is not found is named by position.
        pub fn name_args(&mut self, fns: &[(String, Vec<String>)]) {
            for literal in &mut self.literals {
                let Some((callee, arg)) = literal.callee.clone() else { continue };
                let short = callee.rsplit('.').next().unwrap_or(&callee);
                let param = fns.iter().rev().find(|(n, _)| n == short).and_then(|(_, p)| p.get(arg).cloned()).or_else(|| known_params(&callee).and_then(|p| p.get(arg).cloned()));
                // Unknown: no name (the call's name stands for it).
                literal.label = param.unwrap_or_default();
            }
        }

        /// The innermost region holding byte `at`.
        pub fn region_at(&self, at: usize) -> Option<usize> {
            (0..self.regions.len()).filter(|&i| self.regions[i].range.start <= at && at < self.regions[i].range.end).max_by_key(|&i| self.regions[i].depth)
        }

        /// [`Self::region_at`] of each byte, in one sweep: the regions by their start, the bytes in
        /// order, the deepest region open at a byte on top (the last of the deepest, as there).
        pub fn regions_at(&self, at: impl Iterator<Item = usize>) -> Vec<Option<usize>> {
            let at: Vec<usize> = at.collect();
            let mut order: Vec<usize> = (0..at.len()).collect();
            order.sort_by_key(|&k| at[k]);
            let mut starts: Vec<usize> = (0..self.regions.len()).collect();
            starts.sort_by_key(|&i| self.regions[i].range.start);
            let mut open = std::collections::BinaryHeap::new();
            let mut next = 0;
            let mut found = vec![None; at.len()];
            for k in order {
                while next < starts.len() && self.regions[starts[next]].range.start <= at[k] {
                    open.push((self.regions[starts[next]].depth, starts[next]));
                    next += 1;
                }
                // (a region ended before this byte has ended for every later one)
                while open.peek().is_some_and(|&(_, i)| self.regions[i].range.end <= at[k]) {
                    open.pop();
                }
                found[k] = open.peek().map(|&(_, i)| i);
            }
            found
        }

        /// The literal holding byte `at`.
        pub fn literal_at(&self, at: usize) -> Option<usize> {
            self.literals.iter().position(|l| l.range.start <= at && at <= l.range.end)
        }

        /// Every literal inside region `index`, its sub-regions' too.
        pub fn literals_in(&self, index: usize) -> Vec<usize> {
            let range = self.regions[index].range.clone();
            (0..self.literals.len()).filter(|&i| self.literals[i].range.start >= range.start && self.literals[i].range.end <= range.end).collect()
        }

        /// The top-level regions, in source order.
        pub fn roots(&self) -> Vec<usize> {
            (0..self.regions.len()).filter(|&i| self.regions[i].parent.is_none()).collect()
        }
    }

    /// Calls that build a value rather than do something: not regions.
    const VALUES: &[&str] = &["vec2", "vec3", "vec4", "rgb", "rgba", "hsl", "in", "beat", "beats", "bar", "bars", "tween", "keys", "spring", "cubic", "bezier", "there_and_back", "there_and_back_with_pause", "running_start", "wiggle", "squish", "not_quite_there", "lingering", "exponential_decay", "double_smooth", "lagged", "then", "asset", "import", "input", "min", "max", "abs", "floor", "ceil", "round", "sqrt", "sin", "cos", "clamp", "mix", "smoothstep", "fract", "pow", "exp", "length", "normalize", "dot", "atan2", "sign"];
    const KEYWORDS: &[&str] = &["if", "else", "for", "while", "loop", "match", "return", "let", "var", "fn", "in", "use", "true", "false", "nil", "break", "continue"];

    fn name(id: LiveId) -> String {
        id.as_string(|s| s.map(str::to_string)).unwrap_or_default()
    }

    /// The names a source uses as times: compared with or subtracted from
    /// the clock (`t - O_T0`, `T >= W_I.x`), or handed to what takes times
    /// (`tween`, `keys`, `at`, `span`, `between`, …).
    pub fn time_names(source: &str) -> std::collections::HashSet<String> {
        const CLOCKS: &[&str] = &["t", "T", "time", "now", "song_t", "local_t", "lt", "tt"];
        const TAKES_TIME: &[&str] = &["tween", "keys", "at", "span", "spans", "between", "window", "after", "before", "ramp", "fade", "fade_in", "fade_out", "env", "pulse"];
        const COMPARE: &[&str] = &["-", "<", ">", "<=", ">=", "=="];
        let tokens = lex(source);
        let mut out = std::collections::HashSet::new();
        let id = |i: usize| -> Option<String> {
            tokens.get(i).and_then(|t| t.token().as_identifier()).map(name)
        };
        let op = |i: usize| tokens.get(i).is_some_and(|t| COMPARE.iter().any(|o| op_is(t.token(), o)));
        // The calls open around each token: (name, its closing round).
        let mut calls: Vec<(String, usize)> = Vec::new();
        let mut depth = 0usize;
        for i in 0..tokens.len() {
            match tokens[i].token().kind() {
                ScriptTokenTag::OpenRound => {
                    depth += 1;
                    let callee = if i > 0 { id(i - 1) } else { None };
                    calls.push((callee.unwrap_or_default(), depth));
                }
                ScriptTokenTag::CloseRound => {
                    calls.retain(|(_, d)| *d < depth);
                    depth = depth.saturating_sub(1);
                }
                ScriptTokenTag::Identifier => {
                    let Some(x) = id(i) else { continue };
                    if CLOCKS.contains(&x.as_str()) || matches!((tokens.get(i + 1).map(|t| t.token())).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) {
                        continue;
                    }
                    // `t - O_T0`, `T >= W_I.x`, `t - O_B[i]`, `O_T1 > t`.
                    let near_clock = (i >= 2 && op(i - 1) && id(i - 2).is_some_and(|c| CLOCKS.contains(&c.as_str())))
                        || (op(i + 1) && id(i + 2).is_some_and(|c| CLOCKS.contains(&c.as_str())));
                    let given = calls.last().is_some_and(|(c, _)| TAKES_TIME.contains(&c.rsplit('.').next().unwrap_or(c)));
                    if near_clock || given {
                        out.insert(x);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// How a source indexes its lists: by the clock (`TABLE[floor(t * 60)]`:
    /// a value per frame, so many per second, when written), or by a sung
    /// word or letter (`W[wi]`, `GLYPHS[ch]`). Name → what it is indexed by.
    pub fn list_indexing(source: &str) -> std::collections::HashMap<String, ListIndex> {
        const CLOCKS: &[&str] = &["t", "T", "time", "now", "song_t", "local_t", "lt", "tt"];
        const WORDS: &[&str] = &["w", "wi", "word", "words", "word_i", "wordi"];
        const LETTERS: &[&str] = &["ch", "ci", "c_i", "letter", "glyph", "gi", "char"];
        let tokens = lex(source);
        let mut out = std::collections::HashMap::new();
        let id = |i: usize| -> Option<String> {
            tokens.get(i).and_then(|t| t.token().as_identifier()).map(name)
        };
        for i in 0..tokens.len() {
            let Some(list) = id(i) else { continue };
            if !matches!((tokens.get(i + 1).map(|t| t.token())).map(|t| t.kind()), Some(ScriptTokenTag::OpenSquare)) {
                continue;
            }
            // The index: up to the matching `]`.
            let mut depth = 0;
            let mut j = i + 1;
            let mut inner = Vec::new();
            while j < tokens.len() {
                match tokens[j].token().kind() {
                    ScriptTokenTag::OpenSquare => depth += 1,
                    ScriptTokenTag::CloseSquare => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                if j > i + 1 {
                    inner.push(j);
                }
                j += 1;
            }
            let names: Vec<String> = inner.iter().filter_map(|&k| id(k)).collect();
            let kind = if let Some(k) = inner.iter().position(|&k| id(k).is_some_and(|n| CLOCKS.contains(&n.as_str()))) {
                // `t * 60` or `60 * t`: so many per second.
                let at = inner[k];
                let rate = [at + 2, at.wrapping_sub(2)].into_iter().find_map(|o| {
                    let between = tokens.get(at.min(o) + 1).is_some_and(|t| op_is(t.token(), "*"));
                    let number = tokens.get(o).and_then(|t| match t.token() {
                        tok if is_number(tok) => source[t.span().clone()].parse::<f64>().ok(),
                        _ => None,
                    });
                    number.filter(|_| between)
                });
                ListIndex::Time { per_second: rate }
            } else if names.iter().any(|n| WORDS.contains(&n.as_str())) {
                ListIndex::Words
            } else if names.iter().any(|n| LETTERS.contains(&n.as_str())) {
                ListIndex::Letters
            } else {
                continue;
            };
            out.entry(list).or_insert(kind);
        }
        out
    }

    /// What a list is indexed by ([`list_indexing`]).
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum ListIndex {
        Time { per_second: Option<f64> },
        Words,
        Letters,
    }

    /// Which regions of `code` matter in time (its dataflow): a region whose
    /// value changes with the film (it reads the clock, or a name that
    /// does, directly or through other `let`s), holds a value measured to
    /// change (`varying`: bytes of `let`s whose values differed between
    /// sampled moments), or is named in `seeds` (times the code writes or
    /// computes). What is constant (palettes, static geometry, helpers
    /// whose output does not move, setup) is not. A region that holds a
    /// relevant one is relevant (the chain to what draws).
    pub fn time_relevant(code: &CodeOutline, source: &str, seeds: &std::collections::HashSet<String>, varying: &[usize]) -> Vec<bool> {
        const CLOCKS: &[&str] = &["t", "T", "time", "now", "song_t", "local_t", "lt", "tt", "playhead"];
        let tokens = lex(source);
        // The names a region reads: not a field's key (`x:`), not a field
        // read off something (`b.x`).
        let reads_of = |range: &Range<usize>| -> std::collections::HashSet<String> {
            let a = tokens.partition_point(|t| t.span().start < range.start);
            let b = tokens.partition_point(|t| t.span().start < range.end);
            (a..b).filter_map(|i| {
                let Some(id) = tokens[i].token().as_identifier() else { return None };
                let key = tokens.get(i + 1).is_some_and(|n| op_is(n.token(), ":"));
                let field = i > 0 && op_is(tokens[i - 1].token(), ".");
                (!key && !field).then(|| name(id))
            }).collect()
        };
        let reads: Vec<std::collections::HashSet<String>> = code.regions.iter().map(|r| reads_of(&r.range)).collect();
        let in_fn = |i: usize| -> Option<usize> {
            let mut p = code.regions[i].parent;
            while let Some(x) = p {
                if code.regions[x].kind == RegionKind::Fn {
                    return Some(x);
                }
                p = code.regions[x].parent;
            }
            None
        };
        let scope: Vec<Option<usize>> = (0..code.regions.len()).map(in_fn).collect();
        let lets: Vec<usize> = (0..code.regions.len()).filter(|&i| code.regions[i].kind == RegionKind::Let).collect();
        // What moves, by name: the clock, the times named, what was measured
        // to change; a top-level name is seen everywhere, a function's own
        // only in that function.
        let mut live_global: std::collections::HashSet<String> = CLOCKS.iter().map(|s| s.to_string()).collect();
        live_global.extend(seeds.iter().cloned());
        let mut live_local: std::collections::HashSet<(usize, String)> = Default::default();
        let mut moving = vec![false; code.regions.len()];
        for &i in &lets {
            if varying.iter().any(|b| code.regions[i].range.contains(b)) {
                moving[i] = true;
            }
        }
        for _ in 0..16 {
            let mut grew = false;
            for &i in &lets {
                let r = &code.regions[i];
                let f = scope[i];
                let reads_live = reads[i].iter().any(|n| n != &r.name && (live_global.contains(n) || f.is_some_and(|f| live_local.contains(&(f, n.clone())))));
                if (reads_live || moving[i] || (f.is_none() && live_global.contains(&r.name))) && !moving[i] {
                    moving[i] = true;
                    grew = true;
                }
                if moving[i] {
                    let fresh = match f {
                        None => live_global.insert(r.name.clone()),
                        Some(f) => live_local.insert((f, r.name.clone())),
                    };
                    grew |= fresh;
                }
            }
            if !grew {
                break;
            }
        }
        let mut out: Vec<bool> = (0..code.regions.len()).map(|i| {
            let r = &code.regions[i];
            if r.kind == RegionKind::Let {
                return moving[i];
            }
            let f = if r.kind == RegionKind::Fn { Some(i) } else { scope[i] };
            reads[i].iter().any(|n| live_global.contains(n) || f.is_some_and(|f| live_local.contains(&(f, n.clone()))))
                || varying.iter().any(|b| r.range.contains(b))
                || code.times.iter().any(|t| r.range.start <= t.range.start && t.range.end <= r.range.end)
        }).collect();
        // A holder of a relevant region is relevant.
        for i in (0..out.len()).rev() {
            if out[i] {
                let mut p = code.regions[i].parent;
                while let Some(x) = p {
                    out[x] = true;
                    p = code.regions[x].parent;
                }
            }
        }
        out
    }

    /// What fed a call: one place in the code (a `let` or the call itself)
    /// that the call's arguments read, directly or through other names.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Gathered {
        /// The `let`'s (or the call's) bytes.
        pub range: Range<usize>,
        /// Its name (the call's callee).
        pub name: String,
        /// How many names away from the call (0: the call).
        pub depth: usize,
    }

    /// The code that fed the call at byte `at`: the call, then the `let`s
    /// its arguments read, and the `let`s those read, nearest first (a
    /// function's own `let`s before the top level's; at most `limit`).
    /// Names a function takes as parameters stop the walk here: their
    /// values come from its caller's call.
    pub fn gather(code: &CodeOutline, source: &str, at: usize, limit: usize) -> Vec<Gathered> {
        let Some(call) = (0..code.regions.len()).filter(|&i| code.regions[i].range.start <= at && at < code.regions[i].range.end && code.regions[i].kind == RegionKind::Call).max_by_key(|&i| code.regions[i].depth) else { return Vec::new() };
        let mut out = vec![Gathered { range: code.regions[call].range.clone(), name: code.regions[call].name.clone(), depth: 0 }];
        out.extend(gather_from(code, source, call, &code.regions[call].range.clone(), limit.saturating_sub(1)));
        out
    }

    /// The bytes of argument `index` of the call at byte `at` (its
    /// innermost call), as written between its brackets.
    pub fn call_argument(code: &CodeOutline, source: &str, at: usize, index: usize) -> Option<Range<usize>> {
        let call = (0..code.regions.len()).filter(|&i| code.regions[i].range.start <= at && at < code.regions[i].range.end && code.regions[i].kind == RegionKind::Call).max_by_key(|&i| code.regions[i].depth)?;
        let r = code.regions[call].range.clone();
        let text = source.get(r.clone())?;
        let open = text.find('(')?;
        let bytes = text.as_bytes();
        let (mut depth, mut start, mut k) = (0usize, open + 1, 0usize);
        let mut i = open + 1;
        while i < bytes.len() {
            if let Some(next) = crate::value::skip_literal(bytes, i) {
                i = next;
                continue;
            }
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth == 0 => {
                    return (k == index).then(|| r.start + start..r.start + i);
                }
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    if k == index {
                        return Some(r.start + start..r.start + i);
                    }
                    k += 1;
                    start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// A property a call is given: an argument (named by its parameter)
    /// or a field of its options record (`{px: 100 color: col}`), and the
    /// bytes of what it is given.
    #[derive(Clone, Debug, PartialEq)]
    pub struct CallProperty {
        pub name: String,
        pub expr: Range<usize>,
    }

    /// The properties of the call at byte `at`: its arguments by their
    /// parameters' names (the function's own, or a builtin's), and the
    /// fields of a record argument.
    pub fn call_properties(code: &CodeOutline, source: &str, at: usize) -> Vec<CallProperty> {
        let Some(call) = (0..code.regions.len()).filter(|&i| code.regions[i].range.start <= at && at < code.regions[i].range.end && code.regions[i].kind == RegionKind::Call).max_by_key(|&i| code.regions[i].depth) else { return Vec::new() };
        let callee = code.regions[call].name.clone();
        let short = callee.rsplit('.').next().unwrap_or(&callee).to_string();
        let params = code.fns.iter().rev().find(|(n, _)| *n == short).map(|(_, p)| p.clone()).or_else(|| known_params(&callee)).unwrap_or_default();
        let tokens = lex(source);
        let mut out = Vec::new();
        for k in 0..32 {
            let Some(arg) = call_argument(code, source, at, k) else { break };
            let text = &source[arg.clone()];
            let lead = text.len() - text.trim_start().len();
            let trimmed = arg.start + lead..arg.end - (text.len() - text.trim_end().len());
            if text.trim_start().starts_with('{') {
                // A record: each `name:` at its top level, to the next.
                let a = tokens.partition_point(|t| t.span().start < trimmed.start + 1);
                let b = tokens.partition_point(|t| t.span().start < trimmed.end.saturating_sub(1));
                let mut depth = 0i32;
                let mut fields: Vec<(String, usize)> = Vec::new();
                for i in a..b {
                    match tokens[i].token().kind() {
                        ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare => depth += 1,
                        ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseSquare => depth -= 1,
                        ScriptTokenTag::Identifier if depth == 0 && tokens.get(i + 1).is_some_and(|n| op_is(n.token(), ":")) => {
                            let after = tokens.get(i + 2).map_or(tokens[i].span().end, |n| n.span().start);
                            fields.push((name(tokens[i].token().identifier()), after));
                        }
                        _ => {}
                    }
                }
                for (j, (field, start)) in fields.iter().enumerate() {
                    // To the next field's name (its `name:` begins it).
                    let end = fields.get(j + 1).map_or(trimmed.end - 1, |(next, s2)| {
                        let before = &source[..*s2];
                        before.rfind(next.as_str()).filter(|p| *p >= *start).unwrap_or(*s2)
                    });
                    let t = &source[*start..end];
                    out.push(CallProperty { name: field.clone(), expr: *start..*start + t.trim_end().trim_end_matches(',').trim_end().len() });
                }
            } else {
                let name = params.get(k).cloned().unwrap_or_else(|| format!("#{}", k + 1));
                out.push(CallProperty { name, expr: trimmed });
            }
        }
        out
    }

    /// The one literal a property's value is written as, if it is one:
    /// the expression itself, or the `let` it names whose whole value is
    /// a literal (an index into `literals`).
    pub fn editable_literal(code: &CodeOutline, source: &str, within: usize, expr: &Range<usize>) -> Option<usize> {
        if let Some(i) = code.literals.iter().position(|l| l.range == *expr) {
            return Some(i);
        }
        let text = source.get(expr.clone())?.trim();
        if !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || text.is_empty() {
            return None;
        }
        let g = gather_from(code, source, within, expr, 1).into_iter().next().filter(|g| g.name == text)?;
        // `let NAME = <literal>`: the value after `=` is just the literal.
        let body = source.get(g.range.clone())?;
        let eq = body.find('=')?;
        let value = body[eq + 1..].trim();
        code.literals.iter().position(|l| g.range.start <= l.range.start && l.range.end <= g.range.end && l.text.trim() == value)
    }

    /// The `let`s the code in `range` (inside region `within`'s scope)
    /// reads, and the `let`s those read, nearest first (at most `limit`).
    pub fn gather_from(code: &CodeOutline, source: &str, within: usize, range: &Range<usize>, limit: usize) -> Vec<Gathered> {
        let tokens = lex(source);
        let reads_of = |range: &Range<usize>| -> Vec<String> {
            let a = tokens.partition_point(|t| t.span().start < range.start);
            let b = tokens.partition_point(|t| t.span().start < range.end);
            let mut out: Vec<String> = Vec::new();
            for i in a..b {
                let Some(id) = tokens[i].token().as_identifier() else { continue };
                let key = tokens.get(i + 1).is_some_and(|n| op_is(n.token(), ":"));
                let field = i > 0 && op_is(tokens[i - 1].token(), ".");
                let called = tokens.get(i + 1).is_some_and(|n| op_is(n.token(), "("));
                let n = name(id);
                if !key && !field && !called && !out.contains(&n) {
                    out.push(n);
                }
            }
            out
        };
        let in_fn = |i: usize| -> Option<usize> {
            let mut p = code.regions[i].parent;
            while let Some(x) = p {
                if code.regions[x].kind == RegionKind::Fn {
                    return Some(x);
                }
                p = code.regions[x].parent;
            }
            None
        };
        let resolve = |n: &str, f: Option<usize>, before: usize| -> Option<usize> {
            let lets = (0..code.regions.len()).filter(|&i| code.regions[i].kind == RegionKind::Let && code.regions[i].name == n);
            let mut own: Option<usize> = None;
            let mut top: Option<usize> = None;
            for i in lets {
                let scope = in_fn(i);
                if f.is_some() && scope == f && code.regions[i].range.start < before {
                    if own.is_none_or(|o| code.regions[i].range.start > code.regions[o].range.start) {
                        own = Some(i);
                    }
                } else if scope.is_none() && code.regions[i].parent.is_none() {
                    top = top.or(Some(i));
                }
            }
            own.or(top)
        };
        let scope_of = |i: usize| if code.regions[i].kind == RegionKind::Fn { Some(i) } else { in_fn(i) };
        let mut out: Vec<Gathered> = Vec::new();
        let mut seen: std::collections::HashSet<usize> = Default::default();
        let mut todo: std::collections::VecDeque<(String, Option<usize>, usize, usize)> = reads_of(range).into_iter().map(|n| (n, scope_of(within), range.start, 1)).collect();
        while let Some((n, f, before, depth)) = todo.pop_front() {
            if out.len() >= limit {
                break;
            }
            let Some(i) = resolve(&n, f, before) else { continue };
            if !seen.insert(i) {
                continue;
            }
            let r = &code.regions[i];
            // A function itself (a helper) is not data that fed it.
            if code.regions.iter().any(|c| c.parent == Some(i) && c.kind == RegionKind::Fn && c.range.start <= r.range.start + r.name.len() + 8) {
                continue;
            }
            out.push(Gathered { range: r.range.clone(), name: r.name.clone(), depth });
            for m in reads_of(&r.range) {
                if m != r.name {
                    todo.push_back((m, in_fn(i), r.range.start, depth + 1));
                }
            }
        }
        out
    }

    /// Whether a name reads as a time: `O_T0`, `T1`, `start`, `intro_at`.
    pub fn time_named(name: &str) -> bool {
        // `O_tCirc`, `tNerv`: a `t` before a capital reads as "time of".
        let camel = name.split('_').any(|p| {
            let mut c = p.chars();
            c.next() == Some('t') && c.next().is_some_and(|n| n.is_ascii_uppercase())
        });
        camel || name.split('_').filter(|p| !p.is_empty()).any(|part| {
            let lower = part.to_ascii_lowercase();
            (lower.starts_with('t') && lower.len() <= 3 && lower[1..].chars().all(|c| c.is_ascii_digit()))
                || matches!(lower.as_str(), "time" | "start" | "end" | "at" | "beat" | "beats" | "dur" | "delay" | "onset" | "until" | "from" | "span" | "word" | "line" | "phrase" | "verse" | "chorus" | "section" | "intro" | "outro" | "drop" | "bar")
        })
    }

    fn is_number(t: &ScriptToken) -> bool {
        matches!((t).kind(), ScriptTokenTag::F32 | ScriptTokenTag::F64 | ScriptTokenTag::U32 | ScriptTokenTag::I32 | ScriptTokenTag::F16 | ScriptTokenTag::U40)
    }

    fn op_is(t: &ScriptToken, s: &str) -> bool {
        t.is_operator() && name(t.operator()) == s
    }

    /// A frame of open brackets while reading.
    struct Frame {
        close: usize,
        region: Option<usize>,
        /// The field being written (`key:`), the call whose arguments these
        /// are and the argument's index.
        key: Option<String>,
        call: Option<String>,
        arg: usize,
        /// A list's `[…]`, and how many lists it holds so far.
        square: bool,
        lists: usize,
    }

    /// Read `source` as code.
    pub fn outline(source: &str) -> CodeOutline {
        outline_lexed(source, &lex(source))
    }

    /// [`outline`] of `source` lexed as `tokens` (a caller that has them already).
    pub fn outline_lexed(source: &str, tokens: &[LexedToken]) -> CodeOutline {
        let mut out = outline_unnamed(source, tokens);
        let fns = out.fns.clone();
        out.name_args(&fns);
        out
    }

    /// The outline, its call arguments named by position only.
    fn outline_unnamed(source: &str, tokens: &[LexedToken]) -> CodeOutline {
        let mut out = CodeOutline::default();
        IN_LOOP.with(|f| f.set(false));
        // Matching brackets, by token index.
        let mut closes = vec![usize::MAX; tokens.len()];
        let mut open = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            match t.token().kind() {
                ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare => open.push(i),
                ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseSquare => {
                    if let Some(o) = open.pop() {
                        closes[o] = i;
                    }
                }
                _ => {}
            }
        }
        let ident = |i: usize| -> Option<String> {
            tokens.get(i).and_then(|t| t.token().as_identifier()).map(name)
        };
        let mut stack: Vec<Frame> = Vec::new();
        // The top-level `let` being read: its region.
        let mut top_let: Option<usize> = None;
        // The name the next fn or object takes (a `let`'s, a field's).
        let mut pending_name: Option<String> = None;
        // A `fn` read, waiting for its body: (start byte, name, depth).
        let mut pending_fn: Option<(usize, String, usize)> = None;
        // The `let`s open inside function bodies: (region, frames open at it).
        let mut local: Vec<(usize, usize)> = Vec::new();
        let new_region = |out: &mut CodeOutline, stack: &Vec<Frame>, local: &[(usize, usize)], top_let: Option<usize>, kind: RegionKind, name: String, range: Range<usize>| -> usize {
            // A local `let` holds what is written in it (in its own frame).
            let inner = local.last().filter(|(_, depth)| *depth == stack.len()).map(|(r, _)| *r);
            let parent = inner.or_else(|| stack.iter().rev().find_map(|f| f.region)).or(top_let);
            let depth = parent.map_or(0, |p| out.regions[p].depth + 1);
            let index = out.regions.len();
            out.regions.push(Region { kind, name, range, parent, depth, literals: Vec::new(), children: Vec::new() });
            if let Some(p) = parent {
                out.regions[p].children.push(index);
            }
            index
        };
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            let in_fn = stack.iter().any(|f| f.region.is_some_and(|r| out.regions[r].kind == RegionKind::Fn));
            // A local `let` ends where the next statement of its block
            // starts, or with its block.
            while let Some(&(r, depth)) = local.last() {
                let ends = if stack.len() < depth {
                    true
                } else if stack.len() == depth && t.preceded_by_newline() && i > 0 {
                    let prev = tokens[i - 1].token();
                    !(matches!((t.token()).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator) || matches!((prev).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator))
                } else {
                    false
                };
                if !ends {
                    break;
                }
                out.regions[r].range.end = tokens[i - 1].span().end;
                local.pop();
                // What follows is not written under its name.
                if let Some(f) = stack.get_mut(depth.saturating_sub(1)) {
                    f.key = None;
                }
            }
            // A top-level `let` ends where the next top-level statement starts.
            if stack.is_empty() && t.preceded_by_newline() && i > 0 {
                let prev = tokens[i - 1].token();
                let continues = matches!((t.token()).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator) || matches!((prev).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator);
                if !continues {
                    if let Some(r) = top_let.take() {
                        out.regions[r].range.end = tokens[i - 1].span().end;
                    }
                }
            }
            match t.token().kind() {
                ScriptTokenTag::Identifier => {
                    let word = name(t.token().identifier());
                    let next = tokens.get(i + 1).map(|t| t.token());
                    if word == "let" || word == "var" {
                        if let Some(n) = ident(i + 1) {
                            if stack.is_empty() {
                                if let Some(r) = top_let.take() {
                                    out.regions[r].range.end = tokens[i - 1].span().end;
                                }
                                top_let = Some(new_region(&mut out, &stack, &[], None, RegionKind::Let, n.clone(), t.span().start..source.len()));
                            } else if in_fn {
                                // Inside a function: a named value of its own.
                                let r = new_region(&mut out, &stack, &local, top_let, RegionKind::Let, n.clone(), t.span().start..source.len());
                                local.push((r, stack.len()));
                            }
                            pending_name = Some(n.clone());
                            if let Some(f) = stack.last_mut() {
                                f.key = Some(n);
                            }
                            i += 2;
                            continue;
                        }
                    }
                    if word == "for" {
                        IN_LOOP.with(|f| f.set(true));
                    }
                    if word == "fn" {
                        let fn_name = ident(i + 1).or_else(|| pending_name.clone()).unwrap_or_else(|| "fn".into());
                        // Its parameters: the names in its `(…)`.
                        let open_at = if ident(i + 1).is_some() { i + 2 } else { i + 1 };
                        if matches!((tokens.get(open_at).map(|t| t.token())).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) && closes[open_at] != usize::MAX {
                            let mut params = Vec::new();
                            let mut expect = true;
                            for t in &tokens[open_at + 1..closes[open_at]] {
                                match t.token().kind() {
                                    ScriptTokenTag::Identifier if expect => {
                                        params.push(name(t.token().identifier()));
                                        expect = false;
                                    }
                                    ScriptTokenTag::Separator => expect = true,
                                    _ => {}
                                }
                            }
                            out.fns.push((fn_name.clone(), params));
                        }
                        pending_fn = Some((t.span().start, fn_name, stack.len()));
                        i += 1;
                        continue;
                    }
                    if word == "true" || word == "false" {
                        let label = label_of(&stack, &pending_name);
                        push_literal(&mut out, &stack, top_let, t.span().clone(), ValueKind::Bool, label, source);
                        i += 1;
                        continue;
                    }
                    // `key: value`: the field being written.
                    if next.is_some_and(|n| op_is(n, ":") || op_is(n, ":=") || op_is(n, "+:")) {
                        pending_name = Some(word.clone());
                        if let Some(f) = stack.last_mut() {
                            f.key = Some(word);
                        }
                        i += 2;
                        continue;
                    }
                    // Time written down: the keys of keys(…) / tween(…).
                    if (word == "keys" || word == "tween") && matches!((next).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) && closes[i + 1] != usize::MAX {
                        let close = closes[i + 1];
                        // The top-level arguments' token ranges.
                        let mut args: Vec<(usize, usize)> = Vec::new();
                        let mut start = i + 2;
                        let mut k = i + 2;
                        while k < close {
                            match tokens[k].token().kind() {
                                ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare if closes[k] != usize::MAX => k = closes[k],
                                ScriptTokenTag::Separator => {
                                    args.push((start, k));
                                    start = k + 1;
                                }
                                _ => {}
                            }
                            k += 1;
                        }
                        if start < close {
                            args.push((start, close));
                        }
                        let span = |a: (usize, usize)| -> Option<Range<usize>> { (a.0 < a.1).then(|| tokens[a.0].span().start..tokens[a.1 - 1].span().end) };
                        let number = |r: &Range<usize>| source[r.clone()].trim().parse::<f64>().ok();
                        let mut keys = Vec::new();
                        if word == "tween" && args.len() >= 4 {
                            for (t, v) in [(args[2], args[0]), (args[3], args[1])] {
                                if let Some(tr) = span(t) {
                                    keys.push(TimeKey { t_value: number(&tr), t: tr, value: span(v) });
                                }
                            }
                        } else if word == "keys" {
                            for a in &args {
                                if matches!((tokens[a.0].token()).kind(), ScriptTokenTag::OpenSquare) && closes[a.0] != usize::MAX {
                                    let inner_close = closes[a.0];
                                    let mut parts = Vec::new();
                                    let mut ps = a.0 + 1;
                                    let mut q = a.0 + 1;
                                    while q < inner_close {
                                        match tokens[q].token().kind() {
                                            ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare if closes[q] != usize::MAX => q = closes[q],
                                            ScriptTokenTag::Separator => {
                                                parts.push((ps, q));
                                                ps = q + 1;
                                            }
                                            _ => {}
                                        }
                                        q += 1;
                                    }
                                    if ps < inner_close {
                                        parts.push((ps, inner_close));
                                    }
                                    if let Some(tr) = parts.first().and_then(|p| span(*p)) {
                                        keys.push(TimeKey { t_value: number(&tr), t: tr, value: parts.get(1).and_then(|p| span(*p)) });
                                    }
                                }
                            }
                        }
                        out.times.push(TimeSite { range: t.span().start..tokens[close].span().end, tween: word == "tween", keys });
                    }
                    // vecN(numbers): one literal.
                    if matches!(word.as_str(), "vec2" | "vec3" | "vec4") && matches!((next).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) {
                        let close = closes[i + 1];
                        if close != usize::MAX {
                            let inner = &tokens[i + 2..close];
                            let numeric = inner.iter().all(|t| is_number(t.token()) || matches!((t.token()).kind(), ScriptTokenTag::Separator) || op_is(t.token(), "-"));
                            let dims = inner.iter().filter(|t| is_number(t.token())).count();
                            if numeric && dims >= 2 {
                                let label = label_of(&stack, &pending_name);
                                push_literal(&mut out, &stack, top_let, t.span().start..tokens[close].span().end, ValueKind::Number { dims }, label, source);
                                i = close + 1;
                                continue;
                            }
                        }
                    }
                    // A call inside a function body: a region (a dotted
                    // chain names it: `c.text`, `open.draw_type`).
                    if matches!((next).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) && in_fn && !VALUES.contains(&word.as_str()) && !KEYWORDS.contains(&word.as_str()) {
                        let mut start = i;
                        let mut chain = word.clone();
                        while start >= 2 && op_is(tokens[start - 1].token(), ".") {
                            match ident(start - 2) {
                                Some(head) => {
                                    chain = format!("{head}.{chain}");
                                    start -= 2;
                                }
                                None => break,
                            }
                        }
                        let close = closes[i + 1];
                        if close != usize::MAX {
                            let region = new_region(&mut out, &stack, &local, top_let, RegionKind::Call, chain.clone(), tokens[start].span().start..tokens[close].span().end);
                            stack.push(Frame { close, region: Some(region), key: None, call: Some(chain), arg: 0, square: false, lists: 0 });
                            i += 2;
                            continue;
                        }
                    }
                    // Any other call: its arguments are labelled by it.
                    if matches!((next).map(|t| t.kind()), Some(ScriptTokenTag::OpenRound)) {
                        let close = closes[i + 1];
                        if close != usize::MAX {
                            stack.push(Frame { close, region: None, key: None, call: Some(word), arg: 0, square: false, lists: 0 });
                            i += 2;
                            continue;
                        }
                    }
                }
                ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenRound => {
                    let close = closes[i];
                    if matches!((t.token()).kind(), ScriptTokenTag::OpenCurly) {
                        IN_LOOP.with(|f| f.set(false));
                    }
                    // A list of numbers (more than a vector's few): one data
                    // value, each item's bytes kept.
                    if matches!((t.token()).kind(), ScriptTokenTag::OpenSquare) && close != usize::MAX {
                        let inner = &tokens[i + 1..close];
                        let mut elements: Vec<Range<usize>> = Vec::new();
                        let mut plain = true;
                        let mut k = 0;
                        while k < inner.len() {
                            let tk = &inner[k];
                            if op_is(tk.token(), "-") && inner.get(k + 1).is_some_and(|n| is_number(n.token())) {
                                elements.push(tk.span().start..inner[k + 1].span().end);
                                k += 2;
                                continue;
                            }
                            match tk.token().kind() {
                                _ if is_number(tk.token()) => elements.push(tk.span().clone()),
                                ScriptTokenTag::Separator => {}
                                _ => {
                                    plain = false;
                                    break;
                                }
                            }
                            k += 1;
                        }
                        // In a list of lists, short rows are data too.
                        let row = stack.last().filter(|f| f.square).map(|f| f.lists);
                        if plain && (elements.len() > 4 || (row.is_some() && elements.len() >= 2)) {
                            let mut label = label_of(&stack, &pending_name);
                            if let Some(row) = row {
                                label = format!("{label}[{row}]");
                                stack.last_mut().unwrap().lists += 1;
                            }
                            let range = t.span().start..tokens[close].span().end;
                            push_literal(&mut out, &stack, top_let, range, ValueKind::Data, label, source);
                            if let Some(last) = out.literals.last_mut() {
                                last.elements = elements;
                            }
                            i = close + 1;
                            continue;
                        }
                    }
                    let mut region = None;
                    if matches!((t.token()).kind(), ScriptTokenTag::OpenCurly) {
                        if let Some((start, fn_name, depth)) = pending_fn.clone().filter(|(_, _, d)| *d == stack.len()) {
                            let _ = depth;
                            pending_fn = None;
                            let end = tokens.get(close).map_or(source.len(), |t| t.span().end);
                            region = Some(new_region(&mut out, &stack, &local, top_let, RegionKind::Fn, fn_name, start..end));
                        } else {
                            let prev = i.checked_sub(1).map(|p| tokens[p].token());
                            let type_name = ident(i.wrapping_sub(1)).filter(|n| n.chars().next().is_some_and(char::is_uppercase));
                            let as_value = prev.is_none_or(|p| matches!((p).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenCurly));
                            if type_name.is_some() || as_value {
                                let key = stack.last().and_then(|f| f.key.clone()).or_else(|| pending_name.clone());
                                let label = match (&type_name, &key) {
                                    (Some(ty), Some(k)) => format!("{k}: {ty}"),
                                    (Some(ty), None) => ty.clone(),
                                    (None, Some(k)) => k.clone(),
                                    (None, None) => "{…}".into(),
                                };
                                let start = if type_name.is_some() { tokens[i - 1].span().start } else { t.span().start };
                                let end = tokens.get(close).map_or(source.len(), |t| t.span().end);
                                region = Some(new_region(&mut out, &stack, &local, top_let, RegionKind::Object, label, start..end));
                            }
                        }
                    }
                    let key = stack.last().and_then(|f| f.key.clone());
                    if matches!((t.token()).kind(), ScriptTokenTag::OpenSquare) {
                        if let Some(parent) = stack.last_mut().filter(|f| f.square) {
                            parent.lists += 1;
                        }
                    }
                    stack.push(Frame { close, region, key: if matches!((t.token()).kind(), ScriptTokenTag::OpenSquare) { key } else { None }, call: None, arg: 0, square: matches!((t.token()).kind(), ScriptTokenTag::OpenSquare), lists: 0 });
                }
                ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseSquare | ScriptTokenTag::CloseRound => {
                    while let Some(f) = stack.pop() {
                        if f.close == i {
                            break;
                        }
                    }
                }
                ScriptTokenTag::Separator => {
                    if let Some(f) = stack.last_mut() {
                        f.arg += 1;
                        if f.call.is_none() {
                            f.key = None;
                        }
                    }
                }
                ScriptTokenTag::String => {
                    let label = label_of(&stack, &pending_name);
                    let text = &source[t.span().clone()];
                    let leaf = label.rsplit(['.', ' ']).next().unwrap_or("");
                    let multiline = text.contains("\\n") || matches!(leaf, "text" | "caption" | "title" | "label" | "lines" | "words" | "body");
                    // A font's family (`Font{family: "Archivo"}`) is a font too.
                    let kind = if leaf == "font" || leaf == "family" { ValueKind::Font } else { ValueKind::Text { multiline } };
                    push_literal(&mut out, &stack, top_let, t.span().clone(), kind, label, source);
                }
                ScriptTokenTag::Color => {
                    let label = label_of(&stack, &pending_name);
                    push_literal(&mut out, &stack, top_let, t.span().clone(), ValueKind::Color, label, source);
                }
                _ if is_number(t.token()) => {
                    let mut start = t.span().start;
                    // A sign, not a subtraction.
                    if i >= 1 && op_is(tokens[i - 1].token(), "-") && tokens[i - 1].span().end == t.span().start {
                        let before = i.checked_sub(2).map(|p| tokens[p].token());
                        if before.is_none_or(|b| matches!((b).kind(), ScriptTokenTag::Operator | ScriptTokenTag::Separator | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare | ScriptTokenTag::OpenCurly)) {
                            start = tokens[i - 1].span().start;
                        }
                    }
                    let label = label_of(&stack, &pending_name);
                    push_literal(&mut out, &stack, top_let, start..t.span().end, ValueKind::Number { dims: 1 }, label, source);
                }
                ScriptTokenTag::Operator if name(t.token().operator()) == "@" => {
                    if let Some(n) = ident(i + 1).filter(|_| tokens[i + 1].span().start == t.span().end) {
                        let label = label_of(&stack, &pending_name);
                        let kind = if crate::Ease::from_id(&n).is_some() || n == "linear" { ValueKind::Ease } else { ValueKind::Enum { choices: vec![n.clone()] } };
                        push_literal(&mut out, &stack, top_let, t.span().start..tokens[i + 1].span().end, kind, label, source);
                        i += 2;
                        continue;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        if let Some(r) = top_let.take() {
            out.regions[r].range.end = tokens.last().map_or(source.len(), |t| t.span().end);
        }
        for (r, _) in local.drain(..) {
            out.regions[r].range.end = tokens.last().map_or(source.len(), |t| t.span().end);
        }
        // Every literal joins the innermost region holding it; a `@name`
        // written as a component's field takes that field's choices.
        let regions = out.regions_at(out.literals.iter().map(|l| l.range.start));
        for (li, region) in regions.into_iter().enumerate() {
            out.literals[li].region = region;
            if let Some(r) = region {
                out.regions[r].literals.push(li);
                if matches!(out.literals[li].kind, ValueKind::Enum { .. }) && out.regions[r].kind == RegionKind::Object {
                    let ty = out.regions[r].name.rsplit(": ").next().unwrap_or("").to_string();
                    let field = out.literals[li].label.clone();
                    if let Some(super::super::layers::FieldType::Enum { choices }) = super::field_type(&ty, &field) {
                        out.literals[li].kind = ValueKind::Enum { choices: choices.iter().map(|c| c.to_string()).collect() };
                    }
                }
            }
        }
        out
    }

    fn label_of(stack: &[Frame], pending: &Option<String>) -> String {
        for f in stack.iter().rev() {
            if let Some(k) = &f.key {
                return k.clone();
            }
            if let Some(c) = &f.call {
                return format!("{c} #{}", f.arg + 1);
            }
        }
        pending.clone().unwrap_or_default()
    }

    fn push_literal(out: &mut CodeOutline, stack: &[Frame], _top: Option<usize>, range: Range<usize>, kind: ValueKind, label: String, source: &str) {
        let text = source[range.clone()].to_string();
        let mut callee = None;
        let mut key = None;
        let mut call_id = 0;
        for f in stack.iter().rev() {
            if let Some(k) = &f.key {
                key = Some(k.clone());
                break;
            }
            if let Some(c) = &f.call {
                callee = Some((c.clone(), f.arg));
                call_id = f.close;
                break;
            }
        }
        let in_loop = IN_LOOP.with(std::cell::Cell::get);
        let in_list = stack.last().is_some_and(|f| f.square);
        out.literals.push(Literal { range, kind, label, text, region: None, callee, call_id, key, in_loop, in_list, elements: Vec::new() });
    }

    thread_local! {
        /// Reading a `for` header (up to its body).
        static IN_LOOP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The parameters of what Motion code may call without defining it:
    /// the engine's native functions and methods (`c.text(text, x, y, o)`),
    /// its modules' functions (`song_line(s, text, nth)`), the shader
    /// library's and the math builtins'. Read from their declarations.
    pub fn known_params(callee: &str) -> Option<Vec<String>> {
        static TABLE: std::sync::OnceLock<Vec<(String, Vec<String>)>> = std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let mut out: Vec<(String, Vec<String>)> = Vec::new();
            for src in [include_str!("draw2d.rs"), include_str!("type_fns.rs"), include_str!("value.rs"), include_str!("kernel2d.rs")] {
                for part in src.split("vm.add_method(").skip(1) {
                    let Some(at) = part.find("id_lut!(") else { continue };
                    let rest = &part[at + 8..];
                    let Some(close) = rest.find(')') else { continue };
                    let name = rest[..close].to_string();
                    let Some(args) = rest.find("script_args!(").map(|a| &rest[a + 13..]) else { continue };
                    let Some(end) = args.find(')') else { continue };
                    let params = args[..end].split(',').filter_map(|p| p.split('=').next()).map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                    out.push((name, params));
                }
            }
            // The modules a host installed (a pack's, at its start) declare theirs too.
            let installed = crate::modules::installed().into_iter().map(|(_, _, source)| source);
            for src in [
                include_str!("../modules/song.splash"),
                include_str!("../modules/strokes.splash"),
                include_str!("../modules/film.splash"),
                include_str!("../modules/style.splash"),
                include_str!("../modules/camera2d.splash"),
                include_str!("../modules/numbers.splash"),
                include_str!("../modules/print.splash"),
                include_str!("../modules/sparks.splash"),
                include_str!("../modules/spectrum.splash"),
                include_str!("../../../../../platform/script/compute/std/shared.splash"),
            ].into_iter().chain(installed) {
                out.extend(outline_unnamed(src, &lex(src)).fns);
            }
            for (name, params) in [
                ("mix", &["a", "b", "t"][..]),
                ("clamp", &["x", "lo", "hi"]),
                ("smoothstep", &["edge0", "edge1", "x"]),
                ("pow", &["x", "power"]),
                ("min", &["a", "b"]),
                ("max", &["a", "b"]),
                ("atan2", &["y", "x"]),
                ("vec2", &["x", "y"]),
                ("vec3", &["x", "y", "z"]),
                ("vec4", &["x", "y", "z", "w"]),
                ("rgb", &["r", "g", "b"]),
                ("rgba", &["r", "g", "b", "a"]),
                ("hsl", &["h", "s", "l"]),
            ] {
                out.push((name.to_string(), params.iter().map(|p| p.to_string()).collect()));
            }
            out
        });
        let name = callee.rsplit('.').next().unwrap_or(callee);
        table.iter().find(|(n, _)| n == name).map(|(_, p)| p.clone())
    }

    #[allow(dead_code)]
    fn _unused(_: &LexedToken) {}
}

#[cfg(test)]
mod tests {
    use crate::plain::PrimitiveFrame;
    use super::*;

    const DOC: &str = "use mod.motion.*\nlet accent = #ff5a36\nMotion{\n    duration: 4.0\n    inputs: {title: input(@text, \"Hello\", \"Title\") tint: input(@color, #102030)}\n    layers: [\n        Rect{at: vec2(10, 20) fill: accent radius: 4}\n        Group{layers: [\n            Text{text: \"Make\\nit move\" font: Font{family: \"Inter\"} size: 48 align: @center color: in(@tint)\n                 opacity: tween(0, 1, 0.0, 0.4, @ease_out_cubic) y: keys([0, 30], [0.6, 0, @ease_out_back]) x: 2 * 3}\n        ]}\n    ]\n}\n";

    fn find<'a>(sites: &'a [Site], layer: &str, field: &str) -> Vec<&'a Site> {
        sites.iter().filter(|site| site.layer() == Some(layer) && site.field() == field).collect()
    }

    #[test]
    fn every_value_is_a_site_with_its_kind() {
        let sites = sites(DOC).unwrap();
        let text = find(&sites, "layers[1].layers[0]", "text");
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].kind, ValueKind::Text { multiline: true });
        assert_eq!(text[0].words().as_deref(), Some("Make\nit move"));
        // The family named in the layer's `Font{…}` is a font site (a picker).
        let family = sites.iter().find(|site| site.words().as_deref() == Some("Inter")).expect("the font's family");
        assert_eq!((family.kind.clone(), family.field()), (ValueKind::Font, "font.family"), "{:?}", family.layer());
        assert_eq!(find(&sites, "layers[0]", "at")[0].kind, ValueKind::Number { dims: 2 });
        assert!(matches!(&find(&sites, "layers[1].layers[0]", "align")[0].kind, ValueKind::Enum { choices } if choices.iter().any(|c| c == "left") && choices.iter().any(|c| c == "center")));
        assert_eq!(find(&sites, "layers[1].layers[0]", "color")[0].kind, ValueKind::Input("tint".into()));
        assert_eq!(find(&sites, "layers[1].layers[0]", "x")[0].kind, ValueKind::Code);
        // A let's colour is edited at the let.
        let fill = find(&sites, "layers[0]", "fill");
        assert_eq!(fill[0].kind, ValueKind::Color);
        assert_eq!(fill[0].text, "#ff5a36");
        // Keys: times, values and eases.
        let y: Vec<_> = find(&sites, "layers[1].layers[0]", "y");
        assert!(y.iter().any(|s| s.owner == Owner::Key { layer: "layers[1].layers[0]".into(), field: "y".into(), index: 1, what: KeyPart::Ease } && s.kind == ValueKind::Ease));
        assert!(y.iter().any(|s| matches!(s.owner, Owner::Key { index: 1, what: KeyPart::Time, .. }) && s.text == "0.6"));
        let opacity = find(&sites, "layers[1].layers[0]", "opacity");
        assert_eq!(opacity.len(), 5);
        // Inputs: the defaults.
        assert!(sites.iter().any(|s| s.owner == Owner::Input { name: "title".into() } && s.kind == ValueKind::Text { multiline: true }));
        assert!(sites.iter().any(|s| s.owner == Owner::Input { name: "tint".into() } && s.kind == ValueKind::Color));
        assert!(sites.iter().any(|s| s.owner == Owner::Doc { field: "duration".into() }));
    }

    #[test]
    fn a_splice_replaces_one_value_and_keeps_the_rest() {
        let all = sites(DOC).unwrap();
        let size = find(&all, "layers[1].layers[0]", "size")[0].clone();
        let next = splice(DOC, size.range.clone(), "64").unwrap();
        assert_eq!(next, DOC.replacen("size: 48", "size: 64", 1));
        assert!(splice(DOC, size.range.clone(), "64 }").is_err());
        let text = find(&all, "layers[1].layers[0]", "text")[0].clone();
        let next = splice(DOC, text.range.clone(), &crate::patch::fmt_text("Two\nlines \"quoted\"")).unwrap();
        let again = sites(&next).unwrap();
        assert_eq!(find(&again, "layers[1].layers[0]", "text")[0].words().as_deref(), Some("Two\nlines \"quoted\""));
    }

    #[test]
    fn records_and_data_lists_are_sites_per_entry() {
        let doc = "use mod.motion.*\nMotion{layers: [Chart{data: [[\"Q1\", 34], [\"Q2\", 46]] walk: {start: 0.5 steps: 6}}]}";
        let all = sites(doc).unwrap();
        let data: Vec<_> = all.iter().filter(|s| s.field() == "data").collect();
        assert_eq!(data.len(), 4);
        assert!(data.iter().any(|s| s.text == "46" && matches!(&s.owner, Owner::Field { part, .. } if part == "1.1")));
        assert!(all.iter().any(|s| s.field() == "walk.steps" && s.text == "6"));
    }

    #[test]
    fn site_at_finds_the_innermost() {
        let all = sites(DOC).unwrap();
        let at = DOC.find("vec2(10").unwrap() + 6;
        assert_eq!(site_at(&all, at).unwrap().text, "vec2(10, 20)");
    }

    #[test]
    fn code_reads_literals_in_functions_lets_and_objects() {
        let src = "use mod.motion.*\nlet prompt = {text: \"Draw a unicorn\" size: 0.155 col: @bone}\nlet draw_type = fn(c, t) {\n    note(\"% prompt\", -3.98, 2.36, t + 0.12, {em: 0.155 col: #eee9df})\n    c.text(\"Hi\", vec2(1, -2))\n}\nMotion{layers: [Draw2D{draw: fn(c, t, edit) { draw_type(c, t) }}]}";
        let o = code::outline(src);
        let names: Vec<&str> = o.regions.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"prompt") && names.contains(&"draw_type") && names.contains(&"note") && names.contains(&"c.text"), "{names:?}");
        let words: Vec<String> = o.literals.iter().filter_map(|l| l.words()).collect();
        assert!(words.contains(&"Draw a unicorn".to_string()) && words.contains(&"% prompt".to_string()), "{words:?}");
        let neg = o.literals.iter().find(|l| l.text == "-3.98").expect("signed number");
        assert_eq!(neg.label, "");
        assert_eq!(neg.callee, Some(("note".to_string(), 1)));
        let o2 = code::outline("let o_lerp = fn(a, b, t) { a + (b - a) * t }\nlet O_B = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6]\nlet f = fn(c, t) {\n    for i in 0..24 { c.stroke(pts, 2.5, #fff, 1) }\n    o_lerp(1, 4, t)\n}");
        assert_eq!(o2.fns.iter().find(|(n, _)| n == "o_lerp").map(|(_, p)| p.clone()), Some(vec!["a".to_string(), "b".to_string(), "t".to_string()]));
        let data = o2.literals.iter().find(|l| l.kind == ValueKind::Data).expect("a data list");
        let rows = code::outline("let R = [\n    [1, 2, 3, 4, 5, 6]\n    [7, 8],\n    [9, 10, 11]\n]");
        assert_eq!(rows.literals.iter().filter(|l| l.kind == ValueKind::Data).map(|l| l.label.as_str()).collect::<Vec<_>>(), vec!["R[0]", "R[1]", "R[2]"]);
        assert_eq!((data.label.as_str(), data.elements.len()), ("O_B", 6));
        assert!(o2.literals.iter().any(|l| l.text == "4" && l.label == "b"), "{:?}", o2.literals.iter().map(|l| (&l.text, &l.label)).collect::<Vec<_>>());
        assert!(o2.literals.iter().any(|l| l.text == "2.5" && l.label == "width"));
        assert_eq!(code::known_params("song_line"), Some(vec!["s".to_string(), "text".to_string(), "nth".to_string()]));
        assert!(code::known_params("linear_rgb").is_some() && code::known_params("c.text").is_some());
        assert!(o2.literals.iter().any(|l| l.text == "24" && l.in_loop));
        assert!(!o2.literals.iter().any(|l| l.text == "2.5" && l.in_loop));
        assert!(o.literals.iter().any(|l| l.text == "vec2(1, -2)" && l.kind == ValueKind::Number { dims: 2 }));
        assert!(o.literals.iter().any(|l| l.text == "@bone" && l.label == "col"));
        let unicorn = o.literals.iter().position(|l| l.words().as_deref() == Some("Draw a unicorn")).unwrap();
        let region = o.literals[unicorn].region.unwrap();
        assert_eq!(o.regions[region].name, "prompt");
        let note = o.regions.iter().position(|r| r.name == "note").unwrap();
        assert_eq!(o.regions[o.regions[note].parent.unwrap()].name, "draw_type");
        assert_eq!(o.regions[o.regions.iter().position(|r| r.name == "draw_type").unwrap()].kind, code::RegionKind::Let);
        // A function's own `let`s are named values in it.
        let f = code::outline("let f = fn(c, t) {\n    let u = (t - 1) * 2\n    let k = wobble(t, 0.5)\n    c.text(\"x\", u, 0.25)\n}");
        let r = |n: &str| f.regions.iter().position(|r| r.name == n).unwrap_or_else(|| panic!("{n}: {:?}", f.regions.iter().map(|r| &r.name).collect::<Vec<_>>()));
        assert_eq!(f.regions[r("u")].kind, code::RegionKind::Let);
        assert_eq!(f.regions[f.regions[r("u")].parent.unwrap()].name, "f");
        assert!(f.regions[r("u")].range.end < f.regions[r("k")].range.start);
        assert_eq!(f.regions[f.regions[r("c.text")].parent.unwrap()].name, "f", "a call after the lets is the function's");
        assert_eq!(f.regions[f.regions[r("wobble")].parent.unwrap()].name, "k", "{:?}", f.regions);
        let quarter = f.literals.iter().find(|l| l.text == "0.25").unwrap();
        assert_eq!(quarter.label, "y", "not named by the last let");
        let timed = code::outline("use mod.motion.*\nMotion{layers: [Text{opacity: tween(0, 1, 0.5, 1.2, @ease_out) y: keys([0, 30], [0.6, 0, @ease_out_back], [t0 + 1, 5])}]}");
        assert_eq!(timed.times.len(), 2);
        assert!(timed.times[0].tween && timed.times[0].keys.iter().map(|k| k.t_value).collect::<Vec<_>>() == vec![Some(0.5), Some(1.2)]);
        assert_eq!(timed.times[1].keys.iter().map(|k| k.t_value).collect::<Vec<_>>(), vec![Some(0.0), Some(0.6), None]);
    }

    #[test]
    fn times_are_found_by_use_and_name() {
        let names = code::time_names("let O_T0 = 0.0\nlet W_I = vec2(1.4, 2.35)\nlet EYE = vec2(2.9, 2.0)\nlet f = fn(c, t) {\n    let u = (t - O_T0) / 2\n    if T >= W_I.x { c.text(\"I\", EYE.x, 0) }\n    let k = tween(0, 1, START, 2)\n}");
        assert!(names.contains("O_T0") && names.contains("W_I") && names.contains("START"), "{names:?}");
        assert!(!names.contains("EYE") && !names.contains("u"), "{names:?}");
        assert!(code::time_named("O_tCirc") && !code::time_named("O_text"));
        assert!(code::time_named("O_T0") && code::time_named("T1") && code::time_named("intro_at") && !code::time_named("TABLE_POS") && !code::time_named("EYE"));
    }

    #[test]
    fn lists_are_known_by_what_indexes_them() {
        let idx = code::list_indexing("let A = [1, 2, 3]\nlet f = fn(c, t) { let v = A[floor(t * 60)]\n let g = GL[ch]\n let w = W[wi + 1]\n let q = Q[i] }");
        assert_eq!(idx.get("A"), Some(&code::ListIndex::Time { per_second: Some(60.0) }));
        assert_eq!(idx.get("GL"), Some(&code::ListIndex::Letters));
        assert_eq!(idx.get("W"), Some(&code::ListIndex::Words));
        assert_eq!(idx.get("Q"), None);
    }

    #[test]
    fn a_call_gathers_the_lets_that_fed_it() {
        let src = "let SPEED = 2.0\nlet unused = 9.0\nlet draw = fn(c, t) {\n    let r = SPEED * t\n    let w = r + 1.0\n    c.circle(w, 0.0, 3.0)\n}\n";
        let code = code::outline(src);
        let at = src.find("c.circle").unwrap();
        let got: Vec<String> = code::gather(&code, src, at, 20).into_iter().map(|g| g.name).collect();
        assert_eq!(got.first().map(String::as_str), Some("c.circle"));
        assert!(got.contains(&"w".to_string()) && got.contains(&"r".to_string()) && got.contains(&"SPEED".to_string()), "{got:?}");
        assert!(!got.contains(&"unused".to_string()), "{got:?}");
    }

    #[test]
    fn what_moves_in_time_is_found_by_its_dataflow() {
        let src = "let PALETTE = [#fff, #000]\nlet SIZE = 40\nlet wobble = fn(x) { x * 2 }\nlet speed = fn(t) { SIZE * t }\nlet draw = fn(c, t) {\n    let r = SIZE + 2\n    let k = speed(t)\n    let q = k + 1\n    c.circle(q, r, 3)\n}";
        let o = code::outline(src);
        let rel = code::time_relevant(&o, src, &Default::default(), &[]);
        let of = |n: &str| rel[o.regions.iter().position(|r| r.name == n).unwrap_or_else(|| panic!("{n}"))];
        assert!(!of("PALETTE") && !of("SIZE") && !of("wobble"));
        assert!(of("speed") && of("k") && of("q") && of("draw"));
        assert!(!of("r"), "a constant inside a moving function");
        // A field's key or a field read is not a read of a name.
        let src2 = "let G = [{x: 1, y: 2}]\nlet f = fn(c, t) {\n    let x = t * 2\n    let b = G[0]\n    c.circle(b.x, x, 3)\n}\nlet g = fn(c) { let x = 3\n c.rect(x, 0, 1, 1) }";
        let o2 = code::outline(src2);
        let rel2 = code::time_relevant(&o2, src2, &Default::default(), &[]);
        let of2 = |n: &str| rel2[o2.regions.iter().position(|r| r.name == n).unwrap_or_else(|| panic!("{n}"))];
        assert!(!of2("G"), "x: is a key, not the moving x");
        assert!(!of2("g"), "another function's x is its own");
        assert!(of2("f"));
    }

    #[test]
    fn a_traced_draw_maps_to_its_line_past_non_ascii_and_raw_strings() {
        // Non-ASCII (’ — × −) and raw strings (a quote inside, a line
        // break inside) before the draw: its call still maps to its line.
        let src = "use mod.motion.*\nlet A = \"it’s — 2×3 − 1\"\nlet B = r#\"a \"quoted\" \\draw\nsecond line\"#\nlet C = r\"\\x\"\nlet draw_dial = fn(c, t) {\n    let w = \"surprisal −log p\"\n    c.text(\"red\", 0, 0, {px: 10})\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::MotionDoc::load(src).expect("loads");
        let frame = doc.eval_traced(0.5);
        let list = frame.walk().into_iter().find_map(|(_, item)| match &item.kind {
            crate::ItemKind::Draw2d { picture, .. } => Some(picture.list.clone()),
            _ => None,
        }).expect("a Draw2D item");
        let mark = list.marks.last().expect("a mark");
        let at = doc.code_at(&mark.sites);
        let byte = at[0].as_ref().unwrap().byte_in(src).unwrap();
        let line = src[..byte].matches('\n').count() + 1;
        assert_eq!(line, 8, "{at:?}");
        let outline = code::outline(src);
        let region = outline.region_at(byte).map(|r| outline.regions[r].name.clone());
        assert_eq!(region.as_deref(), Some("c.text"));
        // Its first argument, as written.
        let arg = code::call_argument(&outline, src, byte, 0).unwrap();
        assert_eq!(&src[arg], "\"red\"");
    }

    #[test]
    fn a_traced_draws_callers_map_to_their_call_lines_past_doc_lines_and_continuations() {
        // `///` docs, a raw string over lines, a `\`-newline continuation,
        // and nested calls: each frame of the stack maps to the line of
        // its call (a caller's to the call, not the statement after it).
        let src = "use mod.motion.*\n/// the label\n/// over two lines\nlet A = \"one \\\n    two\"\nlet B = r\"x\ny\"\n/// the dot\nlet dot = fn(c) {\n    c.text(\"o\", 0, 0, {px: 10})\n    return 0\n}\n/// a stroke\nlet stroke = fn(c, k) {\n    dot(c)\n    if k >= 1.0 { return 0 }\n    return 1\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) {\n        stroke(c, 0.5)\n        return 0\n    }}\n]}";
        let line_of = |needle: &str| src[..src.find(needle).unwrap()].matches('\n').count() + 1;
        let doc = crate::MotionDoc::load(src).expect("loads");
        let frame = doc.eval_traced(0.5);
        let list = frame.walk().into_iter().find_map(|(_, item)| match &item.kind {
            crate::ItemKind::Draw2d { picture, .. } => Some(picture.list.clone()),
            _ => None,
        }).expect("a Draw2D item");
        let mark = list.marks.last().expect("a mark");
        let lines: Vec<usize> = doc.code_at(&mark.sites).into_iter().flatten().filter_map(|at| at.byte_in(src)).map(|b| src[..b].matches('\n').count() + 1).collect();
        assert_eq!(lines[..3], [line_of("c.text"), line_of("    dot(c)"), line_of("        stroke(c, 0.5)")], "{lines:?}");
    }

    #[test]
    fn a_watched_lets_values_map_to_their_lines_past_doc_lines() {
        let src = "use mod.motion.*\n/// the dot\nlet dot = fn(c, k) {\n    let lift = k * 2.0\n    c.text(\"o\", 0, 0, {px: 10})\n    return lift\n}\n/// One stroke: a doc over\n/// two lines.\nlet stroke = fn(c, t) {\n    let heat = 0.25\n    let when = t * 4.0\n    dot(c, heat)\n    return 0\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) {\n        stroke(c, t)\n        return 0\n    }}\n]}";
        let line_of = |needle: &str| src[..src.find(needle).unwrap()].matches('\n').count() + 1;
        let doc = crate::MotionDoc::load(src).expect("loads");
        let (_, lets) = doc.eval_watched(0.5);
        let at = doc.code_at(&lets.iter().map(|(ip, _, _)| *ip).collect::<Vec<_>>());
        for ((_, name, _), at) in lets.iter().zip(at) {
            let Some(byte) = at.and_then(|a| a.byte_in(src)) else { continue };
            let line = src[..byte].matches('\n').count() + 1;
            let want = line_of(&format!("let {name} ="));
            assert_eq!(line, want, "{name}");
        }
    }

    #[test]
    fn a_calls_properties_and_their_literals() {
        let src = "let SANS = \"Roboto Flex\"\nlet draw = fn(c, t) {\n    let col = if t > 1 { #ff0000 } else { #00ff00 }\n    let size = 100\n    c.text(\"red\", 0.0, 2.5, {px: size font: SANS weight: 700 color: col})\n}\n";
        let code = code::outline(src);
        let at = src.find("c.text").unwrap();
        let props: Vec<(String, String)> = code::call_properties(&code, src, at).into_iter().map(|p| (p.name, src[p.expr].to_string())).collect();
        let get = |n: &str| props.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        assert_eq!(get("px").as_deref(), Some("size"), "{props:?}");
        assert_eq!(get("font").as_deref(), Some("SANS"));
        assert_eq!(get("weight").as_deref(), Some("700"));
        assert_eq!(get("color").as_deref(), Some("col"));
        let call = code.region_at(at).unwrap();
        let lit = |n: &str| {
            let p = code::call_properties(&code, src, at).into_iter().find(|p| p.name == n).unwrap();
            code::editable_literal(&code, src, call, &p.expr).map(|i| code.literals[i].text.clone())
        };
        assert_eq!(lit("px").as_deref(), Some("100"));
        assert_eq!(lit("font").as_deref(), Some("\"Roboto Flex\""));
        assert_eq!(lit("weight").as_deref(), Some("700"));
        assert_eq!(lit("color"), None, "computed: an if");
    }

    #[test]
    fn a_call_argument_reads_back_to_its_lets() {
        let src = "let L_CH = [[\"A\"], [\"r\", \"e\", \"d\"]]\nlet draw = fn(c, t) {\n    let li = 1\n    let chs = L_CH[li]\n    let color_name = \"red\"\n    c.text(chs[0], 0, 0, {px: 10 label: color_name})\n}\n";
        let code = code::outline(src);
        let at = src.find("c.text").unwrap();
        let arg = code::call_argument(&code, src, at, 0).unwrap();
        assert_eq!(&src[arg.clone()], "chs[0]");
        let call = code.region_at(at).unwrap();
        let got: Vec<String> = code::gather_from(&code, src, call, &arg, 10).into_iter().map(|g| g.name).collect();
        assert_eq!(got, ["chs", "L_CH", "li"], "the text's own reads, not the call's other arguments");
    }

    #[test]
    fn a_drawn_character_leads_to_the_record_its_word_was_written_in() {
        let src = "use mod.motion.*\nlet SONG = [{w: \"A\" start: 1.0}, {w: \"red\" start: 1.5}, {w: \"thread\" start: 2.0}]\nlet ROWS = [{words: SONG}]\nlet WORDS = [{word: SONG[1] text: SONG[1].w}]\nlet draw = fn(c, t) {\n    for li in 0..1 {\n        let text = \"A red thread\"\n        for gi in 0..12 {\n            c.text(\"x\", gi * 10, 0, {px: 10})\n        }\n    }\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw(c, t) }}\n]}";
        let doc = crate::value::editable(|| crate::MotionDoc::load(src)).expect("loads");
        // The loops each glyph was drawn in.
        let frame = doc.eval_traced(0.5);
        let list = frame.walk().into_iter().find_map(|(_, item)| match &item.kind {
            crate::ItemKind::Draw2d { picture, .. } => Some(picture.list.clone()),
            _ => None,
        }).expect("a Draw2D item");
        let fifth = list.marks.iter().find(|m| m.loops.iter().any(|(id, v)| *id == makepad_script::LiveId::from_str("gi") && *v == 4.0)).expect("a mark per glyph");
        assert!(fifth.loops.iter().any(|(id, v)| *id == makepad_script::LiveId::from_str("li") && *v == 0.0), "{:?}", fifth.loops);
        // Character 4 ("d" of "red") of row 0 came from the "red" record.
        let (at, words, start) = doc.text_origin(&["ROWS".to_string()], 0, 4).expect("an origin");
        assert_eq!(start, Some(1.5));
        assert_eq!(words, "red");
        assert_eq!(at.line, 2, "{at:?}");
        let byte = at.byte_in(src).unwrap();
        assert!(src[byte..].starts_with("{w: \"red\""), "{}", &src[byte..byte + 12]);
        // A record made from it (a word's own record): back to where the
        // words were first written.
        let (at, words, _) = doc.text_origin(&["WORDS".to_string()], 0, 1).expect("an origin");
        assert_eq!(words, "red");
        let byte = at.byte_in(src).unwrap();
        assert!(src[byte..].starts_with("{w: \"red\""), "{}", &src[byte..byte + 12]);
    }

    #[test]
    fn a_traced_draw_says_which_code_drew_it() {
        let src = "use mod.motion.*\nlet draw_dial = fn(c, t) {\n    c.seg(0, 0, 10, 10, 2, #ffffff, 1)\n    c.seg(0, 0, 20, 5, 2, #ff0000, 1)\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::MotionDoc::load(src).expect("loads");
        let frame = doc.eval_traced(0.5);
        let list = frame.walk().into_iter().find_map(|(_, item)| match &item.kind {
            crate::ItemKind::Draw2d { picture, .. } => Some(picture.list.clone()),
            _ => None,
        }).expect("a Draw2D item");
        assert_eq!(list.marks.len(), 2, "{:?}", list.marks);
        let at = doc.code_at(&list.marks[1].sites);
        assert_eq!(at[0].as_ref().map(|a| (a.file.clone(), a.line)), Some((CodeFile::Doc, 4)));
        assert_eq!(at[1].as_ref().map(|a| a.line), Some(7), "the caller: {at:?}");
        let byte = at[0].as_ref().unwrap().byte_in(src).unwrap();
        let outline = code::outline(src);
        let region = outline.region_at(byte).map(|r| outline.regions[r].name.clone());
        assert_eq!(region.as_deref(), Some("c.seg"));
        // Untraced: no marks.
        let frame = doc.eval(0.5);
        assert!(frame.walk().into_iter().all(|(_, item)| !matches!(&item.kind, crate::ItemKind::Draw2d { picture, .. } if !picture.list.marks.is_empty())));
    }

    #[test]
    fn a_watched_frame_says_what_its_lets_bound() {
        let src = "use mod.motion.*\nlet draw_dial = fn(c, t) {\n    let r = 10 + t * 4\n    for i in 0..3 { let k = i * 2 }\n    let name = \"dial\"\n    c.seg(0, 0, r, r, 2, #ffffff, 1)\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::MotionDoc::load(src).expect("loads");
        let (_, lets) = doc.eval_watched(0.5);
        let shown: Vec<(String, String)> = lets.iter().map(|(_, n, v)| (n.clone(), v.text.clone())).collect();
        assert!(shown.contains(&("r".into(), "12".into())), "{shown:?}");
        assert!(shown.contains(&("k".into(), "4".into())), "the last of a loop's: {shown:?}");
        assert!(shown.iter().any(|(n, v)| n == "name" && v.contains("dial")), "{shown:?}");
        let at = doc.code_at(&lets.iter().map(|(ip, _, _)| *ip).collect::<Vec<_>>());
        let r = lets.iter().position(|(_, n, _)| n == "r").unwrap();
        assert_eq!(at[r].as_ref().map(|a| a.line), Some(3));
        // Unwatched: nothing is told.
        assert!(doc.vm.with(|e| e.with_vm(|vm| !vm.has_let_hook())));
    }

    #[test]
    fn a_load_says_what_its_constants_compute() {
        let src = "use mod.motion.*\nlet SONG = {beats: [0.5, 1.0, 1.5, 2.0, 2.5] title: \"x\"}\nlet T_A = 1.0 + 0.5\nlet W = {hi: {start: 1.0, end: 2.0, w: \"hi\"} yo: {start: 2.5, end: 3.0, w: \"yo\"}}\nlet B = [0.5, 1.0, 1.5]\nlet S = vec2(0.2, 0.9)\nMotion{size: vec2(100, 100) duration: 4.0 layers: []}";
        let doc = crate::MotionDoc::load(src).expect("loads");
        let lets = doc.bound_at_load();
        let get = |n: &str| lets.iter().find(|(_, name, _)| name == n).map(|(_, _, w)| w.clone()).unwrap_or_else(|| panic!("{n}: {:?}", lets.iter().map(|l| &l.1).collect::<Vec<_>>()));
        assert_eq!(get("T_A").number, Some(1.5));
        assert_eq!(get("W").spans, vec![(1.0, 2.0, "hi".to_string()), (2.5, 3.0, "yo".to_string())]);
        assert_eq!(get("B").ticks, vec![0.5, 1.0, 1.5]);
        assert_eq!(get("SONG").ticks.len(), 5, "an object's beats");
        assert_eq!(get("S").spans.len(), 1, "{:?}", get("S"));
        let at = doc.code_at(&[lets.iter().find(|l| l.1 == "W").unwrap().0]);
        assert_eq!(at[0].as_ref().map(|a| a.line), Some(4), "1-based");
    }

    #[test]
    fn a_number_dragged_changes_the_running_document() {
        let src = "use mod.motion.*\nlet K = 3.0\nlet draw_dial = fn(c, t) {\n    let k = sin(t * 3.0) * K\n    c.seg(0, 0, k, k, 2, #ffffff, 1)\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::value::editable(|| crate::MotionDoc::load(src)).expect("loads");
        let k_at = |doc: &crate::MotionDoc| doc.eval_watched(0.5).1.into_iter().find(|(_, n, _)| n == "k").and_then(|(_, _, w)| w.number).unwrap();
        let before = k_at(&doc);
        assert!((before - (1.5f64).sin() * 3.0).abs() < 1e-6, "{before}");
        // The 3.0 in the function.
        let at = src.find("t * 3.0").unwrap() + 4;
        assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(5.0)).places > 0);
        let after = k_at(&doc);
        assert!((after - (2.5f64).sin() * 3.0).abs() < 1e-6, "{after}");
        // A top-level let read by the function.
        let at = src.find("3.0").unwrap();
        assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(2.0)).places > 0);
        let again = k_at(&doc);
        assert!((again - (2.5f64).sin() * 2.0).abs() < 1e-6, "{again}");
    }

    #[test]
    fn a_dragged_record_field_changes_the_records_it_built() {
        let src = "use mod.motion.*\nlet LOOK = {\n    axis: {alpha: 0.55 heat: 0.8}\n    prim: {alpha: 0.8 heat: -1.0 col: #ff4d12}\n}\nlet ROWS = [{y: 2.0}, {y: 3.0}]\nlet draw_dial = fn(c, t) {\n    let k = LOOK.prim.alpha + LOOK.prim.heat\n    let r = ROWS[1].y\n    c.seg(0, 0, k, r, 2, LOOK.prim.col, 1)\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::value::editable(|| crate::MotionDoc::load(src)).expect("loads");
        let watched = |doc: &crate::MotionDoc, name: &str| doc.eval_watched(0.5).1.into_iter().find(|(_, n, _)| n == name).and_then(|(_, _, w)| w.number).unwrap();
        assert!((watched(&doc, "k") - (0.8 - 1.0)).abs() < 1e-9);
        // prim's alpha: its record, built at load, holds the new value.
        let at = src.find("0.8 heat: -1").unwrap();
        assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(0.25)).places >= 2);
        assert!((watched(&doc, "k") - (0.25 - 1.0)).abs() < 1e-9);
        // Dragged again, from what it holds now.
        assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(0.5)).places >= 2);
        assert!((watched(&doc, "k") - (0.5 - 1.0)).abs() < 1e-9);
        // A negative field.
        let at = src.find("-1.0").unwrap();
        let reach = doc.patch_literal(&CodeFile::Doc, src, at..at + 4, Literal::Number(-2.0));
        assert!(reach.places >= 2);
        assert!((watched(&doc, "k") - (0.5 - 2.0)).abs() < 1e-9);
        // Read each frame: no load needed.
        assert!(!reach.needs_reload(), "{reach:?}");
        // A list's item's field.
        let at = src.find("3.0").unwrap();
        assert!(doc.patch_literal(&CodeFile::Doc, src, at..at + 3, Literal::Number(7.0)).places >= 2);
        assert!((watched(&doc, "r") - 7.0).abs() < 1e-9);
    }

    #[test]
    fn what_a_load_reads_is_known_from_its_trace() {
        let src = "use mod.motion.*\nlet K = 0.5\nlet LOOK = {alpha: 0.55 travel: 0.025 / K}\nlet SEEN = {a: 1.0}\nlet S_ALPHA = SEEN.a * 2.0\nlet ROWS = [1.0, 2.0].map(fn(x) { x * 3.0 })\nlet draw_dial = fn(c, t) {\n    let k = LOOK.alpha * 4.0\n}\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Draw2D{draw: fn(c, t, edit) { draw_dial(c, t) }}\n]}";
        let doc = crate::value::editable(|| crate::MotionDoc::load(src)).expect("loads");
        let reload = |needle: &str, len: usize, v: f64| {
            let at = src.find(needle).unwrap();
            doc.patch_literal(&CodeFile::Doc, src, at..at + len, Literal::Number(v)).needs_reload()
        };
        // Read each frame: a field of a record, a function's literal.
        assert!(!reload("0.55", 4, 0.6));
        assert!(!reload("4.0", 3, 5.0));
        // Read at load: a computed field, a constant a load computes with,
        // a record field a `let` computes from, a function a load calls.
        assert!(reload("0.025", 5, 0.03));
        assert!(reload("0.5", 3, 0.7));
        assert!(reload("1.0}", 3, 2.0));
        assert!(reload("3.0", 3, 4.0));
    }

    #[test]
    fn a_literal_reach_is_known_without_patching() {
        let src = "use mod.motion.*\nlet K = 0.5\nlet LOOK = {alpha: 0.55 travel: 0.025 / K}\nlet wide = fn() { LOOK.alpha * 100.0 }\nMotion{size: vec2(100, 100) duration: 1.0 layers: [\n    Rect{size: fn(t) { vec2(wide(), 10) } fill: #ffffff}\n]}";
        let doc = crate::value::editable(|| crate::MotionDoc::load(src)).expect("loads");
        let width = || match &doc.eval(0.0).items[0].kind {
            crate::ItemKind::Primitive(PrimitiveFrame::Rect { size, .. }) => size[0],
            _ => panic!("not a rect"),
        };
        let reach = |needle: &str, len: usize| {
            let at = src.find(needle).unwrap();
            doc.literal_reach(&CodeFile::Doc, src, at..at + len).expect("a literal")
        };
        let alpha = reach("0.55", 4);
        assert!(alpha.places > 0 && !alpha.needs_reload());
        assert!(reach("0.5\n", 3).needs_reload());
        assert!(reach("0.025", 5).needs_reload());
        // A read sets nothing: a value dragged since the load stays.
        let at = src.find("0.55").unwrap();
        assert_eq!(doc.patch_literal(&CodeFile::Doc, src, at..at + 4, Literal::Number(0.7)).places, alpha.places);
        assert!((width() - 70.0).abs() < 1e-6);
        assert_eq!(reach("0.55", 4).places, alpha.places);
        assert!((width() - 70.0).abs() < 1e-6);
        assert!(doc.literal_reach(&CodeFile::Doc, src, 0..3).is_none());
    }

    #[test]
    fn every_shipped_catalogue_example_reads() {
        for kind in crate::layers::layer_types() {
            // What the patcher cannot read, the editors show as code.
            if let Some(example) = crate::catalog::layer_example(kind).filter(|example| crate::patch::outline(example).is_ok()) {
                sites(example).unwrap_or_else(|e| panic!("{kind}: {e}"));
            }
        }
    }
}

/// A module body's file name without the revision (`#` and 16 hex digits)
/// that tells the bodies of its successive texts apart.
fn unrevised(file: &str) -> &str {
    match file.rsplit_once('#') {
        Some((name, rev)) if rev.len() == 16 && rev.bytes().all(|b| b.is_ascii_hexdigit()) => name,
        _ => file,
    }
}
