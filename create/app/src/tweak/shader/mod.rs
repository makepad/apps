//! The shared app supplies reflection and formatted source. Town only edits source as a note.
pub(super) mod view;
use super::props::{RowBinding, RowKind, view::{PropLine, Sidebar}};
use makepad_strict_json::Value;
use std::{collections::{HashMap, HashSet}, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum ShaderIo { Instance, Uniform, Field }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) struct ShaderInput {
    pub name: String,
    pub io: ShaderIo,
    pub doc: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) struct ShaderConst {
    pub name: String,
    pub value: String,
    pub initial: String,
    pub loc: String,
    pub doc: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::tweak) struct ShaderFn {
    pub name: String,
    pub loc: String,
    pub src: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::tweak) struct ShaderLayer {
    pub layer: String,
    pub drawn: bool,
    pub doc: String,
    pub inputs: Vec<ShaderInput>,
    pub consts: Vec<ShaderConst>,
    pub fns: Vec<ShaderFn>,
    pub has_metadata: bool,
}

pub(super) fn layer_name(v: &Value) -> Option<&str> {
    v.get("layer")?.as_str().filter(|s| makepad_town_core::literal::is_identifier(s))
}

pub(super) fn fn_name(v: &Value) -> Option<&str> {
    v.get("name")?.as_str().filter(|s| matches!(*s, "pixel" | "vertex"))
}

fn entries<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_arr).unwrap_or_default()
}

fn string(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(in crate::tweak) fn parse_shaders(json: &Value, rows: &[RowBinding]) -> Vec<ShaderLayer> {
    if json.get("shaders").is_none() {
        let mut seen = HashSet::new();
        return rows.iter().filter_map(|row| {
            let (layer, _) = row.prop.split_once('.')?;
            (layer.starts_with("draw_") && seen.insert(layer)).then(|| ShaderLayer {
                layer: layer.to_owned(), drawn: true, ..Default::default()
            })
        }).collect();
    }
    entries(json, "shaders").iter().filter_map(|v| Some(ShaderLayer {
        layer: layer_name(v)?.to_owned(),
        drawn: v.get("drawn").and_then(Value::as_bool).unwrap_or_default(),
        has_metadata: true,
        doc: string(v, "doc"),
        inputs: entries(v, "inputs").iter().filter_map(|i| Some(ShaderInput {
            name: i.get("name")?.as_str()?.to_owned(),
            io: match i.get("io").and_then(Value::as_str) {
                Some("instance") => ShaderIo::Instance,
                Some("uniform") => ShaderIo::Uniform,
                _ => ShaderIo::Field,
            },
            doc: string(i, "doc"),
        })).collect(),
        consts: entries(v, "consts").iter().filter_map(|c| Some(ShaderConst {
            name: c.get("name")?.as_str()?.to_owned(), value: string(c, "value"),
            initial: string(c, "initial"), loc: string(c, "loc"), doc: string(c, "doc"),
        })).collect(),
        fns: entries(v, "fns").iter().filter_map(|f| Some(ShaderFn {
            name: fn_name(f)?.to_owned(), src: f.get("src")?.as_str()?.to_owned(), loc: string(f, "loc"),
        })).collect(),
    })).collect()
}

pub(in crate::tweak) fn format_fn_sources(layer: &ShaderLayer) -> String {
    if !layer.has_metadata {
        return "(shader source unavailable from this app version)".into();
    }
    if layer.fns.is_empty() {
        return "(no script-defined pixel/vertex fn on this layer — the type's own shader)".into();
    }
    layer.fns.iter().map(|f| format!("// {} — {}\n{}", f.name, f.loc, f.src))
        .collect::<Vec<_>>().join("\n\n")
}

/// Split on the editor's function headings and preserve comments inside each body.
pub(in crate::tweak) fn diff_fn_edits(layer: &ShaderLayer, editor_text: &str) -> Vec<(String, String)> {
    let mut edits = Vec::new();
    let mut record = |f: &ShaderFn, body: &str| {
        if body.trim() != f.src.trim() {
            edits.push((format!("{}.{}", layer.layer, f.name), body.trim().to_owned()));
        }
    };
    let mut current = None;
    let mut offset = 0;
    for line in editor_text.split_inclusive('\n') {
        if let Some((name, _loc)) = line.strip_prefix("// ").and_then(|s| s.split_once(" — ")) {
            if let Some(f) = layer.fns.iter().find(|f| f.name == name) {
                if let Some((previous, start)) = current {
                    record(previous, &editor_text[start..offset]);
                }
                current = Some((f, offset + line.len()));
            }
        }
        offset += line.len();
    }
    if let Some((f, start)) = current {
        record(f, &editor_text[start..]);
    } else if let [f] = layer.fns.as_slice() {
        // A single-function editor also accepts a replacement without its heading.
        record(f, editor_text);
    }
    edits
}

pub(in crate::tweak) struct ShaderTab {
    pub active_layer: String,
    pub consts_open: bool,
    pub src_open: bool,
    pub synced_key: (Rc<str>, String),
    pub synced_text: String,
    pub status: String,
    pub layer_pills: Vec<String>,
}

impl Default for ShaderTab {
    fn default() -> Self {
        Self {
            active_layer: String::new(), consts_open: true, src_open: true,
            synced_key: Default::default(), synced_text: String::new(),
            status: String::new(), layer_pills: Vec::new(),
        }
    }
}

impl ShaderTab {
    pub(super) fn lines(&self, sidebar: &Sidebar, layer: &ShaderLayer, palette: &super::theme::ThemePalette) -> Vec<PropLine> {
        let mut lines = Vec::new();
        if !layer.consts.is_empty() {
            lines.push(PropLine::Header { title: "Shader constants".into(),
                count: layer.consts.len().to_string(), open: Some(self.consts_open) });
            if self.consts_open {
                lines.extend(layer.consts.iter().map(|c| PropLine::Info {
                    name: c.name.clone(), value: c.value.clone(), loc: c.loc.clone(),
                }));
            }
        }
        let inputs: HashMap<_, _> = layer.inputs.iter().map(|i| (i.name.as_str(), i)).collect();
        let mut documented = Vec::new();
        let mut other = Vec::new();
        let prefix = format!("{}.", layer.layer);
        for row in &sidebar.props.rows {
            let Some(leaf) = row.prop.strip_prefix(&prefix) else { continue };
            let input = inputs.get(leaf);
            let tag = layer.has_metadata.then(|| match input.map(|i| i.io) {
                Some(ShaderIo::Instance) => "inst",
                Some(ShaderIo::Uniform) => "unif",
                _ => "field",
            });
            let group = if input.is_some_and(|i| !i.doc.is_empty()) { &mut documented } else { &mut other };
            group.push(PropLine::Prop { path: sidebar.path.clone(), row: sidebar.bound_row(row), theme_match: (row.kind == RowKind::Color).then(|| palette.match_hex(&row.value)).flatten(),
                tag, refusal: sidebar.refusals.get(&row.prop).cloned() });
        }
        lines.push(PropLine::Header { title: "Inputs".into(), count: (documented.len() + other.len()).to_string(), open: None });
        lines.extend(documented);
        lines.extend(other);
        lines
    }
}
