//! Source-preserving edits of a Motion document (EDITS.md §1: the Splash is
//! the truth). The editor's timeline, inspector and viewer change a
//! document through these functions, and each returns the next source with
//! exactly the bytes of the changed value replaced: the AI's (or a
//! person's) layout, spacing, comments, `let`s and sugar stay as written.
//!
//! [`outline`] reads the document's layers, their spans and every property
//! as written, with its keys: `keys(...)` and `tween(...)` both read as
//! keys, so a key of either can be moved, re-valued or re-eased in place (a
//! tween stays a tween while it has two keys). Other forms (`sequence`,
//! `loop`, functions, expressions) are shown and kept, and replaced only
//! when the person sets a plain value or a key on them (`repeat`, the
//! document's loop, included).
//!
//! Times follow the document model: a layer's `span: [start, length]` is on
//! its parent's clock and its keys are on its own clock (from its start), so
//! moving a layer moves its keys with it, and trimming its start keeps them
//! where they are (they shift on the layer's clock).
//!
//! Where a value is a name, the edit goes where the name is defined: a
//! `let accent = #ff5a36` gets the new colour, and `in(@headline)` changes
//! the input's default, so the document stays parameterised.

mod document;

pub use document::{type_name, unqualified, Document};
use makepad_script::syntax::{self, edit, write::quote, Kind, Node, Syntax};
use std::ops::Range;

/// One value or fragment of Splash (`keys([0, 1], [1, 2])`).
fn parse_value(text: &str) -> Result<Node, syntax::SyntaxError> {
    syntax::parse_value(text)
}

/// Times written by the editor are rounded to this (seconds).
const TIME_STEP: f64 = 1e-4;

/// One layer as written.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerOutline {
    /// `layers[2]`, `layers[2].layers[0]`, `layers[1].mask`: the model's
    /// [`crate::Layer::path`].
    pub path: String,
    pub ty: String,
    pub name: Option<String>,
    /// `span: [start, length]` in seconds when both are plain numbers.
    pub span: Option<(f64, f64)>,
    /// Whether `span` is written in another form (beats, a let).
    pub span_written: bool,
    pub visible: bool,
    pub locked: bool,
    pub props: Vec<PropOutline>,
    pub children: Vec<LayerOutline>,
    /// The layer's constructor in the source.
    pub range: Range<usize>,
}

impl LayerOutline {
    pub fn prop(&self, name: &str) -> Option<&PropOutline> {
        self.props.iter().find(|prop| prop.name == name)
    }

    /// A label: its `name`, else its type and text.
    pub fn label(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        match self.prop("text").map(|prop| &prop.form) {
            Some(PropForm::Value(text)) if text.starts_with('"') => format!("{} {}", self.ty, text),
            _ => self.ty.clone(),
        }
    }

    /// Whether any property is animated.
    pub fn animated(&self) -> bool {
        self.props.iter().any(|prop| prop.form.is_animated())
    }
}

/// One property as written.
#[derive(Clone, Debug, PartialEq)]
pub struct PropOutline {
    pub name: String,
    pub form: PropForm,
    /// The value in the source (where it was written, or the `let` it names).
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropForm {
    /// A plain value, as written (`120`, `vec2(10, 20)`, `#ff5a36`, `"Hi"`).
    Value(String),
    /// `in(@name)`: an input of the document.
    Input(String),
    /// `keys(...)` (`tween: false`) or `tween(...)`.
    Keys { keys: Vec<KeyOutline>, tween: bool },
    /// Anything else, as written: `sequence`, `loop`, a function, an
    /// expression. Shown, kept, not taken apart.
    Other(String),
}

impl PropForm {
    pub fn is_animated(&self) -> bool {
        matches!(self, PropForm::Keys { .. }) || matches!(self, PropForm::Other(text) if is_animation_text(text))
    }

    pub fn keys(&self) -> &[KeyOutline] {
        match self {
            PropForm::Keys { keys, .. } => keys,
            _ => &[],
        }
    }
}

fn is_animation_text(text: &str) -> bool {
    let head = text.trim_start();
    ["sequence(", "repeat(", "loop(", "fn("].iter().any(|prefix| head.starts_with(prefix))
}

/// One key as written: its time on the layer's clock (when it is a plain
/// number), its value and its ease (of the segment arriving at it).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyOutline {
    pub t: Option<f64>,
    pub t_text: String,
    pub value: String,
    /// The ease as written (`@ease_out`, `spring(170, 26)`), `None` linear.
    pub ease: Option<String>,
    /// `@pin`: a key placed by hand that solvers keep.
    pub pin: bool,
}

/// The document's outline: its header and layers, bottom first.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Outline {
    pub duration: Option<f64>,
    pub fps: Option<f64>,
    pub size: Option<[f64; 2]>,
    /// Declared inputs: name and default as written.
    pub inputs: Vec<(String, String)>,
    pub layers: Vec<LayerOutline>,
}

impl Outline {
    /// Every layer, depth first (a group before its children, a mask after
    /// them), as in the model.
    pub fn all(&self) -> Vec<&LayerOutline> {
        fn walk<'a>(layers: &'a [LayerOutline], out: &mut Vec<&'a LayerOutline>) {
            for layer in layers {
                out.push(layer);
                walk(&layer.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.layers, &mut out);
        out
    }

    pub fn layer(&self, path: &str) -> Option<&LayerOutline> {
        self.all().into_iter().find(|layer| layer.path == path)
    }
}

/// A located document: its tree and where its `Motion{...}` is.
struct Doc<'a> {
    source: &'a str,
    syntax: Syntax,
}

impl<'a> Doc<'a> {
    fn new(source: &'a str) -> Result<Self, String> {
        let syntax = syntax::parse(source).map_err(|error| {
            let (line, col) = syntax::line_col(source, error.at);
            format!("{line}:{col}: {}", error.message)
        })?;
        Ok(Self { source, syntax })
    }

