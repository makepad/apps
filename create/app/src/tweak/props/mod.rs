//! The tweaker's Props rows, built from the shared app's design data.
pub(super) mod view;
pub(super) mod layout;
pub(super) mod live;
use layout::{BoxKind, GroupKind, LayoutKind, LayoutValues};

use makepad_widgets::{design_data::props::fmt_f64, fab_controls::{format_hex, FabValueInputAction}, live_id, vec4, LiveId, Vec4f};
use std::{borrow::Cow, collections::HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StructKind {
    None,
    Vec2,
    Vec3,
    Vec4,
    Inset,
    Metrics,
    /// Recognized as structured but with no editor yet (a big nested
    /// struct like a full text_style): shown collapsed, never dumped.
    NoEditor,
}

/// The number token after `key:` in a `{..}` dump (`null` reads as 0).
fn struct_num(text: &str, key: &str) -> Option<f64> {
    let at = text.find(&format!("{key}:"))?;
    let rest = text[at + key.len() + 1..].trim_start();
    let tok: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '}' && *c != ',')
        .collect();
    if tok == "null" {
        return Some(0.0);
    }
    tok.parse().ok()
}

/// Classify a reflected value dump into a typed editor + its component
/// values (empty for NoEditor). Only dumps reach here — scalars, colours
/// and bools are already their own row kinds.
pub(super) fn parse_struct(value: &str) -> (StructKind, Vec<f64>) {
    let v = value.trim();
    for (pfx, k, n) in [
        ("vec2f(", StructKind::Vec2, 2usize),
        ("vec3f(", StructKind::Vec3, 3),
        ("vec4f(", StructKind::Vec4, 4),
    ] {
        if let Some(inner) = v.strip_prefix(pfx).and_then(|s| s.strip_suffix(')')) {
            let nums: Vec<f64> = inner.split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|s| s.parse().ok()).collect();
            if nums.len() == n {
                return (k, nums);
            }
        }
    }
    // Rows supplies bare field dumps. A Set can also carry the named struct literal.
    let v = v.strip_prefix("Inset").or_else(|| v.strip_prefix("Metrics")).unwrap_or(v);
    if v.starts_with('{') && v.ends_with('}') {
        if v.contains("left:") && v.contains("bottom:") {
            return (
                StructKind::Inset,
                vec![
                    struct_num(v, "left").unwrap_or(0.0),
                    struct_num(v, "top").unwrap_or(0.0),
                    struct_num(v, "right").unwrap_or(0.0),
                    struct_num(v, "bottom").unwrap_or(0.0),
                ],
            );
        }
        if v.contains("descender:") {
            return (
                StructKind::Metrics,
                vec![
                    struct_num(v, "descender").unwrap_or(0.0),
                    struct_num(v, "line_gap").unwrap_or(0.0),
                    struct_num(v, "line_scale").unwrap_or(1.0),
                ],
            );
        }
        return (StructKind::NoEditor, Vec::new());
    }
    (StructKind::None, Vec::new())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowKind {
    Num,
    Bool,
    Text,
    Color,
    /// Read-only display row (the CASCADE section): label + value only.
    Info,
}

/// The sidebar's property groups, hottest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SectionKind {
    Layout,
    Style,
    Text,
    Behavior,
    Other,
    /// The construction chain of the selection: one block per prototype
    /// level (instance -> styled type -> base -> native), each with its
    /// source location, `///` docs and the keys it sets. CSS-inspector
    /// style: a key later marked ~name~ is overridden by a closer level.
    Cascade,
}

pub(super) const SECTION_ORDER: [SectionKind; 6] = [
    SectionKind::Layout,
    SectionKind::Style,
    SectionKind::Text,
    SectionKind::Behavior,
    SectionKind::Other,
    SectionKind::Cascade,
];

impl SectionKind {
    pub(super) fn index(self) -> usize {
        match self {
            SectionKind::Layout => 0,
            SectionKind::Style => 1,
            SectionKind::Text => 2,
            SectionKind::Behavior => 3,
            SectionKind::Other => 4,
            SectionKind::Cascade => 5,
        }
    }
    pub(super) fn title(self) -> &'static str {
        match self {
            SectionKind::Layout => "Layout",
            SectionKind::Style => "Style",
            SectionKind::Text => "Text",
            SectionKind::Behavior => "Behavior",
            SectionKind::Other => "Other",
            SectionKind::Cascade => "Cascade",
        }
    }
}

