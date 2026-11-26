//! A Motion document's structure, read from its code (STRUCTURE.md): the
//! scene, its post stack, its layers and what is attached to each (a
//! layer's `effects:`, a Group's or Scene3D's `post:`), as a tree of nodes
//! an editor shows, selects and edits.
//!
//! Nothing here is a scene format. The tree is read from ordinary code each
//! time it changes: a node is a component written in the document
//! (`Group{…}`, `Bloom{…}`, `Effect{name: @glow …}`) or a call that makes
//! them (`film_post({…})`, a generator); anything else is a code node that
//! jumps to its source. Every node and parameter is addressed by a path the
//! [`crate::paths`] resolver reads (`Motion.post[1].strength`,
//! `Motion.layers[0].layers[3].effects[0].radius`), so an editor writes
//! through the same text edits as the scene's panel, one undo step each.
//!
//! Nothing is evaluated: the code is read.

use makepad_edit_values::ValueKind;
use crate::paths::{self, call_args, read_at, V};
use std::ops::Range;

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// The document's `Motion{…}`.
    Scene,
    /// A layer component (`Group`, `Text`, `Draw2D`, `Scene3D`, …).
    Layer,
    /// An entry of a `post:` list: a kit (`Bloom{…}`) or a `Pass{…}`.
    Post,
    /// An entry of a layer's `effects:` list (`Effect{name: @glow …}`).
    Effect,
    /// A call that makes several of the above (`film_post({…})`).
    Generator,
    /// Something else (a name, an expression): shown, opened as code.
    Code,
}

/// One node of the structure.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: NodeKind,
    /// The path its value is at (`Motion.layers[0]`).
    pub path: String,
    /// Its type or callee (`Group`, `Bloom`, `film_post`, `glow` for an
    /// Effect).
    pub ty: String,
    /// What to call it: its `name:`, else the comment line above it, else
    /// its type.
    pub label: String,
    /// The `//` lines written right above it.
    pub comment: Option<String>,
    /// Its bytes in the document.
    pub range: Range<usize>,
    /// `false` when written `on: false` (bypassed).
    pub on: bool,
    /// Child layers (`layers`, `mask`, `inside`).
    pub children: Vec<Node>,
    /// The lists of effects attached to it (`post`, `effects`), written or
    /// not.
    pub stacks: Vec<Stack>,
}

/// An attached list: a node's `post:` or `effects:`.
#[derive(Clone, Debug, PartialEq)]
pub struct Stack {
    /// `post` or `effects`.
    pub field: &'static str,
    /// The list's path (`Motion.post`, `Motion.layers[2].effects`).
    pub path: String,
    /// The list's bytes, when written.
    pub range: Option<Range<usize>>,
    /// Written as a call (`post: film_post({…})`), not a list.
    pub generated: bool,
    pub items: Vec<Node>,
}

/// How a parameter's value is written: what an editor offers for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A literal (number, vector, colour, …): set and animated in place.
    Literal,
    /// `keys(…)`, `tween(…)`, `sequence(…)`, `Keys{…}`, `Tween{…}`: a curve.
    Curve,
    /// `modulate(base, @lfo, {…})`.
    Modulated,
    /// A name of another value (`look.KNEE`): written where it is defined.
    Name,
    /// Code (`fn(t) {…}`, arithmetic, a call): shown, opened as code.
    Expression,
    /// Not written: the definition's default.
    Unwritten,
}

/// One parameter of a node.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    /// Its path (`Motion.post[1].strength`).
    pub path: String,
    /// As written, when it is.
    pub written: Option<String>,
    /// The definition's default, when known.
    pub default: Option<String>,
    pub form: Form,
    /// What kind of value it is (written, else the definition's).
    pub kind: ValueKind,
    /// A line saying what it does, when the definition has one.
    pub doc: Option<String>,
}

impl Param {
    /// Whether "animate" is offered: a literal number, vector or colour, or
    /// a curve already.
    pub fn animatable(&self) -> bool {
        match self.form {
            Form::Curve => true,
            Form::Literal | Form::Unwritten => matches!(self.kind, ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length | ValueKind::Colour) || self.written.as_deref().is_some_and(is_vector),
            _ => false,
        }
    }
}

fn is_vector(t: &str) -> bool {
    let t = t.trim();
    (t.starts_with("vec2(") || t.starts_with("vec3(") || t.starts_with("vec4(")) && t[5..].chars().all(|c| c.is_ascii_digit() || " .,-)".contains(c))
}

/// Fields that hold a node's children or stacks, not parameters.
const STRUCTURAL: &[&str] = &["layers", "post", "effects", "mask", "inside", "children", "on", "name"];

/// Where a document's top-level `Motion{…}` starts: the last line that
/// begins with it at the margin.
pub fn scene_at(text: &str) -> Option<usize> {
    let mut found = None;
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        if line.strip_prefix("Motion").map(str::trim_start).is_some_and(|r| r.starts_with('{')) {
            found = Some(line_start);
        }
        line_start += line.len();
    }
    found
}

/// The document's structure: its scene node (with its post stack and its
/// layers as children), `None` when no `Motion{…}` is written.
pub fn read(text: &str) -> Option<Node> {
    let at = scene_at(text)?;
    let value = read_at(text, at);
    let V::Typed(fields, range) = &value else { return None };
    let label = fields.iter().find(|(k, _)| k == "label").map(|(_, v)| unquote(&text[v.range()])).unwrap_or_else(|| "scene".into());
    let mut scene = Node {
        kind: NodeKind::Scene,
        path: "Motion".into(),
        ty: "Motion".into(),
        label,
        comment: None,
        range: range.clone(),
        on: true,
        children: Vec::new(),
        stacks: Vec::new(),
    };
    scene.stacks.push(stack(text, fields, "post", "Motion"));
    if let Some((_, layers)) = fields.iter().find(|(k, _)| k == "layers") {
        scene.children = layer_list(text, layers, "Motion.layers");
    }
    Some(scene)
}

fn unquote(t: &str) -> String {
    t.trim().trim_matches('"').to_string()
}

/// The `//` lines right above byte `at` (none when a blank line or code is
/// in between).
fn comment_above(text: &str, at: usize) -> Option<String> {
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    // Something else written before it on its line: no comment of its own.
    if !text[line_start..at].trim().is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let mut end = line_start;
    while end > 0 {
        let start = text[..end - 1].rfind('\n').map_or(0, |i| i + 1);
        let line = text[start..end - 1].trim();
        match line.strip_prefix("//") {
            Some(c) if !line.starts_with("///") || true => lines.push(c.trim().to_string()),
            _ => break,
        }
        end = start;
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join(" "))
}

fn layer_list(text: &str, value: &V, path: &str) -> Vec<Node> {
    match value {
        V::Arr(items, _) => items.iter().enumerate().map(|(i, item)| node(text, item, &format!("{path}[{i}]"), NodeKind::Layer)).collect(),
        // `layers: OVERLAY`, `layers: make_rows()`: one code node.
        other => vec![node(text, other, path, NodeKind::Layer)],
    }
}