    fn document(&self) -> Document<'_> {
        Document::of(&self.syntax)
    }

    /// The `Motion{...}`: the last top-level object with `layers`, or the
    /// last `Motion` constructor.
    fn motion(&self) -> Result<&Node, String> {
        self.document().motion().ok_or_else(|| "the document has no `Motion{...}` with `layers`".to_string())
    }

    /// `name`'s `let` value.
    fn let_value(&self, name: &str) -> Option<&Node> {
        self.document().let_value(name)
    }

    /// A value, following a bare name to the `let` that defines it.
    fn resolve<'n>(&'n self, node: &'n Node) -> &'n Node {
        match &node.kind {
            Kind::Ident(name) => self.let_value(name).map_or(node, |value| self.resolve(value)),
            _ => node,
        }
    }

    /// The layer constructor at `path`.
    fn layer(&self, path: &str) -> Result<&Node, String> {
        let mut node = self.motion()?;
        for step in path.split('.') {
            let (field, index) = match step.find('[') {
                Some(open) => {
                    let index = step[open + 1..].trim_end_matches(']').parse::<usize>().map_err(|_| format!("`{path}` is not a layer path"))?;
                    (&step[..open], Some(index))
                }
                None => (step, None),
            };
            let value = &node.field(field).ok_or_else(|| format!("`{path}`: no `{field}` here"))?.value;
            let value = self.resolve(value);
            node = match index {
                Some(index) => {
                    let items = value.items().ok_or_else(|| format!("`{path}`: `{field}` is not a list"))?;
                    self.resolve(items.get(index).ok_or_else(|| format!("`{path}`: there is no layer {index}"))?)
                }
                None => value,
            };
        }
        match node.kind {
            Kind::Object { .. } => Ok(node),
            _ => Err(format!("`{path}` is not a layer constructor")),
        }
    }

    fn text(&self, node: &Node) -> &'a str {
        &self.source[node.span.clone()]
    }
}

/// A document's own `label`.
pub fn label_of(source: &str) -> Option<String> {
    let syntax = syntax::parse(source).ok()?;
    let document = Document::of(&syntax);
    match &document.values.last()?.field("label")?.value.kind {
        Kind::Str(text) => Some(text.clone()),
        _ => None,
    }
}

/// Read a document's outline.
pub fn outline(source: &str) -> Result<Outline, String> {
    let doc = Doc::new(source)?;
    let motion = doc.motion()?;
    let number = |name: &str| motion.field(name).and_then(|field| doc.resolve(&field.value).number());
    let size = motion.field("size").and_then(|field| numbers(doc.resolve(&field.value))).filter(|v| v.len() == 2).map(|v| [v[0], v[1]]);
    let inputs = motion.field("inputs").and_then(|field| doc.resolve(&field.value).object().map(|(_, fields)| {
        fields.iter().filter_map(|field| Some((field.name.clone()?, doc.text(&field.value).to_string()))).collect()
    })).unwrap_or_default();
    let layers = motion.field("layers").map(|field| layer_list(&doc, doc.resolve(&field.value), "layers")).unwrap_or_default();
    Ok(Outline { duration: number("duration"), fps: number("fps"), size, inputs, layers })
}

fn layer_list(doc: &Doc, list: &Node, prefix: &str) -> Vec<LayerOutline> {
    let Some(items) = list.items() else { return Vec::new() };
    items.iter().enumerate().filter_map(|(index, item)| layer_outline(doc, doc.resolve(item), &format!("{prefix}[{index}]"))).collect()
}

fn layer_outline(doc: &Doc, node: &Node, path: &str) -> Option<LayerOutline> {
    let (ty, fields) = node.object()?;
    let ty = ty?.rsplit('.').next()?.to_string();
    let mut outline = LayerOutline {
        path: path.to_string(),
        ty,
        name: None,
        span: None,
        span_written: false,
        visible: true,
        locked: false,
        props: Vec::new(),
        children: Vec::new(),
        range: node.span.clone(),
    };
    for field in fields {
        let Some(name) = field.name.as_deref() else { continue };
        let value = doc.resolve(&field.value);
        match name {
            "name" => outline.name = match &value.kind {
                Kind::Str(text) => Some(text.clone()),
                _ => None,
            },
            "span" => {
                outline.span = numbers(value).filter(|v| v.len() == 2).map(|v| (v[0], v[1]));
                outline.span_written = outline.span.is_none();
            }
            "visible" => outline.visible = !matches!(&value.kind, Kind::Ident(word) if word == "false"),
            "locked" => outline.locked = matches!(&value.kind, Kind::Ident(word) if word == "true"),
            "layers" => outline.children.extend(layer_list(doc, value, &format!("{path}.layers"))),
            "mask" => outline.children.extend(layer_outline(doc, value, &format!("{path}.mask"))),
            // A Repeat's (or another layout's) one layer: a child the
            // layout places.
            "layer" if value.object().is_some_and(|(ty, _)| ty.is_some()) => outline.children.extend(layer_outline(doc, value, &format!("{path}.layer"))),
            _ => outline.props.push(PropOutline { name: name.to_string(), form: form_of(doc, value), range: value.span.clone() }),
        }
    }
    Some(outline)
}

fn numbers(node: &Node) -> Option<Vec<f64>> {
    match &node.kind {
        Kind::Array { items, .. } => items.iter().map(Node::number).collect(),
        Kind::Number(value) => Some(vec![*value]),
        _ => match node.as_call() {
            Some(("vec2" | "vec3", args)) => args.iter().map(Node::number).collect(),
            _ => None,
        },
    }
}

