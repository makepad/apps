//! V1 bodies on the reliable Tweak topic. Little-endian integers. String lengths count UTF-8 bytes.
use super::{RequestId, Seat, SessionId};
use makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES;
use std::sync::Arc;
use makepad_town_core::{event::Id, literal::is_identifier};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect { pub window: usize, pub x: f64, pub y: f64, pub w: f64, pub h: f64 }
impl Rect {
    pub fn valid(self) -> bool {
        [self.x, self.y, self.w, self.h, self.x + self.w, self.y + self.h].iter().all(|n| n.is_finite())
            && self.w >= 0. && self.h >= 0.
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum At { Point { window: usize, x: f64, y: f64 }, Path(String) }

pub const TREE_PAGE: u16 = 200;
const TAG_REQUEST_TREE: u8 = 25;
const TAG_NOTICE_TREE: u8 = 26;

const TAG_REQUEST_THEME: u8 = 27;
const TAG_NOTICE_THEME: u8 = 28;

const TAG_REQUEST_DESIGN: u8 = 29;
const TAG_NOTICE_DESIGN: u8 = 30;

pub const MAX_DESIGN_PATH_LEN: usize = 1024;
pub const MAX_DESIGN_NAME_LEN: usize = 128;
pub const MAX_DESIGN_TY_LEN: usize = 256;
pub const MAX_DESIGN_QUERY_LEN: usize = 256;
pub const MAX_DESIGN_PROP_LEN: usize = 256;
pub const MAX_DESIGN_VALUE_LEN: usize = 16 * 1024;
pub const MAX_DESIGN_PROPS: usize = 128;
pub const MAX_DESIGN_SUMMARY_LEN: usize = 512;
pub const MAX_PALETTE_LIMIT: u16 = 500;
pub const MAX_DIFF_LIMIT: u32 = 32 * 1024;

pub(super) fn min_level(bytes: &[u8]) -> u16 {
    match bytes {
        [1, TAG_REQUEST_TREE | TAG_NOTICE_TREE, ..] => super::TREE_LEVEL,
        [1, TAG_REQUEST_THEME | TAG_NOTICE_THEME, ..] => super::THEME_LEVEL,
        [1, TAG_REQUEST_DESIGN | TAG_NOTICE_DESIGN, ..] => super::DESIGN_LEVEL,
        _ => 0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DesignPlace { Before = 1, Inside = 2, After = 3 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum StepDir { Up = 1, Down = 2, Out = 3 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BakeScope { Props = 1, Theme = 2, All = 3 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DesignReplyKind { State = 1, Palette = 2, DropTarget = 3, Diff = 4, Verify = 5, WritePatch = 6 }

#[derive(Clone, Debug, PartialEq)]
pub enum DesignRequest {
    State { path: Option<String> },
    Palette { query: Option<String>, offset: u32, limit: u16 },
    DropTarget { window: usize, x: f64, y: f64, exclude: Option<String> },
    Diff { file: Option<String>, offset: u32, limit: u32 },
    Verify { path: Option<String> },
    Insert { path: String, place: DesignPlace, ty: String, name: Option<String>, props: Vec<(String, String)> },
    Move { path: String, target: String, place: DesignPlace },
    Step { path: String, dir: StepDir },
    Delete { path: String, force_rust_refs: bool },
    Duplicate { path: String, name: Option<String> },
    Wrap { path: String, ty: String, name: Option<String> },
    Rename { path: String, name: String, force_rust_refs: bool },
    Bake { scope: BakeScope, path: Option<String>, prop: Option<String> },
    Undo,
    Redo,
    Reset,
    WritePatch { file: Option<String> },
    GhostInsert { target: String, place: DesignPlace, ty: String },
    GhostMove { source: String, target: String, place: DesignPlace },
    GhostCommit,
    GhostCancel,
}

fn design_path(path: &str) -> Result<(), &'static str> {
    if path.len() > MAX_DESIGN_PATH_LEN { return Err("overlong design path") }
    check_path(path)
}

fn design_prop(prop: &str) -> Result<(), &'static str> {
    if prop.len() > MAX_DESIGN_PROP_LEN { return Err("overlong design property") }
    check_prop(prop)
}

fn design_name(name: &str) -> Result<(), &'static str> {
    if name.len() <= MAX_DESIGN_NAME_LEN && is_identifier(name) { Ok(()) } else { Err("invalid design name") }
}

fn design_ty(ty: &str) -> Result<(), &'static str> {
    if ty.len() <= MAX_DESIGN_TY_LEN && ty.split('.').all(is_identifier) { Ok(()) } else { Err("invalid design type") }
}

fn design_text(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(char::is_control)
}

impl DesignRequest {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::State { .. } | Self::Palette { .. } | Self::DropTarget { .. }
            | Self::Diff { .. } | Self::Verify { .. })
    }

    pub fn is_ghost(&self) -> bool {
        matches!(self, Self::GhostInsert { .. } | Self::GhostMove { .. } | Self::GhostCommit | Self::GhostCancel)
    }

    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::State { path } | Self::Verify { path } => {
                if let Some(path) = path { design_path(path)?; }
            }
            Self::Palette { query, limit, .. } => {
                if !(1..=MAX_PALETTE_LIMIT).contains(limit) { return Err("invalid palette limit") }
                if query.as_ref().is_some_and(|q| q.trim().is_empty() || !design_text(q, MAX_DESIGN_QUERY_LEN)) {
                    return Err("invalid design query")
                }
            }
            Self::DropTarget { x, y, exclude, .. } => {
                if !x.is_finite() || !y.is_finite() { return Err("invalid coordinate") }
                if let Some(path) = exclude { design_path(path)?; }
            }
            Self::Diff { file, .. } | Self::WritePatch { file } => {
                if let Self::Diff { limit, .. } = self {
                    if !(1..=MAX_DIFF_LIMIT).contains(limit) { return Err("invalid diff limit") }
                }
                if file.as_ref().is_some_and(|f| f.is_empty() || !design_text(f, MAX_DESIGN_PATH_LEN)) {
                    return Err("invalid design file")
                }
            }
            Self::Insert { path, ty, name, props, .. } => {
                design_path(path)?;
                design_ty(ty)?;
                if let Some(name) = name { design_name(name)?; }
                if props.len() > MAX_DESIGN_PROPS { return Err("overlong design props count") }
                for (prop, value) in props {
                    design_prop(prop)?;
                    if value.len() > MAX_DESIGN_VALUE_LEN || value.trim().is_empty() { return Err("invalid design value") }
                }
            }
            Self::Move { path, target, .. } | Self::GhostMove { source: path, target, .. } => {
                design_path(path)?;
                design_path(target)?;
            }
            Self::Step { path, .. } | Self::Delete { path, .. } => design_path(path)?,
            Self::Duplicate { path, name } | Self::Wrap { path, name, .. } => {
                design_path(path)?;
                if let Some(name) = name { design_name(name)?; }
                if let Self::Wrap { ty, .. } = self { design_ty(ty)?; }
            }
            Self::Rename { path, name, .. } => { design_path(path)?; design_name(name)?; }
            Self::Bake { path, prop, .. } => {
                if let Some(path) = path { design_path(path)?; }
                if let Some(prop) = prop { design_prop(prop)?; }
            }
            Self::GhostInsert { target, ty, .. } => { design_path(target)?; design_ty(ty)?; }
            Self::Undo | Self::Redo | Self::Reset | Self::GhostCommit | Self::GhostCancel => {}
        }
        Ok(())
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::State { path } => { out.push(1); option(out, path.as_deref(), string); }
            Self::Palette { query, offset, limit } => {
                out.push(2);
                option(out, query.as_deref(), string);
                out.extend(offset.to_le_bytes());
                out.extend(limit.to_le_bytes());
            }
            Self::DropTarget { window, x, y, exclude } => {
                out.push(3);
                out.extend((*window as u64).to_le_bytes());
                out.extend(x.to_le_bytes());
                out.extend(y.to_le_bytes());
                option(out, exclude.as_deref(), string);
            }
            Self::Diff { file, offset, limit } => {
                out.push(4);
                option(out, file.as_deref(), string);
                out.extend(offset.to_le_bytes());
                out.extend(limit.to_le_bytes());
            }
            Self::Verify { path } => { out.push(5); option(out, path.as_deref(), string); }
            Self::Insert { path, place, ty, name, props } => {
                out.push(6);
                string(out, path);
                out.push(*place as u8);
                string(out, ty);
                option(out, name.as_deref(), string);
                out.extend((props.len() as u64).to_le_bytes());
                for (prop, value) in props { string(out, prop); string(out, value); }
            }
            Self::Move { path, target, place } => {
                out.push(7); string(out, path); string(out, target); out.push(*place as u8);
            }
            Self::Step { path, dir } => { out.push(8); string(out, path); out.push(*dir as u8); }
            Self::Delete { path, force_rust_refs } => { out.push(9); string(out, path); out.push(*force_rust_refs as u8); }
            Self::Duplicate { path, name } => { out.push(10); string(out, path); option(out, name.as_deref(), string); }
            Self::Wrap { path, ty, name } => { out.push(11); string(out, path); string(out, ty); option(out, name.as_deref(), string); }
            Self::Rename { path, name, force_rust_refs } => {
                out.push(12); string(out, path); string(out, name); out.push(*force_rust_refs as u8);
            }
            Self::Bake { scope, path, prop } => {
                out.extend([13, *scope as u8]);
                option(out, path.as_deref(), string);
                option(out, prop.as_deref(), string);
            }
            Self::Undo => out.push(14),
            Self::Redo => out.push(15),
            Self::Reset => out.push(16),
            Self::WritePatch { file } => { out.push(17); option(out, file.as_deref(), string); }
            Self::GhostInsert { target, place, ty } => { out.push(18); string(out, target); out.push(*place as u8); string(out, ty); }
            Self::GhostMove { source, target, place } => { out.push(19); string(out, source); string(out, target); out.push(*place as u8); }
            Self::GhostCommit => out.push(20),
            Self::GhostCancel => out.push(21),
        }
    }

    fn decode(r: &mut Reader) -> Result<Option<Self>, &'static str> {
        Ok(Some(match r.bytes()? {
            [1] => Self::State { path: r.optional_string(MAX_DESIGN_PATH_LEN)? },
            [2] => Self::Palette { query: r.optional_string(MAX_DESIGN_QUERY_LEN)?,
                offset: u32::from_le_bytes(r.bytes()?), limit: u16::from_le_bytes(r.bytes()?) },
            [3] => Self::DropTarget { window: r.size()?, x: r.float()?, y: r.float()?, exclude: r.optional_string(MAX_DESIGN_PATH_LEN)? },
            [4] => Self::Diff { file: r.optional_string(MAX_DESIGN_PATH_LEN)?,
                offset: u32::from_le_bytes(r.bytes()?), limit: u32::from_le_bytes(r.bytes()?) },
            [5] => Self::Verify { path: r.optional_string(MAX_DESIGN_PATH_LEN)? },
            [6] => Self::Insert { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, place: r.design_place()?, ty: r.bounded_string(MAX_DESIGN_TY_LEN)?,
                name: r.optional_string(MAX_DESIGN_NAME_LEN)?, props: r.design_props()? },
            [7] => Self::Move { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, target: r.bounded_string(MAX_DESIGN_PATH_LEN)?, place: r.design_place()? },
            [8] => Self::Step { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, dir: match r.bytes()? {
                [1] => StepDir::Up, [2] => StepDir::Down, [3] => StepDir::Out, _ => return Err("invalid step direction"),
            } },
            [9] => Self::Delete { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, force_rust_refs: r.flag()? },
            [10] => Self::Duplicate { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, name: r.optional_string(MAX_DESIGN_NAME_LEN)? },
            [11] => Self::Wrap { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, ty: r.bounded_string(MAX_DESIGN_TY_LEN)?, name: r.optional_string(MAX_DESIGN_NAME_LEN)? },
            [12] => Self::Rename { path: r.bounded_string(MAX_DESIGN_PATH_LEN)?, name: r.bounded_string(MAX_DESIGN_NAME_LEN)?, force_rust_refs: r.flag()? },
            [13] => Self::Bake { scope: match r.bytes()? {
                [1] => BakeScope::Props, [2] => BakeScope::Theme, [3] => BakeScope::All, _ => return Err("invalid bake scope"),
            }, path: r.optional_string(MAX_DESIGN_PATH_LEN)?, prop: r.optional_string(MAX_DESIGN_PROP_LEN)? },
            [14] => Self::Undo,
            [15] => Self::Redo,
            [16] => Self::Reset,
            [17] => Self::WritePatch { file: r.optional_string(MAX_DESIGN_PATH_LEN)? },
            [18] => Self::GhostInsert { target: r.bounded_string(MAX_DESIGN_PATH_LEN)?, place: r.design_place()?, ty: r.bounded_string(MAX_DESIGN_TY_LEN)? },
            [19] => Self::GhostMove { source: r.bounded_string(MAX_DESIGN_PATH_LEN)?, target: r.bounded_string(MAX_DESIGN_PATH_LEN)?, place: r.design_place()? },
            [20] => Self::GhostCommit,
            [21] => Self::GhostCancel,
            _ => return Ok(None),
        }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DesignNotice {
    Status(String),
    Reply { kind: DesignReplyKind, json: String },
    Mutated { op: String, path: String, focus_path: Option<String>, seq: u64, summary: String },
    Running { summary: String },
}

impl DesignNotice {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Status(json) => { out.push(1); string(out, json); }
            Self::Reply { kind, json } => { out.extend([2, *kind as u8]); string(out, json); }
            Self::Mutated { op, path, focus_path, seq, summary } => {
                out.push(3);
                string(out, op);
                string(out, path);
                option(out, focus_path.as_deref(), string);
                out.extend(seq.to_le_bytes());
                string(out, summary);
            }
            Self::Running { summary } => { out.push(4); string(out, summary); }
        }
    }

    fn decode(r: &mut Reader) -> Result<Option<Self>, &'static str> {
        Ok(Some(match r.bytes()? {
            [1] => Self::Status(r.string()?),
            [2] => {
                let kind = match r.bytes()? {
                    [1] => DesignReplyKind::State, [2] => DesignReplyKind::Palette, [3] => DesignReplyKind::DropTarget,
                    [4] => DesignReplyKind::Diff, [5] => DesignReplyKind::Verify, [6] => DesignReplyKind::WritePatch,
                    _ => return Ok(None),
                };
                Self::Reply { kind, json: r.string()? }
            }
            [3] => {
                let op = r.bounded_string(32)?;
                if op.is_empty() || op.chars().any(char::is_control) { return Err("invalid design operation") }
                let path = r.bounded_string(MAX_DESIGN_PATH_LEN)?;
                check_path(&path)?;
                let focus_path = r.optional_string(MAX_DESIGN_PATH_LEN)?;
                if let Some(p) = &focus_path { check_path(p)?; }
                let seq = r.integer()?;
                if seq == 0 { return Err("invalid edit sequence") }
                let summary = r.bounded_string(MAX_DESIGN_SUMMARY_LEN)?;
                if summary.chars().any(char::is_control) { return Err("invalid design summary") }
                Self::Mutated { op, path, focus_path, seq, summary }
            }
            [4] => {
                let summary = r.bounded_string(MAX_DESIGN_SUMMARY_LEN)?;
                if summary.is_empty() || summary.chars().any(char::is_control) { return Err("invalid design summary") }
                Self::Running { summary }
            }
            _ => return Ok(None),
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThemeVal { Color(u32), Num(f64) }

#[derive(Clone, Debug, PartialEq)]
pub struct ThemeInstall {
    pub label: String,
    pub base: String,
    pub sheet: Option<String>,
    pub globals: Vec<(String, ThemeVal)>,
    pub overrides: Vec<(String, ThemeVal)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ThemeOp {
    Get,
    Set { name: String, value: String },
    Install(ThemeInstall),
}

impl ThemeOp {
    fn validate(&self) -> Result<(), &'static str> {
        let text = |s: &str| !s.trim().is_empty() && !s.chars().any(char::is_control);
        match self {
            Self::Get => Ok(()),
            Self::Set { name, .. } if is_identifier(name) => Ok(()),
            Self::Set { .. } => Err("invalid theme token name"),
            Self::Install(spec) => {
                if !matches!(spec.base.as_str(), "dark" | "light" | "skeleton") { return Err("invalid theme base") }
                if !text(&spec.label) || !spec.sheet.as_deref().is_none_or(text) { return Err("invalid theme label or sheet") }
                for (name, val) in spec.globals.iter().chain(&spec.overrides) {
                    if !is_identifier(name) { return Err("invalid theme token name") }
                    if matches!(val, ThemeVal::Num(n) if !n.is_finite()) { return Err("invalid theme number") }
                }
                Ok(())
            }
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Get => out.push(1),
            Self::Set { name, value } => { out.push(2); string(out, name); string(out, value); }
            Self::Install(spec) => {
                out.push(3);
                string(out, &spec.label);
                string(out, &spec.base);
                option(out, spec.sheet.as_deref(), string);
                for entries in [&spec.globals, &spec.overrides] {
                    out.extend((entries.len() as u64).to_le_bytes());
                    for (name, value) in entries {
                        string(out, name);
                        match value {
                            ThemeVal::Color(c) => { out.push(1); out.extend(c.to_le_bytes()); }
                            ThemeVal::Num(n) => { out.push(2); out.extend(n.to_le_bytes()); }
                        }
                    }
                }
            }
        }
    }

    fn decode(r: &mut Reader) -> Result<Option<Self>, &'static str> {
        Ok(Some(match r.bytes()? {
            [1] => Self::Get,
            [2] => Self::Set { name: r.string()?, value: r.string()? },
            [3] => Self::Install(ThemeInstall {
                label: r.string()?, base: r.string()?, sheet: r.path()?,
                globals: r.theme_entries()?, overrides: r.theme_entries()?,
            }),
            _ => return Ok(None),
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeQuery {
    Children { path: Option<String>, depth: u8, offset: u32, limit: u16 },
    Reveal { path: String, limit: u16 },
    Search { query: String, offset: u32, limit: u16 },
}

impl TreeQuery {
    fn validate(&self) -> Result<(), &'static str> {
        let limit = match self {
            Self::Children { path, depth, limit, .. } => {
                if *depth == 0 { return Err("tree depth must be at least 1") }
                if let Some(path) = path { check_path(path)?; }
                limit
            }
            Self::Reveal { path, limit } => { check_path(path)?; limit }
            Self::Search { query, limit, .. } => {
                if query.trim().is_empty() || query.chars().any(char::is_control) { return Err("invalid widget tree query") }
                limit
            }
        };
        if *limit == 0 { Err("tree limit must be at least 1") } else { Ok(()) }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Children { path, depth, offset, limit } => {
                out.push(1);
                option(out, path.as_deref(), string);
                out.push(*depth);
                out.extend(offset.to_le_bytes());
                out.extend(limit.to_le_bytes());
            }
            Self::Reveal { path, limit } => {
                out.push(2);
                string(out, path);
                out.extend(limit.to_le_bytes());
            }
            Self::Search { query, offset, limit } => {
                out.push(3);
                string(out, query);
                out.extend(offset.to_le_bytes());
                out.extend(limit.to_le_bytes());
            }
        }
    }

    /// An unknown query kind from a newer copy is skipped, like an unknown tweak tag.
    fn decode(r: &mut Reader) -> Result<Option<Self>, &'static str> {
        Ok(Some(match r.bytes()? {
            [1] => Self::Children { path: r.path()?, depth: r.bytes::<1>()?[0],
                offset: u32::from_le_bytes(r.bytes()?), limit: u16::from_le_bytes(r.bytes()?) },
            [2] => Self::Reveal { path: r.string()?, limit: u16::from_le_bytes(r.bytes()?) },
            [3] => Self::Search { query: r.string()?, offset: u32::from_le_bytes(r.bytes()?), limit: u16::from_le_bytes(r.bytes()?) },
            _ => return Ok(None),
        }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Pick(At), Props { path: String }, Set { path: String, prop: String, value: String },
    Tree(TreeQuery),
    Theme(ThemeOp),
    Design(DesignRequest),
    Select(Option<String>), AskEdit, Look, LookDone { look: RequestId, ok: bool },
}
fn check_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.chars().any(char::is_control) { Err("invalid widget path") } else { Ok(()) }
}

fn check_prop(prop: &str) -> Result<(), &'static str> {
    if prop.split('.').all(is_identifier) { Ok(()) } else { Err("invalid property path") }
}

impl Request {
    pub(super) fn min_level(&self) -> u16 { min_level(&[1, self.tag()]) }

    fn tag(&self) -> u8 {
        match self {
            Self::Pick(At::Point { .. }) => 1, Self::Pick(At::Path(_)) => 2, Self::Props { .. } => 3,
            Self::Set { .. } => 4, Self::Select(_) => 5, Self::AskEdit => 6,
            Self::Look => 21, Self::LookDone { .. } => 22,
            Self::Tree(_) => TAG_REQUEST_TREE, Self::Theme(_) => TAG_REQUEST_THEME,
            Self::Design(_) => TAG_REQUEST_DESIGN,
        }
    }

    fn encode_fields(&self, out: &mut Vec<u8>) {
        match self {
            Self::Pick(At::Point { window, x, y }) => {
                out.extend((*window as u64).to_le_bytes());
                out.extend(x.to_le_bytes());
                out.extend(y.to_le_bytes());
            }
            Self::Pick(At::Path(path)) | Self::Props { path } => string(out, path),
            Self::Set { path, prop, value } => for s in [path, prop, value] { string(out, s) },
            Self::Select(path) => option(out, path.as_deref(), string),
            Self::LookDone { look, ok } => { out.extend(look.to_le_bytes()); out.push(*ok as u8); }
            Self::AskEdit | Self::Look => {}
            Self::Tree(query) => query.encode(out),
            Self::Theme(op) => op.encode(out),
            Self::Design(request) => request.encode(out),
        }
    }

    fn decode(tag: u8, r: &mut Reader) -> Result<Option<Self>, &'static str> {
        let request = match tag {
            1 => Self::Pick(At::Point { window: r.size()?, x: r.float()?, y: r.float()? }),
            2 => Self::Pick(At::Path(r.string()?)), 3 => Self::Props { path: r.string()? },
            4 => Self::Set { path: r.string()?, prop: r.string()?, value: r.string()? },
            5 => Self::Select(r.path()?), 6 => Self::AskEdit,
            TAG_REQUEST_DESIGN => {
                let Some(request) = DesignRequest::decode(r)? else { return Ok(None) };
                Self::Design(request)
            }
            TAG_REQUEST_THEME => {
                let Some(op) = ThemeOp::decode(r)? else { return Ok(None) };
                Self::Theme(op)
            }
            TAG_REQUEST_TREE => {
                let Some(query) = TreeQuery::decode(r)? else { return Ok(None) };
                Self::Tree(query)
            }
            21 => Self::Look, 22 => Self::LookDone { look: u128::from_le_bytes(r.bytes()?), ok: r.flag()? },
            _ => return Ok(None),
        };
        request.validate()?;
        Ok(Some(request))
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let path = match self {
            Self::Tree(query) => return query.validate(),
            Self::Theme(op) => return op.validate(),
            Self::Design(request) => return request.validate(),
            Self::Pick(At::Point { x, y, .. }) => return if x.is_finite() && y.is_finite() { Ok(()) } else { Err("invalid pick coordinate") },
            Self::Pick(At::Path(path)) | Self::Props { path } | Self::Select(Some(path)) => path,
            Self::Set { path, prop, .. } => {
                check_prop(prop)?;
                path
            }
            Self::Select(None) | Self::AskEdit | Self::Look | Self::LookDone { .. } => return Ok(()),
        };
        check_path(path)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    Asked, Allowed(bool),
    Running(Request), Stopped(bool), Looked,
    Picture { png: Arc<[u8]>, size: [u32; 2] },
    Window { window: usize, size: [f64; 2] },
    Ended(String),
    Picked { path: String, ty: String, rect: Rect },
    Rows { path: String, rows: String },
    Tree { query: TreeQuery, tree: String },
    Theme { theme: String },
    Design(DesignNotice),
    Applied { path: String, prop: String, old: String, new: String, seq: u64 },
    Refused(String),
    Selected { path: Option<String>, rect: Option<Rect> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Request(Request),
    Notice { seat: Seat, delivery: u64, notice: Notice },
    Ack(u64),
    Ready,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub share: SessionId,
    pub sender: Seat,
    pub req: RequestId,
    pub agent: Option<Id>,
    pub body: Body,
}

fn string(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}
fn option<T>(out: &mut Vec<u8>, v: Option<T>, put: impl FnOnce(&mut Vec<u8>, T)) {
    out.push(v.is_some() as u8);
    if let Some(v) = v { put(out, v) }
}
fn rect(out: &mut Vec<u8>, r: Rect) {
    out.extend((r.window as u64).to_le_bytes());
    for n in [r.x, r.y, r.w, r.h] { out.extend(n.to_le_bytes()) }
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let tag = match &self.body {
            Body::Request(request) => request.tag(),
            Body::Notice { notice, .. } => match notice {
                Notice::Asked => 7, Notice::Allowed(_) => 8, Notice::Picked { .. } => 9, Notice::Rows { .. } => 10,
                Notice::Window { .. } => 16, Notice::Ended(_) => 17,
                Notice::Tree { .. } => TAG_NOTICE_TREE, Notice::Theme { .. } => TAG_NOTICE_THEME,
                Notice::Design(_) => TAG_NOTICE_DESIGN,
                Notice::Running(_) => 19, Notice::Stopped(_) => 20, Notice::Looked => 23, Notice::Picture { .. } => 24,
                Notice::Applied { .. } => 11, Notice::Refused(_) => 12, Notice::Selected { .. } => 13,
            },
            Body::Ack(_) => 14, Body::Ready => 18,
        };
        let mut out = vec![1, tag];
        out.extend(self.share);
        out.extend(self.sender);
        out.extend(self.req.to_le_bytes());
        option(&mut out, self.agent, |out, id| out.extend(id));
        match &self.body {
            Body::Request(request) => request.encode_fields(&mut out),
            Body::Notice { seat, delivery, notice } => {
                out.extend(seat);
                out.extend(delivery.to_le_bytes());
                match notice {
                    Notice::Theme { theme } => string(&mut out, theme),
                    Notice::Design(notice) => notice.encode(&mut out),
                    Notice::Ended(why) => string(&mut out, why),
                    Notice::Window { window, size } => {
                        out.extend((*window as u64).to_le_bytes());
                        for side in size { out.extend(side.to_le_bytes()); }
                    }
                    Notice::Picture { png, size } => {
                        for side in size { out.extend(side.to_le_bytes()); }
                        out.extend((png.len() as u64).to_le_bytes());
                        out.extend_from_slice(png);
                    }
                    Notice::Asked | Notice::Looked => {}
                    Notice::Allowed(yes) | Notice::Stopped(yes) => out.push(*yes as u8),
                    Notice::Running(request) => {
                        debug_assert!(!matches!(request, Request::Design(_)));
                        out.push(request.tag());
                        request.encode_fields(&mut out);
                    }
                    Notice::Picked { path, ty, rect: r } => { string(&mut out, path); string(&mut out, ty); rect(&mut out, *r); }
                    Notice::Rows { path, rows } => { string(&mut out, path); string(&mut out, rows); }
                    Notice::Tree { query, tree } => { query.encode(&mut out); string(&mut out, tree); }
                    Notice::Applied { path, prop, old, new, seq } => {
                        for s in [path, prop, old, new] { string(&mut out, s) }
                        out.extend(seq.to_le_bytes());
                    }
                    Notice::Refused(why) => string(&mut out, why),
                    Notice::Selected { path, rect: r } => { option(&mut out, path.as_deref(), string); option(&mut out, *r, rect); }
                }
            }
            Body::Ready => {}
            Body::Ack(delivery) => out.extend(delivery.to_le_bytes()),
        }
        out
    }
}

struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        let (head, tail) = self.0.split_at_checked(N).ok_or("truncated body")?;
        self.0 = tail;
        Ok(head.try_into().unwrap())
    }
    fn flag(&mut self) -> Result<bool, &'static str> {
        match self.bytes()? { [0] => Ok(false), [1] => Ok(true), _ => Err("invalid flag") }
    }
    fn integer(&mut self) -> Result<u64, &'static str> { self.bytes().map(u64::from_le_bytes) }
    fn size(&mut self) -> Result<usize, &'static str> { self.integer()?.try_into().map_err(|_| "overlong count") }
    fn float(&mut self) -> Result<f64, &'static str> {
        let n = f64::from_le_bytes(self.bytes()?);
        if n.is_finite() { Ok(n) } else { Err("invalid coordinate") }
    }
    fn string(&mut self) -> Result<String, &'static str> {
        self.bounded_string(usize::MAX)
    }
    fn bounded_string(&mut self, max: usize) -> Result<String, &'static str> {
        let n = self.size()?;
        if n > max { return Err("overlong string") }
        let (head, tail) = self.0.split_at_checked(n).ok_or("overlong string")?;
        self.0 = tail;
        Ok(std::str::from_utf8(head).map_err(|_| "invalid UTF-8")?.to_owned())
    }
    fn optional_string(&mut self, max: usize) -> Result<Option<String>, &'static str> {
        if self.flag()? { self.bounded_string(max).map(Some) } else { Ok(None) }
    }
    fn path(&mut self) -> Result<Option<String>, &'static str> { self.optional_string(usize::MAX) }
    fn design_place(&mut self) -> Result<DesignPlace, &'static str> {
        match self.bytes()? {
            [1] => Ok(DesignPlace::Before), [2] => Ok(DesignPlace::Inside), [3] => Ok(DesignPlace::After),
            _ => Err("invalid design place"),
        }
    }
    fn design_props(&mut self) -> Result<Vec<(String, String)>, &'static str> {
        let n = self.size()?;
        // Each pair has two eight-byte string lengths, before either string's contents.
        if n > MAX_DESIGN_PROPS || n > self.0.len() / 16 { return Err("overlong design props count") }
        let mut props = Vec::with_capacity(n);
        for _ in 0..n {
            props.push((self.bounded_string(MAX_DESIGN_PROP_LEN)?, self.bounded_string(MAX_DESIGN_VALUE_LEN)?));
        }
        Ok(props)
    }
    fn theme_entries(&mut self) -> Result<Vec<(String, ThemeVal)>, &'static str> {
        let n = self.size()?;
        // Even an empty name and the smallest value take 8 + 1 + 4 bytes.
        if n > self.0.len() / 13 { return Err("overlong theme entry count") }
        let mut entries = Vec::new();
        for _ in 0..n {
            let name = self.string()?;
            let value = match self.bytes()? {
                [1] => ThemeVal::Color(u32::from_le_bytes(self.bytes()?)),
                [2] => ThemeVal::Num(f64::from_le_bytes(self.bytes()?)),
                _ => return Err("invalid theme value kind"),
            };
            entries.push((name, value));
        }
        Ok(entries)
    }
    fn rect(&mut self) -> Result<Rect, &'static str> {
        let r = Rect { window: self.size()?, x: self.float()?, y: self.float()?, w: self.float()?, h: self.float()? };
        if r.valid() { Ok(r) } else { Err("invalid rect") }
    }
}