/// A node from a value written at `path`; `as_kind` what a record there is.
fn node(text: &str, value: &V, path: &str, as_kind: NodeKind) -> Node {
    let range = value.range();
    let written = text[range.clone()].trim_end();
    let comment = comment_above(text, range.start);
    match value {
        V::Typed(fields, _) => {
            let ty = written.split('{').next().unwrap_or("").trim().rsplit('.').next().unwrap_or("").to_string();
            let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| text[v.range()].trim_end().to_string());
            let on = field("on").as_deref() != Some("false");
            let (kind, ty) = match (as_kind, ty.as_str()) {
                (NodeKind::Effect, _) | (_, "Effect") => (NodeKind::Effect, field("name").map(|n| n.trim_start_matches('@').trim_matches('"').to_string()).unwrap_or(ty)),
                (k, _) => (k, ty),
            };
            let label = match kind {
                NodeKind::Layer => field("name").map(|n| unquote(&n)).or_else(|| comment.clone()).unwrap_or_else(|| ty.clone()),
                _ => ty.clone(),
            };
            let mut n = Node { kind, path: path.to_string(), ty, label, comment, range: range.start..range.start + written.len(), on, children: Vec::new(), stacks: Vec::new() };
            if kind == NodeKind::Layer {
                if let Some((_, layers)) = fields.iter().find(|(k, _)| k == "layers") {
                    n.children = layer_list(text, layers, &format!("{path}.layers"));
                }
                for one in ["mask", "inside"] {
                    if let Some((_, v)) = fields.iter().find(|(k, _)| k == one) {
                        n.children.push(node(text, v, &format!("{path}.{one}"), NodeKind::Layer));
                    }
                }
                n.stacks.push(stack(text, fields, "effects", path));
                if matches!(n.ty.as_str(), "Group" | "Scene3D") {
                    n.stacks.push(stack(text, fields, "post", path));
                }
            }
            n
        }
        V::Atom(_) => match call_args(text, value) {
            Some((callee, _)) => Node {
                kind: NodeKind::Generator,
                path: path.to_string(),
                label: callee.rsplit('.').next().unwrap_or(&callee).to_string(),
                ty: callee,
                comment,
                range: range.start..range.start + written.len(),
                on: true,
                children: Vec::new(),
                stacks: Vec::new(),
            },
            None => code_node(written, path, comment, range.start),
        },
        _ => code_node(written, path, comment, range.start),
    }
}

fn code_node(written: &str, path: &str, comment: Option<String>, start: usize) -> Node {
    let short: String = written.chars().take(32).collect();
    Node { kind: NodeKind::Code, path: path.to_string(), ty: String::new(), label: comment.clone().unwrap_or(short), comment, range: start..start + written.len(), on: true, children: Vec::new(), stacks: Vec::new() }
}

/// A node's `field` list (`post`, `effects`) as a stack, written or not.
fn stack(text: &str, fields: &[(String, V)], field: &'static str, owner: &str) -> Stack {
    let path = format!("{owner}.{field}");
    let item_kind = if field == "effects" { NodeKind::Effect } else { NodeKind::Post };
    match fields.iter().find(|(k, _)| k == field) {
        None => Stack { field, path, range: None, generated: false, items: Vec::new() },
        Some((_, V::Arr(items, range))) => {
            Stack { field, items: items.iter().enumerate().map(|(i, v)| node(text, v, &format!("{path}[{i}]"), item_kind)).collect(), path, range: Some(range.clone()), generated: false }
        }
        Some((_, v)) => {
            let item = node(text, v, &path, item_kind);
            Stack { field, generated: item.kind == NodeKind::Generator, range: Some(item.range.clone()), path, items: vec![item] }
        }
    }
}

/// Every node, depth first (the scene first, then its post stack, then each
/// layer and its stacks).
pub fn flatten(root: &Node) -> Vec<&Node> {
    fn go<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
        out.push(n);
        for s in &n.stacks {
            for item in &s.items {
                go(item, out);
            }
        }
        for c in &n.children {
            go(c, out);
        }
    }
    let mut out = Vec::new();
    go(root, &mut out);
    out
}

/// The node at `path`.
pub fn find<'a>(root: &'a Node, path: &str) -> Option<&'a Node> {
    flatten(root).into_iter().find(|n| n.path == path)
}

/// The innermost node whose bytes hold `at` (a click on the code, a pick's
/// call site).
pub fn at_byte(root: &Node, at: usize) -> Option<&Node> {
    flatten(root).into_iter().filter(|n| n.range.contains(&at)).min_by_key(|n| n.range.len())
}

/// The layer node a runtime layer path names (`layers[0].layers[3]`, the
/// model's [`crate::Layer::path`]; `layers[1].mask`).
pub fn layer<'a>(root: &'a Node, model_path: &str) -> Option<&'a Node> {
    let want = format!("Motion.{model_path}");
    // A deeper runtime path (an item inside a generated list) falls back to
    // the nearest node written.
    let mut path = want.as_str();
    loop {
        if let Some(n) = find(root, path) {
            return Some(n);
        }
        let cut = path.rfind(['.', '['])?;
        path = &path[..cut];
    }
}

/// The parameters of `node`: what is written (in order), then what its
/// definition has that is not written (with its default).
pub fn params(text: &str, node: &Node) -> Vec<Param> {
    let mut out = Vec::new();
    let value = read_at(text, node.range.start);
    match node.kind {
        NodeKind::Generator => {
            // Its options record's fields (the last record argument).
            if let Some((_, args)) = call_args(text, &value) {
                match args.iter().rev().find(|a| matches!(a, V::Obj(..))) {
                    Some(V::Obj(fields, _)) => {
                        for (name, v) in fields {
                            out.push(written_param(text, name, &format!("{}.{name}", node.path), v));
                        }
                    }
                    // `film_post(POST)`: the named record's fields, where it
                    // is defined (`POST.bloom`).
                    _ => {
                        if let Some(name) = args.last().map(|a| text[a.range()].trim().to_string()).filter(|n| !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')) {
                            out.extend(named_record_params(text, &name));
                        }
                    }
                }
            }
            return out;
        }
        NodeKind::Code => return out,
        _ => {}
    }
    let (V::Typed(fields, _) | V::Obj(fields, _)) = &value else { return out };
    for (name, v) in fields {
        if STRUCTURAL.contains(&name.as_str()) || (node.kind == NodeKind::Effect && name == "name") {
            continue;
        }
        // A Pass's uniforms: each is a parameter (`….uniforms.flash`).
        if node.ty == "Pass" && name == "uniforms" {
            if let V::Obj(us, _) = v {
                for (u, uv) in us {
                    out.push(written_param(text, u, &format!("{}.uniforms.{u}", node.path), uv));
                }
            }
            continue;
        }
        if node.ty == "Shader" && name == "params" {
            if let V::Obj(ps, _) = v {
                for (u, uv) in ps {
                    out.push(written_param(text, u, &format!("{}.params.{u}", node.path), uv));
                }
            }
            continue;
        }
        if node.ty == "Pass" && matches!(name.as_str(), "pixel" | "reads" | "at" | "helpers" | "name") {
            continue;
        }
        out.push(written_param(text, name, &format!("{}.{name}", node.path), v));
    }
    // A written field's line, where its reader has one.
    if let Some(fields) = registry_fields(node) {
        for p in out.iter_mut().filter(|p| p.doc.is_none()) {
            p.doc = fields.iter().find(|f| f.name == p.name).and_then(|f| f.doc).map(str::to_string);
        }
    }
    for (name, kind, default, doc) in definition(node) {
        if out.iter().any(|p| p.name == name) {
            continue;
        }
        out.push(Param { path: format!("{}.{name}", node.path), name, written: None, default, form: Form::Unwritten, kind, doc });
    }
    out
}