fn form_of(doc: &Doc, node: &Node) -> PropForm {
    let text = doc.text(node).to_string();
    match node.as_call() {
        Some(("in", args)) => match args.first().and_then(Node::name) {
            Some(input) => PropForm::Input(input.to_string()),
            None => PropForm::Other(text),
        },
        Some(("keys", args)) => {
            let keys: Option<Vec<KeyOutline>> = args.iter().map(|arg| key_of(doc, arg)).collect();
            match keys {
                Some(keys) => PropForm::Keys { keys, tween: false },
                None => PropForm::Other(text),
            }
        }
        Some(("tween", args)) if (4..=5).contains(&args.len()) => {
            let ease = args.get(4).map(|ease| doc.text(ease).to_string()).filter(|ease| ease != "@linear");
            PropForm::Keys {
                keys: vec![
                    KeyOutline { t: doc.resolve(&args[2]).number(), t_text: doc.text(&args[2]).into(), value: doc.text(&args[0]).into(), ease: None, pin: false },
                    KeyOutline { t: doc.resolve(&args[3]).number(), t_text: doc.text(&args[3]).into(), value: doc.text(&args[1]).into(), ease, pin: false },
                ],
                tween: true,
            }
        }
        Some(("sequence" | "repeat" | "loop", _)) => PropForm::Other(text),
        _ if node.is_expression() => PropForm::Other(text),
        _ => PropForm::Value(text),
    }
}

fn key_of(doc: &Doc, node: &Node) -> Option<KeyOutline> {
    let items = node.items()?;
    if items.len() < 2 {
        return None;
    }
    let mut ease = None;
    let mut pin = false;
    for extra in &items[2..] {
        match extra.name() {
            Some("pin") => pin = true,
            Some("linear") => {}
            _ => ease = Some(doc.text(extra).to_string()),
        }
    }
    Some(KeyOutline { t: doc.resolve(&items[0]).number(), t_text: doc.text(&items[0]).into(), value: doc.text(&items[1]).into(), ease, pin })
}

/// An ease as written (`@ease_out`, `spring(170, 26)`, `wiggle(@ease_out, 3)`), as the model's.
pub fn ease_of(text: Option<&str>) -> Option<crate::Ease> {
    let Some(text) = text else { return Some(crate::Ease::Linear) };
    ease_node(&parse_value(text).ok()?)
}

fn ease_node(node: &Node) -> Option<crate::Ease> {
    if let Kind::Name(name) = &node.kind {
        return crate::Ease::from_id(name);
    }
    match node.as_call() {
        Some(("spring", args)) => {
            let k = args.first().and_then(Node::number).unwrap_or(170.0);
            let d = args.get(1).and_then(Node::number).unwrap_or(26.0);
            Some(crate::Ease::Spring { k, d })
        }
        Some(("bezier", args)) if args.len() == 4 => {
            let n = |i: usize| args[i].number();
            Some(crate::Ease::Bezier { x1: n(0)?, y1: n(1)?, x2: n(2)?, y2: n(3)? })
        }
        Some((call, args)) => {
            let shape = makepad_tween::named::Shape::from_id(call)?;
            let base = match args.first() {
                Some(of) => ease_node(of)?.as_base()?,
                None => makepad_tween::named::Base::Linear,
            };
            let (da, db) = shape.defaults();
            let a = args.get(1).and_then(Node::number).unwrap_or(da);
            let b = args.get(2).and_then(Node::number).unwrap_or(db);
            Some(crate::Ease::Shaped(makepad_tween::named::Shaped { shape, base, a, b }))
        }
        _ => None,
    }
}

/// A plain value's numbers as written: `120`, `vec2(10, 20)`, `[1, 2]`.
pub fn literal_numbers(text: &str) -> Option<Vec<f64>> {
    numbers(&parse_value(text).ok()?)
}

/// A colour as written (`#rgb`, `#rrggbb`, `#rrggbbaa`, `rgba(r, g, b, a)`),
/// as 0..1 RGBA.
pub fn literal_color(text: &str) -> Option<[f64; 4]> {
    let node = parse_value(text).ok()?;
    if let Kind::Color(hex) = &node.kind {
        return makepad_script::colorhex::hex_bytes_to_u32(hex.as_bytes()).ok().map(crate::load::unpack);
    }
    match node.as_call() {
        Some(("rgba", args)) if args.len() >= 3 => {
            let v: Vec<f64> = args.iter().map(Node::number).collect::<Option<_>>()?;
            Some([v[0], v[1], v[2], v.get(3).copied().unwrap_or(1.0)])
        }
        _ => None,
    }
}

// ---- writing -----------------------------------------------------------

/// A number as the editor writes it: shortest form, no trailing zeros.
pub fn fmt_num(value: f64) -> String {
    if !value.is_finite() {
        return "0".into();
    }
    let rounded = (value * 10_000.0).round() / 10_000.0;
    let mut text = format!("{rounded:.4}");
    while text.contains('.') && (text.ends_with('0') || text.ends_with('.')) {
        text.pop();
    }
    if text == "-0" {
        text = "0".into();
    }
    text
}

/// A time as the editor writes it (rounded to 0.1 ms).
pub fn fmt_time(t: f64) -> String {
    fmt_num((t / TIME_STEP).round() * TIME_STEP)
}

/// Numbers written the way `like` wrote its vector (`vec2(…)`, `[…]`), or
/// plainly for one number.
pub fn fmt_values(values: &[f64], like: Option<&str>) -> String {
    if values.len() == 1 {
        return fmt_num(values[0]);
    }
    let parts = values.iter().map(|value| fmt_num(*value)).collect::<Vec<_>>().join(", ");
    match like.map(str::trim_start) {
        Some(text) if text.starts_with('[') => format!("[{parts}]"),
        Some(text) if text.starts_with("vec3") => format!("vec3({parts})"),
        _ if values.len() == 3 => format!("vec3({parts})"),
        _ => format!("vec2({parts})"),
    }
}

/// A colour as `#rrggbb` (or `#rrggbbaa` when not opaque), from 0..1 RGBA.
pub fn fmt_color(rgba: [f64; 4]) -> String {
    let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b, a] = rgba.map(byte);
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

/// A string literal.
pub fn fmt_text(text: &str) -> String {
    quote(text)
}

/// The byte replacements of one edit, applied together to the parsed
/// document (the result is parsed again before it is returned).
struct Patch {
    edits: Vec<edit::Replace>,
}