/// Derive the group from what the reflection already knows — never a
/// hand-tagged schema. A known family matched by name prefix beats OTHER.
fn classify_prop(prop: &str, value: &str) -> SectionKind {
    let first = prop.split('.').next().unwrap_or("");
    match first {
        "width" | "height" | "abs_pos" | "margin" | "padding" | "spacing" | "line_spacing"
        | "align" | "flow" | "clip_x" | "clip_y" | "scroll" | "wrap_spacing" | "layout"
        | "metrics" | "baseline" | "distribute" | "container_id" | "min_width" | "max_width"
        | "min_height" | "max_height" | "aspect" | "cell" | "columns" | "rows" | "areas"
        | "column_gap" | "row_gap" | "auto_flow" | "justify_items" | "align_items"
        | "implicit_column_size" | "implicit_row_size" => return SectionKind::Layout,
        "text" | "empty_text" | "label" | "title" | "suffix" => return SectionKind::Text,
        "visible" | "enabled" | "grab_key_focus" | "cursor" | "trigger_on_press"
        | "enable_long_press" | "reset_hover_on_click" | "block_signal_event"
        | "capture_overload" | "event_order" | "design_mode" | "search_scope"
        | "grab_focus" | "hover_actions_enabled" | "animator" => return SectionKind::Behavior,
        _ => {}
    }
    if first.ends_with("_walk") {
        // label_walk belongs with the text it lays out; the rest are layout.
        if first == "label_walk" {
            return SectionKind::Text;
        }
        return SectionKind::Layout;
    }
    if first.starts_with("draw_") || first == "text_style" || first == "icon" {
        return SectionKind::Style;
    }
    if value == "true" || value == "false" {
        return SectionKind::Behavior;
    }
    SectionKind::Other
}

/// Inset legs in the box-model order: left, top, right, bottom.
fn inset_leg_rank(leaf: &str) -> u32 {
    match leaf {
        "left" => 0,
        "top" => 1,
        "right" => 2,
        "bottom" => 3,
        _ => 4,
    }
}

/// The within-section ordering: (major, minor, alpha tail). Equal keys keep
/// reflection order (stable sort) — and changed rows never reorder at all.
fn section_rank(section: SectionKind, prop: &str) -> (u32, u32, String) {
    let mut parts = prop.split('.');
    let first = parts.next().unwrap_or("");
    let leaf = parts.next().unwrap_or("");
    match section {
        // Cascade rows are pushed in display order after the sort and keep
        // it via the stable sort's equal-key rule.
        SectionKind::Cascade => (0, 0, String::new()),
        SectionKind::Layout => {
            // The box-model progression, outside-in.
            let major = match first {
                "width" => 0,
                "height" => 1,
                "min_width" => 2,
                "max_width" => 3,
                "min_height" => 4,
                "max_height" => 5,
                "aspect" => 6,
                "abs_pos" => 7,
                "cell" => 8,
                "margin" => 9,
                "padding" => 10,
                "spacing" => 11,
                "wrap_spacing" => 12,
                "line_spacing" => 13,
                "align" => 14,
                "distribute" => 15,
                "flow" => 16,
                "clip_x" => 17,
                "clip_y" => 18,
                "scroll" => 19,
                "container_id" => 20,
                "columns" => 21,
                "rows" => 22,
                "column_gap" => 23,
                "row_gap" => 24,
                "auto_flow" => 25,
                "areas" => 26,
                "justify_items" => 27,
                "align_items" => 28,
                "implicit_column_size" => 29,
                "implicit_row_size" => 29,
                _ => 30,
            };
            let minor = match first {
                "margin" | "padding" => inset_leg_rank(leaf),
                "align" => match leaf {
                    "x" => 0,
                    "y" => 1,
                    _ => 2,
                },
                _ => 0,
            };
            (major, minor, prop.to_string())
        }
        SectionKind::Style => {
            // Surface first, then content layers; colors lead each family.
            let major = match first {
                "draw_bg" => 0,
                "draw_text" => 1,
                "draw_icon" => 2,
                _ => 3, // remaining draw_* families keep reflection order
            };
            let minor = if leaf == "color" {
                0
            } else if first == "draw_bg" {
                match leaf {
                    "border_color" => 2,
                    "border_size" => 3,
                    "border_radius" => 4,
                    _ if leaf.starts_with("color") => 1,
                    _ if leaf.starts_with("border_color") => 2,
                    _ => 10,
                }
            } else if first == "draw_text" {
                if leaf.starts_with("color") {
                    1
                } else if leaf == "text_style" || leaf.contains("font") {
                    2
                } else {
                    10
                }
            } else if first == "draw_icon" {
                if leaf.starts_with("color") {
                    1
                } else if leaf == "scale" {
                    2
                } else {
                    10
                }
            } else if leaf.starts_with("color") {
                1
            } else {
                10
            };
            (major, minor, prop.to_string())
        }
        SectionKind::Text => {
            let major = match first {
                "text" => 0,
                "empty_text" => 1,
                "label" => 2,
                "title" => 3,
                "suffix" => 4,
                "label_walk" => 5,
                "metrics" => 6,
                _ => 10,
            };
            (major, 0, prop.to_string())
        }
        SectionKind::Behavior => {
            let major = match first {
                "visible" => 0,
                "enabled" => 1,
                "grab_key_focus" | "grab_focus" => 2,
                "trigger_on_press" | "enable_long_press" | "reset_hover_on_click" => 3,
                "animator" => 4,
                _ => 10,
            };
            (major, 0, prop.to_string())
        }
        SectionKind::Other => (0, 0, prop.to_string()),
    }
}