/// The fields the reader takes on a layer or the document's root (the
/// field registry), `None` for any other node.
fn registry_fields(node: &Node) -> Option<&'static [crate::field_registry::FieldInfo]> {
    use crate::field_registry::{motion_types, Family};
    let family = match node.kind {
        NodeKind::Scene => Family::Document,
        NodeKind::Layer => Family::Layer,
        _ => return None,
    };
    motion_types().iter().find(|t| t.family == family && t.name == node.ty).map(|t| t.fields.as_slice())
}

/// The range the reader keeps a layer's or the document's field in (the
/// field registry), when it keeps one.
pub fn field_range(node: &Node, field: &str) -> Option<(f64, f64)> {
    let [lo, hi] = registry_fields(node)?.iter().find(|f| f.name == field)?.range?;
    Some((lo, hi))
}

fn named_record_params(text: &str, name: &str) -> Vec<Param> {
    // Only a record written in this document (a module's is reached by the
    // editor's own path resolution, `look.POST.bloom`).
    let Some(at) = text.find(&format!("let {name} = ")).map(|i| i + 7 + name.len()) else { return Vec::new() };
    match read_at(text, at) {
        V::Obj(fields, _) => fields.iter().map(|(f, v)| written_param(text, f, &format!("{name}.{f}"), v)).collect(),
        _ => Vec::new(),
    }
}

fn written_param(text: &str, name: &str, path: &str, v: &V) -> Param {
    let written = text[v.range()].trim_end().to_string();
    let form = form_of(&written);
    let kind = paths::kind_of(&written, |_| false);
    Param { name: name.to_string(), path: path.to_string(), written: Some(written), default: None, form, kind, doc: None }
}

/// How a value written as `t` reads.
pub fn form_of(t: &str) -> Form {
    let t = t.trim();
    let head = t.split(['(', '{']).next().unwrap_or("").trim();
    match head {
        "keys" | "tween" | "sequence" | "repeat" | "Keys" | "Tween" | "Sequence" | "Loop" if t.len() > head.len() => return Form::Curve,
        "modulate" if t.len() > head.len() => return Form::Modulated,
        _ => {}
    }
    match paths::kind_of(t, |_| false) {
        ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length | ValueKind::Colour | ValueKind::Bool | ValueKind::Text | ValueKind::Choice | ValueKind::Ease | ValueKind::Font => Form::Literal,
        _ if is_vector(t) => Form::Literal,
        _ if !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && !t.chars().next().unwrap().is_ascii_digit() => Form::Name,
        _ => Form::Expression,
    }
}