pub(super) fn validate_rows(rows: &str) -> Result<(), &'static str> {
    let parsed = super::parse_json(rows)?;
    if super::app::rows(&parsed) { Ok(()) } else { Err("invalid property rows") }
}

pub fn decode(peer: Seat, bytes: &[u8]) -> Result<Option<Packet>, &'static str> {
    let Some(packet) = decode_envelope(peer, bytes)? else { return Ok(None) };
    if let Body::Notice { notice: Notice::Rows { rows, .. }, .. } = &packet.body { validate_rows(rows)?; }
    if let Body::Notice { notice: Notice::Tree { tree, .. }, .. } = &packet.body { validate_tree(tree)?; }
    if let Body::Notice { notice: Notice::Theme { theme }, .. } = &packet.body { validate_theme(theme)?; }
    if let Body::Notice { notice: Notice::Design(DesignNotice::Status(json) | DesignNotice::Reply { json, .. }), .. } = &packet.body {
        validate_design_json(json)?;
    }
    Ok(Some(packet))
}

pub(super) fn validate_design_json(json: &str) -> Result<(), &'static str> {
    super::parse_json(json).map(|_| ())
}

pub(super) fn validate_theme(theme: &str) -> Result<(), &'static str> {
    if super::app::valid_theme(&super::parse_json(theme)?) { Ok(()) } else { Err("invalid theme snapshot") }
}