fn is_shader_plumbing(name: &str) -> bool {
    matches!(
        name,
        "rect_pos"
            | "rect_size"
            | "draw_clip"
            | "delta_clip"
            | "draw_depth"
            | "draw_zbias"
            | "char_index"
            | "texture_index"
            | "atlas_plane"
            | "t"
            | "t_min"
            | "t_max"
            | "temp_y_shift"
            | "aa_2x2"
            | "aa_4x4"
            | "pad1"
            | "pad2"
            | "font_scale"
            | "depth_clip"
            | "debug"
            | "stem_darken_max"
            | "draw_scroll"
            | "view_shift"
            | "view_clip"
            | "camera_projection"
            | "camera_view"
            | "camera_inv"
            | "dpi_factor"
            | "dpi_dilate"
            | "seed"
            | "pos"
            | "vertex_pos"
            | "total_chars"
            | "char_depth"
            | "draw_call"
            | "draw_list"
            | "draw_pass"
            | "geom"
            | "world"
            | "extend_area"
            | "ink_centered"
            | "sdf_sharpness"
            | "sdf_luma_bias"
            | "draw_flags"
            | "clip_x"
            | "clip_y"
    )
}

fn parse_hex(text: &str) -> Option<([f32; 4], bool)> {
    let t = text.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let nib = |c: u8| -> f32 {
        let d = (c as char).to_digit(16).unwrap_or(0) as f32;
        d / 15.0
    };
    let byte = |hi: u8, lo: u8| -> f32 {
        let h = (hi as char).to_digit(16).unwrap_or(0);
        let l = (lo as char).to_digit(16).unwrap_or(0);
        ((h * 16 + l) as f32) / 255.0
    };
    let b = t.as_bytes();
    match b.len() {
        3 => Some(([nib(b[0]), nib(b[1]), nib(b[2]), 1.0], false)),
        6 => Some((
            [byte(b[0], b[1]), byte(b[2], b[3]), byte(b[4], b[5]), 1.0],
            false,
        )),
        8 => Some((
            [
                byte(b[0], b[1]),
                byte(b[2], b[3]),
                byte(b[4], b[5]),
                byte(b[6], b[7]),
            ],
            true,
        )),
        _ => None,
    }
}