/// What a node's definition has: (name, kind, default as code, doc).
fn definition(node: &Node) -> Vec<(String, ValueKind, Option<String>, Option<String>)> {
    match node.kind {
        NodeKind::Effect => {
            let Some(info) = makepad_stage_render_kit::catalog::by_name(&node.ty) else { return Vec::new() };
            info.params.iter().filter_map(|p| {
                use makepad_stage_render_kit::catalog::ParamKind as P;
                let (kind, default) = match &p.kind {
                    P::Num { default, .. } => (ValueKind::Number, Some(crate::patch::fmt_num(*default))),
                    P::Vec2 { default } => (ValueKind::Code, Some(format!("vec2({}, {})", crate::patch::fmt_num(default[0]), crate::patch::fmt_num(default[1])))),
                    P::Vec3 { default } => (ValueKind::Code, Some(format!("vec3({}, {}, {})", crate::patch::fmt_num(default[0]), crate::patch::fmt_num(default[1]), crate::patch::fmt_num(default[2])))),
                    P::Color { default } => (ValueKind::Colour, Some(crate::patch::fmt_color(default.map(|c| c as f64)))),
                    P::Bool { default } => (ValueKind::Bool, Some(default.to_string())),
                    P::Choice { default, .. } => (ValueKind::Choice, Some(format!("@{default}"))),
                    _ => return None,
                };
                Some((p.name.to_string(), kind, default, Some(p.doc.to_string()).filter(|d| !d.is_empty())))
            }).collect()
        }
        NodeKind::Post => {
            let kind = crate::value::POST_KITS.iter().copied().find(|k| makepad_render_graph::kits::kit(k).is_some_and(|kit| kit.name == node.ty));
            let Some(kit) = kind.and_then(makepad_render_graph::kits::kit) else { return Vec::new() };
            let defaults = kit_defaults(kit.name);
            kit.params.iter().map(|name| {
                let default = defaults.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
                let kind = default.as_deref().map_or(ValueKind::Number, |d| paths::kind_of(d, |_| false));
                (name.to_string(), kind, default, None)
            }).collect()
        }
        // The fields the reader falls back on a default for (the field
        // registry), added at that default.
        NodeKind::Scene | NodeKind::Layer => registry_fields(node)
            .unwrap_or_default()
            .iter()
            .filter(|f| !STRUCTURAL.contains(&f.name.as_str()))
            .filter_map(|f| {
                let default = f.default.clone()?;
                Some((f.name.clone(), paths::kind_of(&default, |_| false), Some(default), f.doc.map(str::to_string)))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A render-kit effect parameter's range (`min`, `max`), when it has one.
pub fn effect_range(effect: &str, param: &str) -> Option<(f64, f64)> {
    let info = makepad_stage_render_kit::catalog::by_name(effect)?;
    match info.param(param)?.1.kind {
        makepad_stage_render_kit::catalog::ParamKind::Num { min, max, .. } => Some((min, max)),
        _ => None,
    }
}

/// A render-kit effect parameter's choices, when it is a choice.
pub fn effect_choices(effect: &str, param: &str) -> Option<Vec<&'static str>> {
    let info = makepad_stage_render_kit::catalog::by_name(effect)?;
    match info.param(param)?.1.kind {
        makepad_stage_render_kit::catalog::ParamKind::Choice { choices, .. } => Some(choices.to_vec()),
        _ => None,
    }
}

/// A post kit's defaults as its Splash writes them (`Glow` → strength 0.6 …).
pub fn kit_defaults(type_name: &str) -> Vec<(String, String)> {
    let source = makepad_render_graph::kits::kit_source("$M", None);
    let head = format!("$M.{type_name} = $M.{type_name}");
    source.lines().find_map(|l| l.trim().strip_prefix(&head).map(str::to_string)).map(|rest| {
        let record = format!("{type_name}{}", rest.trim());
        match read_at(&record, 0) {
            V::Typed(fields, _) => fields.into_iter().map(|(k, v)| (k, record[v.range()].trim_end().to_string())).collect(),
            _ => Vec::new(),
        }
    }).unwrap_or_default()
}

/// The effects that can be added to a stack: (type or name as written,
/// category, one line). `post`: the post kits and a `Pass`; `effects`: the
/// render kit's catalogue.
pub fn catalogue(field: &str) -> Vec<(String, String, String)> {
    if field == "post" {
        let mut out: Vec<(String, String, String)> = crate::value::POST_KITS.iter().filter_map(|k| makepad_render_graph::kits::kit(k)).map(|k| (k.name.to_string(), "post".to_string(), String::new())).collect();
        out.push(("Pass".into(), "post".into(), "Your own pass: a pixel function over the frame".into()));
        return out;
    }
    makepad_stage_render_kit::EFFECTS.iter().map(|e| (e.name.to_string(), format!("{:?}", e.category).to_lowercase(), e.doc.to_string())).collect()
}

/// A new entry's code for a stack: a post kit or `Pass` (`post`), or an
/// `Effect{name: @…}` (`effects`), with its first few numeric defaults
/// written out so its controls show.
pub fn new_entry(field: &str, name: &str) -> String {
    if field == "post" {
        // (Written through its module, so it needs no `use` line.)
        if name == "Pass" {
            return "mod.post.Pass{at: @hdr reads: [@color] uniforms: {amount: 1.0} pixel: fn() -> vec4 { let c = self.color.sample(self.uv()) return vec4(c.xyz * self.amount, c.w) }}".into();
        }
        let numeric: Vec<String> = kit_defaults(name).into_iter().filter(|(_, v)| v.parse::<f64>().is_ok()).take(3).map(|(k, v)| format!("{k}: {v}")).collect();
        return if numeric.is_empty() { format!("mod.post.{name}{{}}") } else { format!("mod.post.{name}{{{}}}", numeric.join(" ")) };
    }
    let node = Node { kind: NodeKind::Effect, path: String::new(), ty: name.to_string(), label: String::new(), comment: None, range: 0..0, on: true, children: Vec::new(), stacks: Vec::new() };
    let numeric: Vec<String> = definition(&node).into_iter().filter(|(_, k, d, _)| *k == ValueKind::Number && d.is_some()).take(2).map(|(n, _, d, _)| format!("{n}: {}", d.unwrap())).collect();
    if numeric.is_empty() { format!("Effect{{name: @{name}}}") } else { format!("Effect{{name: @{name} {}}}", numeric.join(" ")) }
}

// ------------------------------------------------------------------ writing

/// One text edit: the bytes `range` of the document become `text`.
#[derive(Clone, Debug, PartialEq)]
pub struct Splice {
    pub range: Range<usize>,
    pub text: String,
}

impl Splice {
    pub fn apply(&self, text: &str) -> String {
        format!("{}{}{}", &text[..self.range.start], self.text, &text[self.range.end..])
    }
}

/// What to do to an attached stack.
#[derive(Clone, Debug, PartialEq)]
pub enum StackOp {
    /// Entry `i` bypassed (`on: false` written) or on again (taken away).
    Bypass(usize, bool),
    Remove(usize),
    /// Entry `from` to place `to` (counted as the list is after taking it out).
    Move(usize, usize),
    /// A new entry at the end, as code (`Bloom{strength: 0.6}`).
    Add(String),
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn indent_of(text: &str, at: usize) -> String {
    let ls = line_start(text, at);
    text[ls..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// An entry's block: its `//` lines above and itself, as whole lines when
/// it stands on its own lines (`lines`), else its own bytes.
fn block(text: &str, item: &Node) -> (Range<usize>, bool) {
    let ls = line_start(text, item.range.start);
    let alone_before = text[ls..item.range.start].trim().is_empty();
    let line_end = text[item.range.end..].find('\n').map_or(text.len(), |i| item.range.end + i);
    let alone_after = text[item.range.end..line_end].trim().is_empty();
    if !(alone_before && alone_after) {
        return (item.range.clone(), false);
    }
    let mut start = ls;
    while start > 0 {
        let prev = line_start(text, start - 1);
        if text[prev..start].trim_start().starts_with("//") {
            start = prev;
        } else {
            break;
        }
    }
    (start..(line_end + 1).min(text.len()), true)
}

/// A field added to the record written at `record` (its first byte):
/// `name: code` first in it, on its own line when the record's fields are
/// on lines.
pub fn add_field(text: &str, record: usize, name: &str, code: &str) -> Option<Splice> {
    let open = record + text[record..].find('{')?;
    let after = &text[open + 1..];
    if after.starts_with('\n') || after.starts_with("\r\n") {
        let next_line = open + 1 + after.find('\n')? + 1;
        let indent = indent_of(text, next_line);
        let indent = if text[next_line..].trim_start().starts_with('}') { format!("{indent}    ") } else { indent };
        return Some(Splice { range: open + 1..open + 1, text: format!("\n{indent}{name}: {code}") });
    }
    let sep = if after.trim_start().starts_with('}') { "" } else { " " };
    Some(Splice { range: open + 1..open + 1, text: format!("{name}: {code}{sep}") })
}

/// The edit `op` makes to `stack` (attached to `owner`) in the document
/// `text`. A stack not written yet is added to its owner's record; a
/// stack written as a call (`post: film_post({…})`) becomes a list holding
/// it.
pub fn stack_edit(text: &str, owner: &Node, stack: &Stack, op: &StackOp) -> Result<Splice, String> {
    let item = |i: usize| stack.items.get(i).ok_or_else(|| format!("{} has no entry {i}", stack.path));
    match op {
        StackOp::Bypass(i, on) => {
            let it = item(*i)?;
            if it.kind == NodeKind::Generator || it.kind == NodeKind::Code {
                return Err(format!("`{}` is code: expand it to bypass its entries", it.label));
            }
            let at = it.range.start + text[it.range.clone()].find('{').ok_or("not a record")?;
            if !*on {
                if !it.on {
                    return Err("already bypassed".into());
                }
                let closes = text[at + 1..].trim_start().starts_with('}');
                return Ok(Splice { range: at + 1..at + 1, text: if closes { "on: false".into() } else { "on: false ".into() } });
            }
            // Take `on: false` (and the space after it) away.
            let V::Typed(fields, _) = read_at(text, it.range.start) else { return Err("not a record".into()) };
            let (_, v) = fields.iter().find(|(k, _)| k == "on").ok_or("not bypassed")?;
            let vr = v.range();
            let key = text[..vr.start].rfind("on").ok_or("not bypassed")?;
            let mut end = vr.start + text[vr.clone()].trim_end().len();
            while text[end..].starts_with(' ') {
                end += 1;
            }
            Ok(Splice { range: key..end, text: String::new() })
        }
        StackOp::Remove(i) => {
            let it = item(*i)?;
            if stack.generated {
                return Err("the stack is one call: write it as a list first".into());
            }
            let (mut r, lines) = block(text, it);
            if !lines {
                // A space on one side goes with it.
                if text[r.end..].starts_with(' ') {
                    r.end += 1;
                } else if r.start > 0 && text[..r.start].ends_with(' ') {
                    r.start -= 1;
                }
            }
            Ok(Splice { range: r, text: String::new() })
        }
        StackOp::Move(from, to) => {
            let list = stack.range.clone().ok_or("no list")?;
            if stack.generated {
                return Err("the stack is one call".into());
            }
            let n = stack.items.len();
            if *from >= n {
                return Err(format!("no entry {from}"));
            }
            let blocks: Vec<(Range<usize>, bool)> = stack.items.iter().map(|it| block(text, it)).collect();
            let lines = blocks.iter().all(|(_, l)| *l);
            let mut order: Vec<usize> = (0..n).collect();
            let moved = order.remove(*from);
            order.insert((*to).min(n - 1), moved);
            if lines {
                // Whole lines, rebuilt between the first block and the last.
                let span = blocks[0].0.start..blocks[n - 1].0.end;
                let body: String = order.iter().map(|&k| {
                    let t = &text[blocks[k].0.clone()];
                    if t.ends_with('\n') { t.to_string() } else { format!("{t}\n") }
                }).collect();
                let body = if text[span.clone()].ends_with('\n') { body } else { body.trim_end_matches('\n').to_string() };
                return Ok(Splice { range: span, text: body });
            }
            let inner: Vec<&str> = order.iter().map(|&k| &text[stack.items[k].range.clone()]).collect();
            let _ = list;
            let span = stack.items[0].range.start..stack.items[n - 1].range.end;
            Ok(Splice { range: span, text: inner.join(" ") })
        }
        StackOp::Add(code) => {
            match (&stack.range, stack.generated) {
                (None, _) => {
                    let field = stack.field;
                    add_field(text, owner.range.start, field, &format!("[{code}]")).ok_or_else(|| "the owner is not a record".into())
                }
                (Some(r), true) => {
                    let call = &text[r.clone()];
                    Ok(Splice { range: r.clone(), text: format!("[{call}, {code}]") })
                }
                (Some(r), false) => {
                    let list = &text[r.clone()];
                    let close = r.end - 1;
                    if !list.contains('\n') {
                        let empty = list[1..list.len() - 1].trim().is_empty();
                        return Ok(Splice { range: close..close, text: if empty { code.clone() } else { format!(" {code}") } });
                    }
                    let close_ls = line_start(text, close);
                    let indent = match stack.items.last() {
                        Some(last) => indent_of(text, last.range.start),
                        None => format!("{}    ", indent_of(text, close)),
                    };
                    if text[close_ls..close].trim().is_empty() {
                        Ok(Splice { range: close_ls..close_ls, text: format!("{indent}{code}\n") })
                    } else {
                        Ok(Splice { range: close..close, text: format!("\n{indent}{code}") })
                    }
                }
            }
        }
    }
}

/// A parameter not written yet, written into its node's record (its
/// default, or `code`).
pub fn write_param(text: &str, node: &Node, name: &str, code: &str) -> Option<Splice> {
    if node.kind == NodeKind::Generator {
        let value = read_at(text, node.range.start);
        let (_, args) = call_args(text, &value)?;
        return match args.iter().rev().find(|a| matches!(a, V::Obj(..))) {
            Some(record) => add_field(text, record.range().start, name, code),
            None => None,
        };
    }
    add_field(text, node.range.start, name, code)
}

// ------------------------------------------------------------------ generated content

/// A call in a layer's drawing code that makes several things (a helper
/// drawn in a loop, a kernel's segments): what the picture's generated
/// instances group under (STRUCTURE.md §1.4).
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    /// `<layer path>#<callee>@<byte>`: stable while the code around it is.
    pub path: String,
    pub callee: String,
    /// The call's bytes.
    pub range: Range<usize>,
    /// Its arguments as written.
    pub args: Vec<String>,
    /// The loops it is drawn in, outermost first: (variable, what it runs
    /// over as written).
    pub loops: Vec<(String, String)>,
}

impl Generated {
    /// Where instance `index` of the innermost loop is written, when the
    /// loop runs over a list written in the document (`for s in STROKES`
    /// → `STROKES[12]`) and the call reads that variable.
    pub fn instance_path(&self, index: usize) -> Option<String> {
        let (var, over) = self.loops.last()?;
        let reads = self.args.iter().any(|a| a.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == var));
        let over = over.trim();
        (reads && !over.is_empty() && over.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')).then(|| format!("{over}[{index}]"))
    }
}

/// The calls a layer's drawing functions make (`draw: fn(c, t, edit) {…}`),
/// each with the loops it runs in. Draw2D's own methods (`c.text`) and
/// calls inside other calls are left out: a node is the outermost call
/// written in the layer.
pub fn generators(text: &str, code: &crate::schema::code::CodeOutline, layer: &Node) -> Vec<Generated> {
    use crate::schema::code::RegionKind;
    let within = |r: &Range<usize>| r.start >= layer.range.start && r.end <= layer.range.end;
    // Child layers' code belongs to them.
    let mut inner: Vec<Range<usize>> = Vec::new();
    fn kids(n: &Node, out: &mut Vec<Range<usize>>) {
        for c in &n.children {
            out.push(c.range.clone());
        }
    }
    kids(layer, &mut inner);
    let calls: Vec<usize> = (0..code.regions.len()).filter(|&i| {
        let r = &code.regions[i];
        r.kind == RegionKind::Call && within(&r.range) && !inner.iter().any(|k| k.start <= r.range.start && r.range.end <= k.end) && !r.name.starts_with("c.") && !r.name.is_empty()
    }).collect();
    let outer: Vec<usize> = calls.iter().copied().filter(|&i| {
        let mut p = code.regions[i].parent;
        while let Some(k) = p {
            if calls.contains(&k) {
                return false;
            }
            p = code.regions[k].parent;
        }
        true
    }).collect();
    // Only what the drawing functions draw: a Draw2D's `draw:` (or a
    // layer's own drawing function), and only calls made as statements (a
    // call whose value is kept, `let f = frame(t)`, computes, it does not
    // draw).
    if layer.ty != "Draw2D" {
        return Vec::new();
    }
    outer.into_iter().filter(|&i| {
        let at = code.regions[i].range.start;
        let ls = line_start(text, at);
        let before = text[ls..at].trim();
        before.is_empty() || before.ends_with('{') || before.ends_with(';')
    }).filter_map(|i| {
        let r = &code.regions[i];
        let value = read_at(text, r.range.start);
        let (callee, args) = call_args(text, &value)?;
        let args = args.iter().map(|a| text[a.range()].trim_end().to_string()).collect();
        Some(Generated { path: format!("{}#{}@{}", layer.path, callee, r.range.start), callee, range: r.range.clone(), args, loops: loops_around(text, layer.range.clone(), r.range.start) })
    }).collect()
}

/// The `for x in EXPR {…}` loops written in `scope` whose bodies hold
/// byte `at`, outermost first.
fn loops_around(text: &str, scope: Range<usize>, at: usize) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut from = scope.start;
    while let Some(k) = text[from..at].find("for ").map(|k| from + k) {
        from = k + 4;
        if k > 0 && text[..k].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let head_end = match text[k..at].find('{') {
            Some(e) => k + e,
            None => continue,
        };
        let head = &text[k + 4..head_end];
        let Some((var, over)) = head.split_once(" in ") else { continue };
        // The body's end: its matching brace.
        let mut depth = 0i32;
        let mut end = text.len();
        for (j, c) in text[head_end..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = head_end + j;
                        break;
                    }
                }
                _ => {}
            }
        }
        if at < end {
            out.push((var.trim().to_string(), over.trim().to_string()));
        }
    }
    out
}

// ------------------------------------------------------------------ signatures and promotion

/// A function the document defines as a top-level `let NAME = fn(…) {…}`:
/// its parameters (as written, a default after `=`) and where its body is.
#[derive(Clone, Debug, PartialEq)]
pub struct FnDef {
    pub name: String,
    /// Each parameter: its name and its default as written.
    pub params: Vec<(String, Option<String>)>,
    /// The `let`'s bytes, and the body's (inside the braces).
    pub range: Range<usize>,
    pub body: Range<usize>,
}

/// The document's own function `name`.
pub fn fn_def(text: &str, code: &crate::schema::code::CodeOutline, name: &str) -> Option<FnDef> {
    use crate::schema::code::RegionKind;
    let region = code.regions.iter().filter(|r| r.kind == RegionKind::Let && r.depth == 0 && r.name == name).last()?;
    let r = region.range.clone();
    let after_eq = r.start + text[r.clone()].find('=')? + 1;
    let rest = text[after_eq..r.end].trim_start();
    let fn_at = r.end - text[after_eq..r.end].len() + (text[after_eq..r.end].len() - rest.len());
    if !rest.starts_with("fn") {
        return None;
    }
    let open_p = fn_at + text[fn_at..].find('(')?;
    let mut depth = 0i32;
    let mut close_p = open_p;
    for (j, c) in text[open_p..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    close_p = open_p + j;
                    break;
                }
            }
            _ => {}
        }
    }
    let params = split_top(&text[open_p + 1..close_p]).into_iter().filter(|p| !p.trim().is_empty()).map(|p| {
        let p = p.trim();
        match p.split_once('=') {
            Some((n, d)) => (n.trim().split(':').next().unwrap_or("").trim().to_string(), Some(d.trim().to_string())),
            None => (p.split(':').next().unwrap_or("").trim().to_string(), None),
        }
    }).collect();
    let open_b = close_p + text[close_p..].find('{')?;
    let close_b = r.end.min(text.len());
    let close_b = text[..close_b].rfind('}')?;
    Some(FnDef { name: name.to_string(), params, range: r, body: open_b + 1..close_b })
}