impl Patch {
    fn new() -> Self {
        Self { edits: Vec::new() }
    }

    fn replace(&mut self, range: Range<usize>, text: impl Into<String>) {
        self.edits.push(edit::Replace::new(range, text));
    }

    fn extend(&mut self, edits: Vec<edit::Replace>) {
        self.edits.extend(edits);
    }

    fn apply(self, syntax: &Syntax) -> Result<String, String> {
        syntax.apply(&self.edits).map(|next| next.source().to_string()).map_err(|error| error.message)
    }
}

/// Insert `name: value` as the object's last field, in its layout (see
/// [`edit::add_field_edits`]).
fn insert_field(patch: &mut Patch, source: &str, object: &Node, name: &str, value: &str) -> Result<(), String> {
    patch.extend(edit::add_field_edits(source, object, edit::At::End, name, value).map_err(|error| error.message)?);
    Ok(())
}

/// Where a property's value is to be written: its own field, or the `let`
/// its name refers to.
fn target<'d>(doc: &'d Doc, layer: &'d Node, prop: &str) -> Option<&'d Node> {
    let field = layer.field(prop)?;
    Some(match &field.value.kind {
        Kind::Ident(name) if doc.let_value(name).is_some_and(|value| !matches!(value.kind, Kind::Object { .. })) => doc.resolve(&field.value),
        _ => &field.value,
    })
}

/// Set a property to a plain value (`value` as Splash text). An `in(@x)`
/// property changes the input's default instead.
pub fn set_value(source: &str, layer: &str, prop: &str, value: &str) -> Result<String, String> {
    parse_value(value).map_err(|error| format!("`{value}` is not a value: {}", error.message))?;
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let mut patch = Patch::new();
    match target(&doc, node, prop) {
        Some(existing) => {
            if let Some(("in", args)) = existing.as_call() {
                if let Some(input) = args.first().and_then(Node::name) {
                    return set_input(source, input, value);
                }
            }
            patch.replace(existing.span.clone(), value);
        }
        None => insert_field(&mut patch, source, node, prop, value)?,
    }
    patch.apply(&doc.syntax)
}

/// Set an input's default (`inputs: {name: value}`); `value` as Splash text.
/// An `input(@type, default, doc)` declaration keeps its type and doc.
pub fn set_input(source: &str, input: &str, value: &str) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let motion = doc.motion()?;
    let inputs = motion.field("inputs").map(|field| doc.resolve(&field.value)).ok_or("the document has no `inputs`")?;
    let field = inputs.field(input).ok_or_else(|| format!("there is no input `{input}`"))?;
    let mut patch = Patch::new();
    match (&field.value.kind, field.value.as_call()) {
        (Kind::Call { open, close, .. }, Some(("input", args))) => match args.get(1) {
            Some(default) => patch.replace(default.span.clone(), value),
            None => match args.first() {
                Some(ty) => patch.replace(ty.span.end..*close, format!(", {value}")),
                None => patch.replace(open + 1..*close, value),
            },
        },
        _ => patch.replace(field.value.span.clone(), value),
    }
    patch.apply(&doc.syntax)
}

/// Set (`Some`) or remove (`None`) a layer field: `name`, `visible`,
/// `locked`, `span`, or any property.
pub fn set_field(source: &str, layer: &str, name: &str, value: Option<&str>) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let mut patch = Patch::new();
    match (node.field(name), value) {
        (Some(field), Some(value)) => patch.replace(field.value.span.clone(), value),
        (None, Some(value)) => insert_field(&mut patch, source, node, name, value)?,
        (Some(_), None) => patch.extend(edit::remove_field_edits(source, node, name).map_err(|error| error.message)?),
        (None, None) => return Ok(source.to_string()),
    }
    patch.apply(&doc.syntax)
}

/// A property's keys, whatever it is now: keys and tweens as written, a
/// plain value as no keys. Errors on forms that are not keys.
fn keys_now(doc: &Doc, node: &Node, prop: &str) -> Result<(Option<Node>, Vec<KeyOutline>, bool), String> {
    match target(doc, node, prop) {
        None => Ok((None, Vec::new(), false)),
        Some(value) => match form_of(doc, value) {
            PropForm::Keys { keys, tween } => Ok((Some(value.clone()), keys, tween)),
            PropForm::Value(_) | PropForm::Input(_) => Ok((Some(value.clone()), Vec::new(), false)),
            PropForm::Other(text) => Err(format!("`{prop}` is `{}`; set a value or a key to replace it", syntax_head(&text))),
        },
    }
}

fn syntax_head(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() > 40 { format!("{}…", line.chars().take(40).collect::<String>()) } else { line.to_string() }
}