pub(super) fn layout_composited(prop: &str) -> bool {
    let first = prop.split('.').next().unwrap_or("");
    matches!(
        first,
        "width" | "height" | "min_width" | "max_width" | "min_height" | "max_height" | "aspect"
            | "margin" | "padding" | "spacing" | "wrap_spacing" | "flow" | "align"
            | "distribute" | "abs_pos" | "container_id" | "cell" | "columns" | "rows"
            | "areas" | "column_gap" | "row_gap" | "auto_flow" | "justify_items"
            | "align_items"
    )
}

fn flow_only(prop: &str) -> bool {
    matches!(
        prop.split('.').next().unwrap_or(""),
        "flow" | "spacing" | "wrap_spacing" | "align" | "distribute"
    )
}

fn level_color(level: usize) -> Vec4f {
    match level {
        0 => vec4(1.0, 0.62, 0.13, 1.0),
        1 => vec4(0.19, 0.78, 1.0, 1.0),
        2 => vec4(0.72, 0.5, 1.0, 1.0),
        3 => vec4(0.35, 0.85, 0.55, 1.0),
        _ => vec4(0.55, 0.55, 0.55, 1.0),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RowBinding {
    pub(super) prop: String,
    pub(super) kind: RowKind,
    pub(super) value: String,
    pub(super) quoted: bool,
    pub(super) section: SectionKind,
    pub(super) set: bool,
    pub(super) changed: bool,
    pub(super) original: Option<String>,
    pub(super) level: Option<usize>,
    pub(super) struct_kind: StructKind,
    pub(super) comp_vals: Vec<f64>,
}

/// Most interactions set one named property. A layout click can also
/// clear distribute before setting the main axis. Keep a single live step
/// on the stack and borrow its property name from the displayed binding.
pub(in crate::tweak) struct PropertySets<'a> {
    before: Option<(&'static str, String)>,
    prop: Cow<'a, str>,
    value: String,
}

impl<'a> PropertySets<'a> {
    fn one(prop: impl Into<Cow<'a, str>>, value: String) -> Self {
        Self { before: None, prop: prop.into(), value }
    }

    pub(in crate::tweak) fn iter(self) -> impl Iterator<Item = (Cow<'a, str>, String)> {
        self.before.into_iter().map(|(prop, value)| (prop.into(), value))
            .chain(std::iter::once((self.prop, self.value)))
    }
}

pub(super) enum EditorEvent<'a> {
    Number { leg: Option<usize>, action: FabValueInputAction },
    Color(Vec4f),
    Toggle(bool),
    Text(&'a str),
    Reset,
}

impl RowBinding {
    pub(super) fn template(&self) -> LiveId {
        match self.struct_kind {
            StructKind::Vec2 | StructKind::Vec3 | StructKind::Vec4 => live_id!(VecRow),
            StructKind::Inset => live_id!(InsetRow),
            StructKind::Metrics => live_id!(MetricsRow),
            StructKind::NoEditor => live_id!(NoEditorRow),
            StructKind::None => match self.kind {
                RowKind::Num => live_id!(NumRow),
                RowKind::Bool => live_id!(BoolRow),
                RowKind::Color => live_id!(ColorRow),
                RowKind::Text | RowKind::Info => live_id!(TextRow),
            },
        }
    }

    /// The value of one Set, if this editor action commits one.
    pub(super) fn set_value(&self, event: EditorEvent<'_>) -> Option<String> {
        use FabValueInputAction::{Changed, Ended, Reset};
        match event {
            EditorEvent::Reset | EditorEvent::Number { action: Reset, .. } => self.reset_prop(),
            EditorEvent::Number { leg: Some(comp), action: Changed(v) | Ended(v) } => self.struct_component_value(comp, v),
            EditorEvent::Number { leg: None, action: Changed(v) | Ended(v) } if self.kind == RowKind::Num => Some(fmt_f64(v)),
            EditorEvent::Color(v) if self.kind == RowKind::Color => Some(format_hex([v.x, v.y, v.z, v.w], true)),
            EditorEvent::Text(text) if self.kind == RowKind::Color => parse_hex(text).map(|(v, _)| format_hex(v, true)),
            EditorEvent::Toggle(on) if self.kind == RowKind::Bool => Some(on.to_string()),
            EditorEvent::Text(text) if self.struct_kind == StructKind::None
                && self.kind == RowKind::Text => {
                Some(if self.quoted { format!("{text:?}") } else { text.trim().to_owned() })
            }
            _ => None,
        }
    }

    fn struct_component_value(&self, comp: usize, v: f64) -> Option<String> {
        let mut vals = self.comp_vals.clone();
        *vals.get_mut(comp)? = v;
        let vals: Vec<_> = vals.into_iter().map(fmt_f64).collect();
        match self.struct_kind {
            StructKind::Vec2 | StructKind::Vec3 | StructKind::Vec4 => {
                Some(format!("vec{}f({})", vals.len(), vals.join(", ")))
            }
            StructKind::Inset => Some(format!("Inset{{left: {} top: {} right: {} bottom: {}}}",
                vals[0], vals[1], vals[2], vals[3])),
            StructKind::Metrics => Some(format!("Metrics{{descender: {} line_gap: {} line_scale: {}}}",
                vals[0], vals[1], vals[2])),
            StructKind::NoEditor | StructKind::None => None,
        }
    }

    fn reset_prop(&self) -> Option<String> {
        Some(match self.original.as_deref()? {
            "null" | "@-" => "nil",
            old => old,
        }.to_owned())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AppliedEdit {
    pub(super) path: String,
    pub(super) prop: String,
    pub(super) old: String,
    pub(super) new: String,
}

#[derive(Default)]
pub(super) struct Props {
    pub(super) rows: Vec<RowBinding>,
    pub(in crate::tweak) shaders: Vec<super::shader::ShaderLayer>,
    pub(super) cascade: Vec<CascadeLevel>,
}

pub(super) struct CascadeLevel {
    pub(super) loc: String,
    pub(super) file: String,
    pub(super) color: Vec4f,
}

impl Props {
    pub(super) fn rebuild_rows(rows: &str) -> Option<Self> {
        let json = super::parse_json(rows).ok()?;
        let mut props = Self::default();
        for row in json.get("props")?.as_arr()? {
            let (name, value, level) = super::app::prop_row(row)?;
            if name.split('.').skip(1).any(is_shader_plumbing)
                || (name.starts_with("draw_") && !name.contains('.') && value.starts_with('{')) {
                continue;
            }
            let value = value.to_owned();
            let (kind, display, quoted) = if value.starts_with('#') && parse_hex(&value).is_some()
            {
                (RowKind::Color, value, false)
            } else if value.parse::<f64>().is_ok() {
                (RowKind::Num, value, false)
            } else if value == "true" || value == "false" {
                (RowKind::Bool, value, false)
            } else if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
                (RowKind::Text, value[1..value.len() - 1].to_string(), true)
            } else {
                (RowKind::Text, value, false)
            };
            let section = classify_prop(name, &display);
            let (struct_kind, comp_vals) = if quoted { (StructKind::None, Vec::new()) } else { parse_struct(&display) };
            props.rows.push(RowBinding {
                prop: name.to_owned(), kind, value: display, quoted, section,
                set: row.get("set").and_then(|v| v.as_bool()).unwrap_or(false),
                changed: false, original: None, struct_kind, comp_vals, level: level.and_then(|n| n.try_into().ok()),
            });
        }
        props.rows.sort_by(|a, b| {
            let ka = (a.section.index(), section_rank(a.section, &a.prop));
            let kb = (b.section.index(), section_rank(b.section, &b.prop));
            ka.cmp(&kb)
        });
        if let Some(levels) = json.get("cascade").and_then(|v| v.as_arr()) {
            for (index, level) in levels.iter().enumerate() {
                props.cascade.push(CascadeLevel {
                    color: level_color(index),
                    loc: level.get("loc")?.as_str()?.to_owned(),
                    file: level.get("file")?.as_str()?.to_owned(),
                });
            }
        }
        props.shaders = super::shader::parse_shaders(&json, &props.rows);
        Some(props)
    }

    /// History arrives in the shared app's sequence order, including notices
    /// whose signed edits have yet to reach this seat.
    pub(super) fn mark_changed<'a>(&mut self, path: &str, history: impl Iterator<Item = &'a AppliedEdit>) {
        let mut changes = HashMap::new();
        for edit in history.filter(|edit| edit.path == path) {
            let (_, last) = changes.entry(edit.prop.as_str()).or_insert((&edit.old, &edit.new));
            *last = &edit.new;
        }
        for row in &mut self.rows {
            let change = changes.get(row.prop.as_str());
            row.changed = change.is_some_and(|(old, new)| old != new);
            row.original = change.map(|(old, _)| (*old).clone());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VisKind {
    Section(SectionKind, usize, bool),
    Measured,
    Layout(LayoutKind),
    Prop(usize),
    More(SectionKind, usize),
    CascadeLevel(usize),
}

fn style_curated(row: &RowBinding) -> bool {
    if row.kind == RowKind::Color {
        return true;
    }
    let leaf = row.prop.split('.').next_back().unwrap_or("");
    leaf.contains("radius")
        || leaf.contains("border_size")
        || leaf.contains("font_size")
        || leaf.contains("shadow")
}

impl Props {
    pub(super) fn layout_values(&self, ty: &str) -> LayoutValues {
        let mut values = LayoutValues { is_grid: self.is_grid(ty), ..Default::default() };
        for row in self.rows.iter().filter(|r| layout_composited(&r.prop)) {
            values.values.insert(row.prop.clone(), row.value.clone());
            if let Some(original) = &row.original { values.originals.insert(row.prop.clone(), original.clone()); }
        }
        values
    }

    fn is_grid(&self, ty: &str) -> bool {
        ty == "Grid" || (ty.is_empty() && self.rows.iter().any(|r| r.prop == "columns")
            && self.rows.iter().any(|r| r.prop == "rows"))
    }

    pub(super) fn visible_rows(&self, collapsed: &[bool; 6], expanded: &[bool; 6], ty: &str) -> Vec<VisKind> {
        let has = |prop: &str| self.rows.iter().any(|r| r.prop == prop);
        let is_grid = self.is_grid(ty);
        let mut out = vec![VisKind::Layout(LayoutKind::Identity)];
        for section in SECTION_ORDER {
            let open = !collapsed[section.index()];
            if section == SectionKind::Cascade {
                if !self.cascade.is_empty() {
                    out.push(VisKind::Section(section, self.cascade.len(), open));
                    if open { out.extend((0..self.cascade.len()).map(VisKind::CascadeLevel)); }
                }
                continue;
            }
            let members: Vec<_> = self.rows.iter().enumerate().filter(|(_, r)| r.section == section).collect();
            if members.is_empty() && section != SectionKind::Layout { continue; }
            out.push(VisKind::Section(section, members.len(), open));
            if !open { continue; }
            if section == SectionKind::Layout {
                use LayoutKind::*;
                out.push(VisKind::Measured);
                let mut push = |kind| out.push(VisKind::Layout(kind));
                push(Group(GroupKind::Item));
                push(Size);
                if self.rows.iter().any(|r| r.prop.starts_with("margin")) { push(BoxInset(BoxKind::Margin)); }
                if has("abs_pos") { push(Absolute); }
                // Rows has no parent type or pending container conversion / docking requests.
                if has("spacing") { push(Group(GroupKind::Container)); }
                if is_grid {
                    push(GridTracks);
                    push(AlignGrid);
                } else {
                    if has("spacing") || has("flow") { push(FlowSpacing); }
                    if has("align.x") { push(AlignGrid); }
                }
                if self.rows.iter().any(|r| r.prop.starts_with("padding")) { push(BoxInset(BoxKind::Padding)); }
            }
            let primary = |row: &RowBinding| match section {
                SectionKind::Layout => false,
                SectionKind::Style => style_curated(row),
                _ => true,
            };
            let force_show = if section == SectionKind::Layout || members.iter().any(|(_, row)| primary(row)) { 0 } else { 3 };
            let mut hidden = 0;
            for (n, (index, row)) in members.into_iter().enumerate() {
                if section == SectionKind::Layout && (layout_composited(&row.prop) || (is_grid && flow_only(&row.prop))) { continue; }
                if expanded[section.index()] || primary(row) || n < force_show {
                    out.push(VisKind::Prop(index));
                } else {
                    hidden += 1;
                }
            }
            if hidden > 0 { out.push(VisKind::More(section, hidden)); }
        }
        out
    }
}
