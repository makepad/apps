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

pub(super) fn min_level(bytes: &[u8]) -> u16 {
    match bytes {
        [1, TAG_REQUEST_TREE | TAG_NOTICE_TREE, ..] => super::TREE_LEVEL,
        [1, TAG_REQUEST_THEME | TAG_NOTICE_THEME, ..] => super::THEME_LEVEL,
        _ => 0,
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
        }
    }

    fn decode(tag: u8, r: &mut Reader) -> Result<Option<Self>, &'static str> {
        let request = match tag {
            1 => Self::Pick(At::Point { window: r.size()?, x: r.float()?, y: r.float()? }),
            2 => Self::Pick(At::Path(r.string()?)), 3 => Self::Props { path: r.string()? },
            4 => Self::Set { path: r.string()?, prop: r.string()?, value: r.string()? },
            5 => Self::Select(r.path()?), 6 => Self::AskEdit,
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
                    Notice::Running(request) => { out.push(request.tag()); request.encode_fields(&mut out); }
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
        let n = self.size()?;
        let (head, tail) = self.0.split_at_checked(n).ok_or("overlong string")?;
        self.0 = tail;
        Ok(std::str::from_utf8(head).map_err(|_| "invalid UTF-8")?.to_owned())
    }
    fn path(&mut self) -> Result<Option<String>, &'static str> { if self.flag()? { self.string().map(Some) } else { Ok(None) } }
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
    Ok(Some(packet))
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
        1..=6 | 21 | 22 | TAG_REQUEST_TREE | TAG_REQUEST_THEME => {
            let Some(request) = Request::decode(tag, &mut r)? else { return Ok(None) };
            Body::Request(request)
        }
        7..=13 | 16 | 17 | 19 | 20 | 23 | 24 | TAG_NOTICE_TREE | TAG_NOTICE_THEME => {
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
                TAG_NOTICE_TREE => {
                    let Some(query) = TreeQuery::decode(&mut r)? else { return Ok(None) };
                    query.validate()?;
                    Notice::Tree { query, tree: r.string()? }
                }
                19 => {
                    let [tag] = r.bytes()?;
                    let Some(request) = Request::decode(tag, &mut r)? else { return Ok(None) };
                    if matches!(request, Request::Tree(_) | Request::Theme(_) | Request::AskEdit | Request::Select(_) | Request::LookDone { .. }) { return Err("request cannot run in the app") }
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