/// `a, b, {c: d}` split at its top-level commas.
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut from) = (0i32, 0);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[from..]);
    out
}

/// One option a function reads: `o.NAME ?? DEFAULT` of its parameter `of`
/// (a record the call gives), or a parameter with a default.
#[derive(Clone, Debug, PartialEq)]
pub struct SigParam {
    pub name: String,
    pub default: Option<String>,
    /// The parameter it is read from (`o`), or None for a parameter itself.
    pub of: Option<String>,
}

/// What a function can be given: its parameters with defaults, and every
/// `p.NAME ?? DEFAULT` it reads of a parameter `p` (an options record).
pub fn signature(text: &str, def: &FnDef) -> Vec<SigParam> {
    let mut out: Vec<SigParam> = def.params.iter().filter(|(_, d)| d.is_some()).map(|(n, d)| SigParam { name: n.clone(), default: d.clone(), of: None }).collect();
    let body = &text[def.body.clone()];
    for (p, _) in &def.params {
        if p.is_empty() {
            continue;
        }
        let pat = format!("{p}.");
        let mut from = 0;
        while let Some(k) = body[from..].find(&pat).map(|k| from + k) {
            from = k + pat.len();
            if k > 0 && body[..k].chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                continue;
            }
            let name: String = body[from..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if name.is_empty() {
                continue;
            }
            let after = body[from + name.len()..].trim_start();
            // A method (`c.stroke(…)`), not an option.
            if body[from + name.len()..].starts_with('(') {
                continue;
            }
            let default = after.strip_prefix("??").map(|d| {
                let d = d.trim_start();
                // The default: up to where the expression ends.
                let mut depth = 0i32;
                let mut end = d.len();
                for (j, c) in d.char_indices() {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' if depth == 0 => {
                            end = j;
                            break;
                        }
                        ')' | ']' | '}' => depth -= 1,
                        ' ' | '\n' | ',' if depth == 0 => {
                            end = j;
                            break;
                        }
                        _ => {}
                    }
                }
                d[..end].trim().to_string()
            });
            if !out.iter().any(|o| o.name == name && o.of.as_deref() == Some(p)) {
                out.push(SigParam { name, default, of: Some(p.clone()) });
            }
        }
    }
    out
}