/// The indentation of the line holding `at`.
fn indent_at(source: &str, at: usize) -> &str {
    let line_start = source[..at].rfind('\n').map_or(0, |nl| nl + 1);
    let line = &source[line_start..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Write `keys` as the property's value: a tween when the property was a
/// tween and still has exactly two keys (the first without an ease), else
/// `keys(...)` laid out the way the old keys were (one per line when they
/// were).
fn write_keys(patch: &mut Patch, source: &str, layer: &Node, prop: &str, old: Option<&Node>, keys: &[KeyOutline], was_tween: bool) -> Result<(), String> {
    let text = if was_tween && keys.len() == 2 && keys[0].ease.is_none() && !keys.iter().any(|key| key.pin) {
        let ease = keys[1].ease.as_deref().map(|ease| format!(", {ease}")).unwrap_or_default();
        format!("tween({}, {}, {}, {}{ease})", keys[0].value, keys[1].value, keys[0].t_text, keys[1].t_text)
    } else {
        let one = |key: &KeyOutline| {
            let mut text = format!("[{}, {}", key.t_text, key.value);
            if let Some(ease) = &key.ease {
                text.push_str(", ");
                text.push_str(ease);
            }
            if key.pin {
                text.push_str(", @pin");
            }
            text.push(']');
            text
        };
        let multi_line = old.is_some_and(|old| old.call("keys").is_some() && source[old.span.clone()].contains('\n'));
        if multi_line {
            let old = old.unwrap();
            let args = old.call("keys").unwrap();
            let indent = args.first().map(|arg| indent_at(source, arg.span.start).to_string()).unwrap_or_default();
            let close_indent = indent_at(source, old.span.end.saturating_sub(1)).to_string();
            format!("keys(\n{}\n{close_indent})", keys.iter().map(|key| format!("{indent}{}", one(key))).collect::<Vec<_>>().join(",\n"))
        } else {
            format!("keys({})", keys.iter().map(one).collect::<Vec<_>>().join(", "))
        }
    };
    match old {
        Some(old) => patch.replace(old.span.clone(), text),
        None => insert_field(patch, source, layer, prop, &text)?,
    }
    Ok(())
}

/// Keys sorted by time (keys whose time is not a plain number keep their
/// place relative to their neighbours).
fn sort_keys(keys: &mut [KeyOutline]) {
    keys.sort_by(|a, b| match (a.t, b.t) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        _ => std::cmp::Ordering::Equal,
    });
}

/// Add a key at `t` (the layer's clock) with `value`, or re-value the key
/// already within `tolerance` of it. A plain value becomes `keys(...)`
/// with this one key.
pub fn set_key(source: &str, layer: &str, prop: &str, t: f64, value: &str, tolerance: f64) -> Result<String, String> {
    parse_value(value).map_err(|error| format!("`{value}` is not a value: {}", error.message))?;
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let (old, mut keys, tween) = keys_now(&doc, node, prop)?;
    match keys.iter_mut().find(|key| key.t.is_some_and(|kt| (kt - t).abs() <= tolerance)) {
        Some(key) => key.value = value.to_string(),
        None => {
            // A new key takes the ease of the segment it splits.
            let ease = keys.iter().find(|key| key.t.is_some_and(|kt| kt > t)).and_then(|key| key.ease.clone());
            keys.push(KeyOutline { t: Some(t), t_text: fmt_time(t), value: value.to_string(), ease, pin: false });
            sort_keys(&mut keys);
        }
    }
    let mut patch = Patch::new();
    write_keys(&mut patch, source, node, prop, old.as_ref(), &keys, tween)?;
    patch.apply(&doc.syntax)
}

/// Remove key `index`. The last key's value stays as the plain value.
pub fn delete_key(source: &str, layer: &str, prop: &str, index: usize) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let (old, mut keys, tween) = keys_now(&doc, node, prop)?;
    if index >= keys.len() {
        return Err(format!("`{prop}` has no key {index}"));
    }
    let removed = keys.remove(index);
    let mut patch = Patch::new();
    if keys.is_empty() {
        patch.replace(old.expect("a key was written").span.clone(), removed.value);
    } else {
        // The segment after the removed key keeps its own ease; a new first
        // key has no segment arriving at it.
        if index == 0 {
            if let Some(first) = keys.first_mut() {
                first.ease = None;
            }
        }
        write_keys(&mut patch, source, node, prop, old.as_ref(), &keys, tween)?;
    }
    patch.apply(&doc.syntax)
}

/// Move keys in time: `moves` are `(key index, new time)`. Keys that pass
/// each other are written in time order.
pub fn move_keys(source: &str, layer: &str, prop: &str, moves: &[(usize, f64)]) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let (old, mut keys, tween) = keys_now(&doc, node, prop)?;
    for (index, t) in moves {
        let key = keys.get_mut(*index).ok_or_else(|| format!("`{prop}` has no key {index}"))?;
        key.t = Some(t.max(0.0));
        key.t_text = fmt_time(t.max(0.0));
    }
    sort_keys(&mut keys);
    for pair in keys.windows(2) {
        if let (Some(a), Some(b)) = (pair[0].t, pair[1].t) {
            if (b - a).abs() < TIME_STEP * 0.5 {
                return Err(format!("two keys of `{prop}` would be at {}", fmt_time(a)));
            }
        }
    }
    if keys.first().is_some_and(|key| key.ease.is_some()) {
        // The first key has no segment arriving at it; a key moved to the
        // front keeps its ease for when it moves back… but it is not written.
        keys[0].ease = None;
    }
    let mut patch = Patch::new();
    write_keys(&mut patch, source, node, prop, old.as_ref(), &keys, tween)?;
    patch.apply(&doc.syntax)
}

/// Set the ease of the segment arriving at key `index` (`@ease_out`,
/// `spring(170, 26)`; `None` or `@linear` for linear).
pub fn set_ease(source: &str, layer: &str, prop: &str, index: usize, ease: Option<&str>) -> Result<String, String> {
    if let Some(ease) = ease {
        parse_value(ease).map_err(|error| format!("`{ease}` is not an ease: {}", error.message))?;
    }
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let (old, mut keys, tween) = keys_now(&doc, node, prop)?;
    if index == 0 || index >= keys.len() {
        return Err(format!("`{prop}` has no segment arriving at key {index}"));
    }
    keys[index].ease = ease.filter(|ease| *ease != "@linear").map(str::to_string);
    let mut patch = Patch::new();
    write_keys(&mut patch, source, node, prop, old.as_ref(), &keys, tween)?;
    patch.apply(&doc.syntax)
}