pub(super) fn validate_tree(tree: &str) -> Result<(), &'static str> {
    if super::app::valid_tree(&super::parse_json(tree)?) { Ok(()) } else { Err("invalid widget tree") }
}

// The live receiver validates Rows and Tree after routing and deduplication so a refused delivery
// still answers its request, advances its acknowledgement and logs once across replays.
pub(super) fn decode_envelope(peer: Seat, bytes: &[u8]) -> Result<Option<Packet>, &'static str> {
    let mut r = Reader(bytes);
    if r.bytes::<1>()? != [1] { return Err("unknown tweak version") }
    let [tag] = r.bytes()?;
    let share = r.bytes()?;
    if r.bytes()? != peer { return Err("sender differs from authenticated peer") }
    let req = u128::from_le_bytes(r.bytes()?);
    let agent = if r.flag()? { Some(r.bytes()?) } else { None };
    let body = match tag {
        1..=6 | 21 | 22 | TAG_REQUEST_TREE | TAG_REQUEST_THEME | TAG_REQUEST_DESIGN => {
            let Some(request) = Request::decode(tag, &mut r)? else { return Ok(None) };
            Body::Request(request)
        }
        7..=13 | 16 | 17 | 19 | 20 | 23 | 24 | TAG_NOTICE_TREE | TAG_NOTICE_THEME | TAG_NOTICE_DESIGN => {
            let seat = r.bytes()?;
            let delivery = r.integer()?;
            if delivery == 0 { return Err("invalid delivery id") }
            let notice = match tag {
                7 => Notice::Asked, 8 => Notice::Allowed(r.flag()?),
                9 => Notice::Picked { path: r.string()?, ty: r.string()?, rect: r.rect()? },
                10 => Notice::Rows { path: r.string()?, rows: r.string()? },
                11 => Notice::Applied { path: r.string()?, prop: r.string()?, old: r.string()?, new: r.string()?, seq: r.integer()? },
                12 => Notice::Refused(r.string()?),
                16 => {
                    let window = r.size()?;
                    let size = [r.float()?, r.float()?];
                    if size.iter().any(|n| *n <= 0.) { return Err("invalid share dimensions") }
                    Notice::Window { window, size }
                }
                17 => Notice::Ended(r.string()?),
                TAG_NOTICE_THEME => Notice::Theme { theme: r.string()? },
                TAG_NOTICE_DESIGN => {
                    let Some(notice) = DesignNotice::decode(&mut r)? else { return Ok(None) };
                    Notice::Design(notice)
                }
                TAG_NOTICE_TREE => {
                    let Some(query) = TreeQuery::decode(&mut r)? else { return Ok(None) };
                    query.validate()?;
                    Notice::Tree { query, tree: r.string()? }
                }
                19 => {
                    let [tag] = r.bytes()?;
                    let Some(request) = Request::decode(tag, &mut r)? else { return Ok(None) };
                    if matches!(request, Request::Tree(_) | Request::Theme(_) | Request::Design(_) | Request::AskEdit | Request::Select(_) | Request::LookDone { .. }) { return Err("request cannot run in the app") }
                    if request == Request::Look && agent.is_none() { return Err("a look needs an AI") }
                    Notice::Running(request)
                }
                20 => {
                    if agent.is_none() { return Err("stopped notice needs an AI") }
                    Notice::Stopped(r.flag()?)
                }
                23 => {
                    if agent.is_none() { return Err("a look needs an AI") }
                    Notice::Looked
                }
                24 => {
                    if agent.is_none() { return Err("a picture needs an AI") }
                    let size = [u32::from_le_bytes(r.bytes()?), u32::from_le_bytes(r.bytes()?)];
                    // PNG dimensions are positive 31-bit integers.
                    if size.iter().any(|&n| n == 0 || n > i32::MAX as u32) { return Err("invalid picture dimensions") }
                    let n = r.size()?;
                    if n == 0 || n > MAX_RESULT_IMAGE_BYTES / 4 * 3 { return Err("invalid picture byte count") }
                    let (png, tail) = r.0.split_at_checked(n).ok_or("truncated picture")?;
                    r.0 = tail;
                    Notice::Picture { png: Arc::from(png), size }
                }
                _ => {
                    let path = r.path()?;
                    let rect = if r.flag()? { Some(r.rect()?) } else { None };
                    if path.is_none() && rect.is_some() { return Err("cleared selection has a rect") }
                    Notice::Selected { path, rect }
                }
            };
            match &notice {
                Notice::Picked { path, ty, .. } => {
                    check_path(path)?;
                    if ty.is_empty() || ty.chars().any(char::is_control) { return Err("invalid widget type") }
                }
                Notice::Rows { path, .. } | Notice::Selected { path: Some(path), .. } => check_path(path)?,
                Notice::Applied { path, prop, seq, .. } => {
                    check_path(path)?;
                    check_prop(prop)?;
                    if *seq == 0 { return Err("invalid edit sequence") }
                }
                _ => {}
            }
            Body::Notice { seat, delivery, notice }
        }
        14 => Body::Ack(r.integer()?),
        18 => Body::Ready,
        _ => return Ok(None),
    };
    makepad_town_core::tlv::skip_tlv_tail(&mut r.0)?;
    Ok(Some(Packet { share, sender: peer, req, agent, body }))
}