/// The literals written in a function's body that could be its
/// parameters: numbers and colours (not loop headers, not list data).
pub fn fn_literals(code: &crate::schema::code::CodeOutline, def: &FnDef) -> Vec<(String, String, Range<usize>)> {
    use crate::schema::ValueKind;
    // Not the plumbing: 0, 1, 2, -1 and tiny guards (`max(0.0001, …)`).
    let plumbing = |t: &str| t.trim().parse::<f64>().is_ok_and(|v| [0.0, 1.0, 2.0, -1.0, 0.5].contains(&v) || (v != 0.0 && v.abs() < 0.01));
    code.literals.iter().filter(|l| def.body.start <= l.range.start && l.range.end <= def.body.end && !l.in_loop && !l.in_list && matches!(l.kind, ValueKind::Number { .. } | ValueKind::Color) && !plumbing(&l.text)).map(|l| {
        let label = if !l.label.is_empty() { l.label.clone() } else { l.key.clone().unwrap_or_else(|| "value".into()) };
        (label, l.text.clone(), l.range.clone())
    }).collect()
}

/// A literal of a generator's function made a parameter, the code written
/// again (one text edit). By what the call gives:
/// - an options record (`film_post({…})`): the literal reads `o.NAME ?? L`,
///   and the call's record gets `NAME: L` (this call's own value);
/// - the item of a list it runs over (`for s in STROKES { f(c, s) }`): the
///   literal reads `s.NAME ?? L` (each item may then write its own NAME);
/// - else a top-level `let NAME = L` the literal names (one value, shared).
pub fn promote(text: &str, code: &crate::schema::code::CodeOutline, g: &Generated, literal: Range<usize>, name: &str) -> Result<String, String> {
    let def = fn_def(text, code, &g.callee).ok_or_else(|| format!("`{}` is not a function of this document", g.callee))?;
    if !(def.body.start <= literal.start && literal.end <= def.body.end) {
        return Err("the value is not in the function's body".into());
    }
    let lit = text[literal.clone()].to_string();
    let call = read_at(text, g.range.start);
    let (_, args) = call_args(text, &call).ok_or("not a call")?;
    // An options record given: that parameter.
    let options = args.iter().enumerate().rev().find(|(_, a)| matches!(a, V::Obj(..))).map(|(k, a)| (k, a.range()));
    if let Some((k, rec)) = options {
        let p = def.params.get(k).map(|p| p.0.clone()).ok_or("the call gives more than the function takes")?;
        let mut out = text.to_string();
        // From the end back: the call is after the function in the text, or before.
        let field = add_field(text, rec.start, name, &lit).ok_or("the options record")?;
        let edits = vec![(literal.clone(), format!("{p}.{name} ?? {lit}")), (field.range.clone(), field.text.clone())];
        apply_all(&mut out, edits);
        return Ok(out);
    }
    // The list item it runs over.
    if let Some((var, over)) = g.loops.last() {
        let k = args.iter().position(|a| text[a.range()].trim() == var);
        if let (Some(k), true) = (k, g.instance_path(0).is_some()) {
            let p = def.params.get(k).map(|p| p.0.clone()).ok_or("the call gives more than the function takes")?;
            let _ = over;
            let mut out = text.to_string();
            apply_all(&mut out, vec![(literal.clone(), format!("{p}.{name} ?? {lit}"))]);
            return Ok(out);
        }
    }
    // One shared value, named.
    let upper = name.to_uppercase();
    if code.regions.iter().any(|r| r.depth == 0 && r.name == upper) {
        return Err(format!("`{upper}` is already a name"));
    }
    let ls = text[..def.range.start].rfind('\n').map_or(0, |i| i + 1);
    let mut out = text.to_string();
    apply_all(&mut out, vec![(literal.clone(), upper.clone()), (ls..ls, format!("let {upper} = {lit}\n"))]);
    Ok(out)
}