/// Every numeric key time of a layer's own properties shifted by `delta`
/// (for a group, its children's spans too: they are on its clock).
fn shift_layer_keys(patch: &mut Patch, doc: &Doc, node: &Node, delta: f64) {
    let Some((_, fields)) = node.object() else { return };
    for field in fields {
        let Some(name) = field.name.as_deref() else { continue };
        let value = doc.resolve(&field.value);
        match name {
            "span" | "name" | "visible" | "locked" | "mask" => {}
            "layers" => {
                for child in value.items().unwrap_or(&[]) {
                    let child = doc.resolve(child);
                    match child.field("span").map(|span| doc.resolve(&span.value)) {
                        Some(span) => {
                            if let Some(first) = span.items().and_then(|items| items.first()).filter(|first| first.number().is_some()) {
                                patch.replace(first.span.clone(), fmt_time(first.number().unwrap() + delta));
                            }
                        }
                        // Without a span a child runs on its group's clock.
                        None => shift_layer_keys(patch, doc, child, delta),
                    }
                }
            }
            _ => match value.as_call() {
                Some(("keys", args)) => {
                    for key in args {
                        if let Some(t) = key.items().and_then(|items| items.first()).filter(|t| t.number().is_some()) {
                            patch.replace(t.span.clone(), fmt_time(t.number().unwrap() + delta));
                        }
                    }
                }
                Some(("tween", args)) if args.len() >= 4 => {
                    for t in &args[2..4] {
                        if let Some(value) = t.number() {
                            patch.replace(t.span.clone(), fmt_time(value + delta));
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// Give a layer the span `[start, length]` (on its parent's clock). Its keys
/// ride along with its start (they are on its own clock) unless `keep_keys`:
/// then they stay where they were on the parent's clock (a trim).
pub fn set_span(source: &str, layer: &str, start: f64, length: f64, keep_keys: bool) -> Result<String, String> {
    if length <= 0.0 {
        return Err("a span has a positive length".into());
    }
    let doc = Doc::new(source)?;
    let node = doc.layer(layer)?;
    let old_start = match node.field("span").map(|field| doc.resolve(&field.value)) {
        Some(span) => numbers(span).filter(|v| v.len() == 2).map(|v| v[0]).ok_or("`span` is not written as plain seconds; edit it in the source")?,
        None => 0.0,
    };
    let start = start.max(0.0);
    let mut patch = Patch::new();
    let text = format!("[{}, {}]", fmt_time(start), fmt_time(length));
    match node.field("span") {
        Some(field) => match &field.value.kind {
            Kind::Array { items, .. } if items.len() == 2 => {
                patch.replace(items[0].span.clone(), fmt_time(start));
                patch.replace(items[1].span.clone(), fmt_time(length));
            }
            _ => patch.replace(field.value.span.clone(), text),
        },
        None => insert_field(&mut patch, source, node, "span", &text)?,
    }
    let delta = old_start - start;
    if keep_keys && delta.abs() > TIME_STEP * 0.5 {
        shift_layer_keys(&mut patch, &doc, node, delta);
    }
    patch.apply(&doc.syntax)
}

/// The list a layer path is in and its index there.
fn parent_of(path: &str) -> Result<(&str, usize), String> {
    let open = path.rfind('[').ok_or_else(|| format!("`{path}` is not in a layer list"))?;
    let index = path[open + 1..].trim_end_matches(']').parse::<usize>().map_err(|_| format!("`{path}` is not a layer path"))?;
    Ok((&path[..open], index))
}

/// The list node (`layers: [...]`) named by a list path (`layers`,
/// `layers[2].layers`).
fn list_at<'d>(doc: &'d Doc, list: &str) -> Result<&'d Node, String> {
    let (owner, field) = match list.rfind('.') {
        Some(dot) => (Some(&list[..dot]), &list[dot + 1..]),
        None => (None, list),
    };
    let node = match owner {
        Some(owner) => doc.layer(owner)?,
        None => doc.motion()?,
    };
    let value = node.field(field).map(|field| doc.resolve(&field.value)).ok_or_else(|| format!("no `{list}`"))?;
    value.items().map(|_| value).ok_or_else(|| format!("`{list}` is not a list"))
}

/// Move a layer to `to` in its own list (0 is the bottom, drawn first).
pub fn move_layer(source: &str, layer: &str, to: usize) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let (list_path, from) = parent_of(layer)?;
    let list = list_at(&doc, list_path)?;
    let items = list.items().unwrap_or(&[]);
    let to = to.min(items.len().saturating_sub(1));
    if from == to || from >= items.len() {
        return Ok(source.to_string());
    }
    let mut patch = Patch::new();
    patch.extend(edit::move_element_edits(source, list, from, to).map_err(|error| error.message)?);
    patch.apply(&doc.syntax)
}

/// Remove a layer.
pub fn remove_layer(source: &str, layer: &str) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let (list_path, index) = parent_of(layer)?;
    let list = list_at(&doc, list_path)?;
    if index >= list.items().map_or(0, <[Node]>::len) {
        return Err(format!("there is no `{layer}`"));
    }
    let mut patch = Patch::new();
    patch.extend(edit::remove_element_edits(source, list, index).map_err(|error| error.message)?);
    patch.apply(&doc.syntax)
}

/// Put a copy of a layer right above it, named `name` when given.
pub fn duplicate_layer(source: &str, layer: &str, name: Option<&str>) -> Result<String, String> {
    let doc = Doc::new(source)?;
    let (list_path, index) = parent_of(layer)?;
    let list = list_at(&doc, list_path)?;
    if index >= list.items().map_or(0, <[Node]>::len) {
        return Err(format!("there is no `{layer}`"));
    }
    let mut patch = Patch::new();
    let transform = |text: &str| -> Result<String, edit::EditError> {
        match name {
            Some(name) => rename_in(text, name).map_err(|message| edit::EditError { message }),
            None => Ok(text.to_string()),
        }
    };
    patch.extend(edit::duplicate_element_edits(source, list, index, &transform).map_err(|error| error.message)?);
    patch.apply(&doc.syntax)
}

/// A layer constructor's text with its `name` set.
fn rename_in(text: &str, name: &str) -> Result<String, String> {
    let syntax = syntax::parse(text).map_err(|error| error.message)?;
    let node = syntax.statements().first().ok_or("a layer constructor was expected")?;
    let mut patch = Patch::new();
    match node.field("name") {
        Some(field) => patch.replace(field.value.span.clone(), quote(name)),
        None => insert_field(&mut patch, text, node, "name", &quote(name))?,
    }
    patch.apply(&syntax)
}

/// Add a layer (its constructor text) at the top of a list (`layers`, or a
/// group's `layers[2].layers`).
pub fn add_layer(source: &str, list: &str, layer_text: &str) -> Result<String, String> {
    parse_value(layer_text).map_err(|error| format!("the new layer is not a value: {}", error.message))?;
    let doc = Doc::new(source)?;
    let list = list_at(&doc, list)?;
    let mut patch = Patch::new();
    patch.extend(edit::insert_element_edits(source, list, edit::At::End, layer_text).map_err(|error| error.message)?);
    patch.apply(&doc.syntax)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"use mod.motion.*
// A title card.
let accent = #ff5a36
Motion{
    size: vec2(1920, 1080) duration: 6.0
    inputs: {headline: "Notes, instantly" accent: in_accent}
    layers: [
        Rect{at: vec2(0, 0) size: vec2(1920, 1080) fill: #0b0d12}
        // The headline.
        Text{text: in(@headline) size: 120 at: vec2(160, 460)
             opacity: tween(0, 1, 0.0, 0.4, @ease_out)
             y: keys([0, 40], [0.6, 0, @ease_out_back])}
        Path{svg: "M0 0 L400 0" stroke: accent width: 8 span: [0.5, 5.0]
             trim_end: tween(0, 1, 0.0, 0.6, @ease_in_out)}
    ]
}
"#;

    #[test]
    fn outlines_layers_props_and_keys() {
        let outline = outline(DOC).unwrap();
        assert_eq!(outline.duration, Some(6.0));
        assert_eq!(outline.layers.len(), 3);
        let text = &outline.layers[1];
        assert_eq!(text.path, "layers[1]");
        assert_eq!(text.ty, "Text");
        assert_eq!(text.prop("text").unwrap().form, PropForm::Input("headline".into()));
        let opacity = text.prop("opacity").unwrap();
        assert!(matches!(&opacity.form, PropForm::Keys { tween: true, keys } if keys.len() == 2 && keys[1].ease.as_deref() == Some("@ease_out")));
        let y = text.prop("y").unwrap().form.keys();
        assert_eq!(y.len(), 2);
        assert_eq!(y[1].t, Some(0.6));
        assert_eq!(outline.layers[2].span, Some((0.5, 5.0)));
        // `stroke: accent` reads the let.
        assert_eq!(outline.layers[2].prop("stroke").unwrap().form, PropForm::Value("#ff5a36".into()));
    }

    #[test]
    fn a_value_changes_only_its_bytes() {
        let next = set_value(DOC, "layers[1]", "size", "96").unwrap();
        assert_eq!(next, DOC.replace("size: 120", "size: 96"));
        // A let is changed where it is defined.
        let next = set_value(DOC, "layers[2]", "stroke", "#00ff00").unwrap();
        assert_eq!(next, DOC.replace("let accent = #ff5a36", "let accent = #00ff00"));
        // An input's default, for an `in(@x)` property.
        let next = set_value(DOC, "layers[1]", "text", "\"Hello\"").unwrap();
        assert_eq!(next, DOC.replace("headline: \"Notes, instantly\"", "headline: \"Hello\""));
        // A new field goes on its own line in a multi-line layer.
        let next = set_value(DOC, "layers[1]", "rotate", "5").unwrap();
        assert!(next.contains("y: keys([0, 40], [0.6, 0, @ease_out_back])\n             rotate: 5}"), "{next}");
        // …and at the end of a one-line layer.
        let next = set_value(DOC, "layers[0]", "radius", "12").unwrap();
        assert!(next.contains("fill: #0b0d12 radius: 12}"));
    }

    #[test]
    fn keys_are_added_moved_eased_and_removed_in_place() {
        // A tween with a third key becomes keys.
        let next = set_key(DOC, "layers[1]", "opacity", 1.0, "0.5", 1e-3).unwrap();
        assert!(next.contains("opacity: keys([0.0, 0], [0.4, 1, @ease_out], [1, 0.5])"), "{next}");
        // Re-valuing a tween's end keeps the tween.
        let next = set_key(DOC, "layers[1]", "opacity", 0.4, "0.8", 1e-3).unwrap();
        assert!(next.contains("opacity: tween(0, 0.8, 0.0, 0.4, @ease_out)"), "{next}");
        // Moving a key.
        let next = move_keys(DOC, "layers[1]", "y", &[(1, 0.75)]).unwrap();
        assert!(next.contains("y: keys([0, 40], [0.75, 0, @ease_out_back])"), "{next}");
        // Easing a segment.
        let next = set_ease(DOC, "layers[1]", "y", 1, Some("spring(170, 26)")).unwrap();
        assert!(next.contains("[0.6, 0, spring(170, 26)]"), "{next}");
        let next = set_ease(DOC, "layers[1]", "opacity", 1, None).unwrap();
        assert!(next.contains("opacity: tween(0, 1, 0.0, 0.4)"), "{next}");
        // A plain value takes its first key.
        let next = set_key(DOC, "layers[1]", "size", 2.0, "140", 1e-3).unwrap();
        assert!(next.contains("size: keys([2, 140])"), "{next}");
        // Deleting down to nothing leaves the last value.
        let one = delete_key(DOC, "layers[1]", "y", 0).unwrap();
        assert!(one.contains("y: keys([0.6, 0])"), "{one}");
        let none = delete_key(&one, "layers[1]", "y", 0).unwrap();
        assert!(none.contains("y: 0}"), "{none}");
        // Every result still outlines.
        for source in [&one, &none] {
            outline(source).unwrap();
        }
    }

    #[test]
    fn spans_move_and_trim() {
        let moved = set_span(DOC, "layers[2]", 1.0, 5.0, false).unwrap();
        assert!(moved.contains("span: [1, 5]"));
        assert!(moved.contains("trim_end: tween(0, 1, 0.0, 0.6, @ease_in_out)"));
        // A trim keeps the keys where they were on the parent's clock.
        let trimmed = set_span(DOC, "layers[2]", 0.75, 4.75, true).unwrap();
        assert!(trimmed.contains("span: [0.75, 4.75]"), "{trimmed}");
        assert!(trimmed.contains("trim_end: tween(0, 1, -0.25, 0.35, @ease_in_out)"), "{trimmed}");
        // A layer without a span gets one.
        let spanned = set_span(DOC, "layers[0]", 0.0, 2.0, false).unwrap();
        assert!(spanned.contains("fill: #0b0d12 span: [0, 2]}"), "{spanned}");
    }

    #[test]
    fn layers_reorder_with_their_comments() {
        let next = move_layer(DOC, "layers[1]", 2).unwrap();
        let outline = outline(&next).unwrap();
        assert_eq!(outline.layers.iter().map(|layer| layer.ty.as_str()).collect::<Vec<_>>(), ["Rect", "Path", "Text"]);
        assert!(next.contains("        // The headline.\n        Text{"), "{next}");
        let back = move_layer(&next, "layers[2]", 1).unwrap();
        assert_eq!(back, DOC);
        let removed = remove_layer(DOC, "layers[0]").unwrap();
        assert_eq!(super::outline(&removed).unwrap().layers.len(), 2);
        let copied = duplicate_layer(DOC, "layers[0]", Some("bg 2")).unwrap();
        let outline = super::outline(&copied).unwrap();
        assert_eq!(outline.layers.len(), 4);
        assert_eq!(outline.layers[1].name.as_deref(), Some("bg 2"));
    }

    #[test]
    fn flags_and_fields() {
        let hidden = set_field(DOC, "layers[0]", "visible", Some("false")).unwrap();
        assert!(!outline(&hidden).unwrap().layers[0].visible);
        let shown = set_field(&hidden, "layers[0]", "visible", None).unwrap();
        assert_eq!(shown, DOC);
        let added = add_layer(DOC, "layers", "Ellipse{size: vec2(40, 40) fill: #fff}").unwrap();
        assert_eq!(outline(&added).unwrap().layers.len(), 4);
    }

    /// Every edit leaves a document the model loads, and changes only what
    /// it names (an AI-written document, as generated).
    #[test]
    fn edits_keep_the_document_loading() {
        let doc = r#"use mod.motion.*
Motion{
    label: "Scene 1"
    size: vec2(1080, 1920)
    duration: 4.0
    inputs: {
        w1: "PLAY"
        accent: #ff6a2b
    }
    layers: [
        Group{name: "block" size: vec2(1080, 1920)
              opacity: keys([0.0, 1.0], [3.7, 1.0], [3.95, 0.0, @ease_in_cubic])
              layers: [
            Text{name: "w1" text: in(@w1) font: Font{family: "Inter" weight: 900} size: 170
                 color: #fff3e6 at: vec2(96, 690)
                 opacity: tween(0, 1, 0.5, 0.62, @ease_out)
                 scale: tween(1.35, 1.0, 0.5, 0.8, @ease_out_expo) anchor: vec2(0.0, 0.5)}
            Path{name: "underline" svg: "M0 0 L620 0" stroke: in(@accent) width: 14
                 at: vec2(104, 1262)
                 trim_end: tween(0, 1, 2.0, 2.6, @ease_in_out_cubic)}
        ]}
    ]
}
"#;
        crate::MotionDoc::load(doc).unwrap();
        let edits: Vec<Result<String, String>> = vec![
            set_value(doc, "layers[0].layers[0]", "size", "190"),
            set_value(doc, "layers[0].layers[0]", "text", "\"GO\""),
            set_key(doc, "layers[0].layers[0]", "opacity", 1.5, "0.5", 1e-3),
            move_keys(doc, "layers[0]", "opacity", &[(1, 3.5)]),
            set_ease(doc, "layers[0].layers[1]", "trim_end", 1, Some("spring(170, 26)")),
            set_span(doc, "layers[0].layers[1]", 1.0, 3.0, true),
            move_layer(doc, "layers[0].layers[1]", 0),
            duplicate_layer(doc, "layers[0].layers[0]", Some("w1 copy")),
            remove_layer(doc, "layers[0].layers[1]"),
            add_layer(doc, "layers[0].layers", "Rect{size: vec2(10, 10) fill: #ffffff}"),
            delete_key(doc, "layers[0]", "opacity", 2),
            set_input(doc, "accent", "#00ff00"),
        ];
        for (index, edit) in edits.into_iter().enumerate() {
            let next = edit.unwrap_or_else(|error| panic!("edit {index}: {error}"));
            if let Err(diags) = crate::MotionDoc::load(&next) {
                panic!("edit {index} broke the document: {diags:?}\n{next}");
            }
            // The header and the comments of an untouched region stay byte for byte.
            assert!(next.starts_with("use mod.motion.*\nMotion{\n    label: \"Scene 1\"\n    size: vec2(1080, 1920)"), "edit {index}");
        }
    }

    #[test]
    fn literals() {
        assert_eq!(literal_numbers("vec2(1, 2.5)"), Some(vec![1.0, 2.5]));
        assert_eq!(literal_numbers("120"), Some(vec![120.0]));
        assert_eq!(literal_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(literal_color("#f008").map(|c| (c[0], (c[3] * 255.0).round())), Some((1.0, 136.0)));
    }

    #[test]
    fn formats() {
        assert_eq!(fmt_num(1.0), "1");
        assert_eq!(fmt_num(0.123456), "0.1235");
        assert_eq!(fmt_num(-0.0), "0");
        assert_eq!(fmt_values(&[1.0, 2.5], Some("[0, 0]")), "[1, 2.5]");
        assert_eq!(fmt_values(&[1.0, 2.5], Some("vec2(0, 0)")), "vec2(1, 2.5)");
        assert_eq!(fmt_color([1.0, 0.0, 0.0, 1.0]), "#ff0000");
        assert_eq!(fmt_color([1.0, 0.0, 0.0, 0.5]), "#ff000080");
    }
}