fn apply_all(text: &mut String, mut edits: Vec<(Range<usize>, String)>) {
    edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    for (r, t) in edits {
        text.replace_range(r, &t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"use mod.motion.*
use mod.post; let BLOOM = 0.7
let POST = {bloom: 0.55 grain: 0.05}
Motion{
    label: "open"
    duration: 4.0
    post: [
        post.Aberration{falloff: 2 amount: fn(t) { 1.0 + t }}
        post.Bloom{strength: BLOOM threshold: 0.82}
        // the flash, after the shoulder
        post.Pass{at: @hdr reads: [@color] uniforms: {flash: 0.4} pixel: fn() -> vec4 { return self.color.sample(self.uv()) }}
        post.Grain{on: false amount: 0.05}
        film_post(POST)
    ]
    layers: [
        // the plate
        Group{size: vec2(1920, 1080) layers: [
            Rect{name: "bg" size: vec2(100, 100) fill: #ff0000 opacity: keys([0, 0], [1, 1])
                effects: [Effect{name: @glow radius: 18}]}
            set
        ]}
        Text{name: "title" text: "Hi" size: 40}
    ]
}
"#;

    #[test]
    fn reads_the_scene_its_post_and_its_layers() {
        let root = read(DOC).unwrap();
        assert_eq!(root.label, "open");
        let post = &root.stacks[0];
        assert_eq!(post.path, "Motion.post");
        let tys: Vec<&str> = post.items.iter().map(|n| n.ty.as_str()).collect();
        assert_eq!(tys, ["Aberration", "Bloom", "Pass", "Grain", "film_post"]);
        assert_eq!(post.items[2].comment.as_deref(), Some("the flash, after the shoulder"));
        assert!(!post.items[3].on);
        assert_eq!(post.items[4].kind, NodeKind::Generator);
        let plate = &root.children[0];
        assert_eq!(plate.label, "the plate");
        assert_eq!(plate.path, "Motion.layers[0]");
        assert_eq!(plate.children[0].label, "bg");
        assert_eq!(plate.children[0].stacks[0].items[0].ty, "glow");
        assert_eq!(plate.children[1].kind, NodeKind::Code);
        assert_eq!(layer(&root, "layers[0].layers[0]").unwrap().label, "bg");
        assert_eq!(root.children[1].label, "title");
    }

    #[test]
    fn params_read_written_and_defaults_and_paths_resolve() {
        let root = read(DOC).unwrap();
        let code = crate::schema::code::outline(DOC);
        let files = [paths::File { name: "", text: DOC, code: &code }];
        let bloom = &root.stacks[0].items[1];
        let ps = params(DOC, bloom);
        let strength = ps.iter().find(|p| p.name == "strength").unwrap();
        assert_eq!(strength.form, Form::Name);
        assert!(ps.iter().any(|p| p.name == "radius" && p.form == Form::Unwritten && p.default.is_some()), "{ps:?}");
        let threshold = ps.iter().find(|p| p.name == "threshold").unwrap();
        assert!(threshold.animatable());
        assert_eq!(paths::resolve(&files, &threshold.path).unwrap().text, "0.82");
        let pass = params(DOC, &root.stacks[0].items[2]);
        assert_eq!(pass[0].path, "Motion.post[2].uniforms.flash");
        assert_eq!(paths::resolve(&files, &pass[0].path).unwrap().text, "0.4");
        let gen = params(DOC, &root.stacks[0].items[4]);
        assert_eq!(gen.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["POST.bloom", "POST.grain"]);
        let bg = &root.children[0].children[0];
        let opacity = params(DOC, bg).into_iter().find(|p| p.name == "opacity").unwrap();
        assert_eq!(opacity.form, Form::Curve);
        let glow = params(DOC, &bg.stacks[0].items[0]);
        let radius = glow.iter().find(|p| p.name == "radius").unwrap();
        assert_eq!(paths::resolve(&files, &radius.path).unwrap().text, "18");
        assert!(glow.iter().any(|p| p.form == Form::Unwritten));
    }

    /// A layer's parameters as its reader takes them (the field registry):
    /// the line on a written one, each unwritten one the loader falls back
    /// on at that default, and the range it keeps one in.
    #[test]
    fn a_layers_params_are_its_readers_fields() {
        let root = read(DOC).unwrap();
        let bg = &root.children[0].children[0];
        let ps = params(DOC, bg);
        let fill = ps.iter().find(|p| p.name == "fill").unwrap();
        assert_eq!((fill.form, fill.doc.as_deref()), (Form::Literal, Some("a colour (#rrggbb, #rrggbbaa) or an animation of one")));
        let radius = ps.iter().find(|p| p.name == "radius").unwrap();
        assert_eq!((radius.form, radius.default.as_deref(), radius.doc.as_deref()), (Form::Unwritten, Some("0"), Some("the corner radius in px")));
        let cap = ps.iter().find(|p| p.name == "cap").unwrap();
        assert_eq!((cap.default.as_deref(), cap.kind), (Some("@butt"), ValueKind::Choice));
        assert!(!ps.iter().any(|p| p.name == "size" && p.form == Form::Unwritten), "written once");
        assert!(!ps.iter().any(|p| STRUCTURAL.contains(&p.name.as_str())), "no children or stacks: {ps:?}");
        assert_eq!(field_range(bg, "opacity"), Some((0.0, 1.0)));
        assert_eq!(field_range(bg, "radius"), None);
        // The document's own fields.
        assert!(params(DOC, &root).iter().any(|p| p.name == "label" && p.form == Form::Literal));
    }

    #[test]
    fn a_generated_post_is_one_generator_whose_options_are_paths() {
        let doc = "use mod.motion.*\nMotion{\n    post: film_post({bloom: 0.7 grain: 0.05})\n    layers: []\n}\n";
        let root = read(doc).unwrap();
        assert!(root.stacks[0].generated);
        let ps = params(doc, &root.stacks[0].items[0]);
        assert_eq!(ps[0].path, "Motion.post.bloom");
        let code = crate::schema::code::outline(doc);
        let files = [paths::File { name: "", text: doc, code: &code }];
        assert_eq!(paths::resolve(&files, "Motion.post.bloom").unwrap().text, "0.7");
    }

    #[test]
    fn new_entries_carry_their_defaults() {
        assert!(new_entry("post", "Glow").starts_with("mod.post.Glow{"));
        assert!(new_entry("effects", "glow").starts_with("Effect{name: @glow"));
        assert!(catalogue("effects").iter().any(|(n, _, _)| n == "drop_shadow"));
    }

    fn edit(doc: &str, owner_path: &str, field: &str, op: StackOp) -> String {
        let root = read(doc).unwrap();
        let owner = find(&root, owner_path).unwrap();
        let stack = owner.stacks.iter().find(|s| s.field == field).unwrap();
        stack_edit(doc, owner, stack, &op).unwrap().apply(doc)
    }

    #[test]
    fn stacks_bypass_remove_move_and_add_as_text() {
        let off = edit(DOC, "Motion", "post", StackOp::Bypass(1, false));
        assert!(off.contains("Bloom{on: false strength: BLOOM threshold: 0.82}"), "{off}");
        let on = edit(&off, "Motion", "post", StackOp::Bypass(1, true));
        assert_eq!(on, DOC);
        assert_eq!(edit(DOC, "Motion", "post", StackOp::Bypass(3, true)).matches("Grain{amount: 0.05}").count(), 1);
        let gone = edit(DOC, "Motion", "post", StackOp::Remove(2));
        assert!(!gone.contains("Pass{") && !gone.contains("the flash"), "{gone}");
        assert!(read(&gone).is_some());
        let moved = edit(DOC, "Motion", "post", StackOp::Move(2, 0));
        let root = read(&moved).unwrap();
        assert_eq!(root.stacks[0].items[0].ty, "Pass");
        assert_eq!(root.stacks[0].items[0].comment.as_deref(), Some("the flash, after the shoulder"));
        assert_eq!(root.stacks[0].items.len(), 5);
        let added = edit(DOC, "Motion", "post", StackOp::Add("Vignette{amount: 0.4}".into()));
        let root = read(&added).unwrap();
        assert_eq!(root.stacks[0].items[5].ty, "Vignette");
        assert!(added.contains("        film_post(POST)\n        Vignette{amount: 0.4}\n    ]"), "{added}");
        // An effects list not written yet: added to the layer.
        let fx = edit(DOC, "Motion.layers[1]", "effects", StackOp::Add("Effect{name: @glow radius: 8}".into()));
        assert!(fx.contains("Text{effects: [Effect{name: @glow radius: 8}] name: \"title\""), "{fx}");
        let inline = edit(DOC, "Motion.layers[0].layers[0]", "effects", StackOp::Add("Effect{name: @drop_shadow}".into()));
        assert!(inline.contains("effects: [Effect{name: @glow radius: 18} Effect{name: @drop_shadow}]"), "{inline}");
        // A post written as a call becomes a list holding it.
        let gen = "use mod.motion.*\nMotion{\n    post: film_post({bloom: 0.7})\n    layers: []\n}\n";
        let g = edit(gen, "Motion", "post", StackOp::Add("Glow{}".into()));
        assert!(g.contains("post: [film_post({bloom: 0.7}), Glow{}]"), "{g}");
        // A scene without post gets one.
        let bare = "use mod.motion.*\nMotion{\n    duration: 2.0\n    layers: []\n}\n";
        let b = edit(bare, "Motion", "post", StackOp::Add("Grain{}".into()));
        assert!(b.starts_with("use mod.motion.*\nMotion{\n    post: [Grain{}]\n    duration"), "{b}");
    }

    #[test]
    fn an_unwritten_param_is_written_into_its_record() {
        let root = read(DOC).unwrap();
        let bloom = &root.stacks[0].items[1];
        let s = write_param(DOC, bloom, "radius", "0.75").unwrap().apply(DOC);
        assert!(s.contains("Bloom{radius: 0.75 strength: BLOOM"), "{s}");
        let doc = "use mod.motion.*\nMotion{\n    post: film_post({bloom: 0.7})\n    layers: []\n}\n";
        let root = read(doc).unwrap();
        let s = write_param(doc, &root.stacks[0].items[0], "grain", "0.1").unwrap().apply(doc);
        assert!(s.contains("film_post({grain: 0.1 bloom: 0.7})"), "{s}");
    }

    #[test]
    fn a_layers_drawing_calls_are_its_generators_with_their_loops() {
        let doc = "use mod.motion.*\nlet STROKES = [{w: 1} {w: 2}]\nlet draw_stroke = fn(c, s, f) { c.stroke([], s.w, #fff, 1) }\nMotion{\n    layers: [\n        Draw2D{name: \"lines\" draw: fn(c, t, edit) {\n            let f = t\n            for s in STROKES { draw_stroke(c, s, f) }\n            draw_pcb(c, f)\n            c.text(\"x\", 0, 0, {})\n        }}\n    ]\n}\n";
        let root = read(doc).unwrap();
        let code = crate::schema::code::outline(doc);
        let gens = generators(doc, &code, &root.children[0]);
        let names: Vec<&str> = gens.iter().map(|g| g.callee.as_str()).collect();
        assert_eq!(names, ["draw_stroke", "draw_pcb"]);
        assert_eq!(gens[0].loops, vec![("s".to_string(), "STROKES".to_string())]);
        assert_eq!(gens[0].instance_path(1).as_deref(), Some("STROKES[1]"));
        assert!(gens[1].instance_path(0).is_none());
    }

    #[test]
    fn signatures_and_promotion() {
        let doc = "use mod.motion.*\nlet ITEMS = [{w: 1} {w: 2}]\nlet look = fn(c, o) { c.stroke([], o.width ?? 2.5, #fff, o.alpha ?? 0.5) c.fill(3.5) }\nlet spot = fn(c, s) { c.circle(s.w, 4.0) }\nlet plain = fn(c) { c.rect(7.0) }\nMotion{\n    layers: [\n        Draw2D{name: \"d\" draw: fn(c, t, edit) {\n            look(c, {width: 3})\n            for s in ITEMS { spot(c, s) }\n            plain(c)\n        }}\n    ]\n}\n";
        let code = crate::schema::code::outline(doc);
        let def = fn_def(doc, &code, "look").unwrap();
        assert_eq!(def.params, vec![("c".to_string(), None), ("o".to_string(), None)]);
        let sig = signature(doc, &def);
        assert_eq!(sig.iter().map(|p| (p.name.as_str(), p.default.as_deref())).collect::<Vec<_>>(), [("width", Some("2.5")), ("alpha", Some("0.5"))]);
        let root = read(doc).unwrap();
        let gens = generators(doc, &code, &root.children[0]);
        assert_eq!(gens.iter().map(|g| g.callee.as_str()).collect::<Vec<_>>(), ["look", "spot", "plain"]);
        let lits = fn_literals(&code, &def);
        let fill = lits.iter().find(|l| l.1 == "3.5").unwrap();
        let out = promote(doc, &code, &gens[0], fill.2.clone(), "fill").unwrap();
        assert!(out.contains("c.fill(o.fill ?? 3.5)") && out.contains("look(c, {fill: 3.5 width: 3})"), "{out}");
        let ddef = fn_def(doc, &code, "spot").unwrap();
        let r = fn_literals(&code, &ddef).into_iter().find(|l| l.1 == "4.0").unwrap().2;
        let out = promote(doc, &code, &gens[1], r, "radius").unwrap();
        assert!(out.contains("c.circle(s.w, s.radius ?? 4.0)"), "{out}");
        let pdef = fn_def(doc, &code, "plain").unwrap();
        let r = fn_literals(&code, &pdef).into_iter().find(|l| l.1 == "7.0").unwrap().2;
        let out = promote(doc, &code, &gens[2], r, "size").unwrap();
        assert!(out.contains("let SIZE = 7.0\nlet plain = fn(c) { c.rect(SIZE) }"), "{out}");
        assert!(read(&out).is_some());
    }
}
