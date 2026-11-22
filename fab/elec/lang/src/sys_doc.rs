//! Literal system documents: links, unit mounts and stable link identities, without a VM.
//!
//! Cost: one syntax parse, linear component-path lookups per end and mount-graph walks on
//! the evaluation worker. No work runs per frame and no component ids survive re-lowering.

use crate::board_doc::{angle, length, read_angle, read_length, read_uint, units as board_units};
use crate::map::SourceMap;
use crate::source::located;
use fab_elec_core::{Angle, BoardId, CompRef, Diag, DocId, LinkId, PinIx, PinRef, Sel, Xform3};
use fab_elec_design::{Design, Frame, Link, LinkEnd, LinkKind, Mount, PinMap, Unit};
use fab_elec_parts::PartDef;
use makepad_script::syntax::{self, write::quote, Field, Kind, Node};
use makepad_script::units::UnitTable;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Link identities from the source map, at their current statement starts.
#[derive(Clone, Debug, Default)]
pub struct SysLedger {
    pub links: Vec<(u32, LinkId)>,
}

impl SysLedger {
    pub fn of_map(map: &SourceMap, doc: DocId) -> SysLedger {
        let links = map.of_doc(doc).into_iter().filter_map(|(sel, range)| match sel {
            Sel::Link(id) => Some((range.span.start, id)),
            _ => None,
        }).collect();
        SysLedger { links }
    }
}

/// The next link identity: zero for no links, otherwise one above the greatest identity.
/// Shared by document lowering and callers that build `SysOp::AddLink`.
pub fn next_link_id(ids: impl IntoIterator<Item = LinkId>) -> LinkId {
    LinkId(ids.into_iter().map(|id| id.0 + 1).max().unwrap_or(0))
}

/// Successfully resolved declarations, including a default mount for every unmounted unit.
#[derive(Debug)]
pub struct SysLowered {
    pub links: Vec<Link>,
    pub mounts: Vec<Mount>,
    pub spans: Vec<(Sel, Range<u32>)>,
    pub diags: Vec<Diag>,
}

/// The system document's number units: the board document's, and the frequencies, times and
/// resistances of the `sim` statement's rates, length and jack loads (`crate::sys_sim`).
pub fn units() -> UnitTable {
    board_units().with("Hz", 1.0, 1.0).with("kHz", 1000.0, 1.0).with("s", 1.0, 1.0).with("ms", 1.0, 1000.0)
        .with("mohm", 1.0, 1e3).with("ohm", 1.0, 1.0).with("kohm", 1e3, 1.0).with("Mohm", 1e6, 1.0)
}

pub(crate) fn default_mount(unit: BoardId) -> Mount {
    Mount { unit, parent: Frame::World, xform: Xform3::IDENTITY }
}

fn component(end: LinkEnd) -> CompRef {
    match end {
        LinkEnd::Comp(c) => c,
        LinkEnd::Pin(p) => CompRef { board: p.board, comp: p.comp },
    }
}

fn identifier(name: &str) -> bool {
    syntax::write::key(name).is_ok_and(|s| s == name)
}

fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(", "))
}

fn pin_name(part: &PartDef, pin: PinIx) -> (String, Option<usize>) {
    let name = &part.pin_def(pin).expect("a link names an existing pin").name;
    let repeated = part.pins.iter().filter(|p| p.name == *name).count() > 1;
    let occurrence = part.pins[..pin.ix()].iter().filter(|p| p.name == *name).count();
    (quote(name), repeated.then_some(occurrence))
}

fn unit_name(design: &Design, id: BoardId, bound: &HashSet<String>) -> String {
    let name = &design.unit(id).expect("a mount or link names an existing unit").name;
    if identifier(name) && !bound.contains(name) { name.clone() }
    else { format!("unit({})", quote(name)) }
}

fn part(design: &Design, end: LinkEnd) -> &PartDef {
    let c = component(end);
    &design.unit(c.board).expect("a link names an existing unit").circuit.comps[c.comp.ix()].part
}

pub(crate) fn link_end(design: &Design, end: LinkEnd, bound: &HashSet<String>) -> String {
    let c = component(end);
    let path = &design.unit(c.board).expect("a link names an existing unit").circuit.comps[c.comp.ix()].path;
    let mut text = format!("{}.{path}", unit_name(design, c.board, bound));
    if let LinkEnd::Pin(p) = end {
        let (name, occurrence) = pin_name(part(design, end), p.pin);
        let arg = occurrence.map_or(name.clone(), |i| format!("{name}, {i}"));
        text.push_str(&format!(".pin({arg})"));
    }
    text
}

pub(crate) fn link_pins(design: &Design, link: &Link) -> Option<String> {
    let PinMap::Given(pairs) = &link.pinmap else { return None; };
    let pin = |end, ix| {
        let (name, occurrence) = pin_name(part(design, end), ix);
        occurrence.map_or(name.clone(), |i| array([name, i.to_string()]))
    };
    let pairs = array(pairs.iter().map(|(a, b)| array([pin(link.a, *a), pin(link.b, *b)])));
    Some(format!("{{pins: {pairs}}}"))
}

pub(crate) fn link_kind(kind: LinkKind) -> &'static str {
    match kind { LinkKind::Mate => "mate", LinkKind::Wire => "wire" }
}

/// One canonical link call, without its optional binding or line ending.
pub(crate) fn link_statement(design: &Design, link: &Link, bound: &HashSet<String>) -> String {
    let mut out = format!("{}({}, {}", link_kind(link.kind), link_end(design, link.a, bound), link_end(design, link.b, bound));
    if let Some(pins) = link_pins(design, link) { out.push_str(&format!(", {pins}")); }
    out.push(')');
    out
}

/// Choose and reserve a binding, shared by the writer and a newly bound link's splice.
pub(crate) fn link_name(design: &Design, link: &Link, used: &mut HashSet<String>) -> String {
    let a = component(link.a);
    let path = &design.unit(a.board).expect("a link names an existing unit").circuit.comps[a.comp.ix()].path;
    let segment = path.rsplit('.').next().unwrap_or("link");
    let base = segment.split('[').next().filter(|s| identifier(s)).unwrap_or("link");
    let mut name = base.to_string();
    let mut suffix = 2;
    while used.contains(&name) {
        name = format!("{base}_{suffix}");
        suffix += 1;
    }
    used.insert(name.clone());
    name
}

#[derive(PartialEq)]
pub(crate) enum MountValue {
    One(String),
    Three([String; 3]),
}

impl MountValue {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::One(s) => s.clone(),
            Self::Three(items) => array(items.iter().cloned()),
        }
    }
}

pub(crate) const MOUNT_FIELDS: [&str; 3] = ["on", "at", "rot"];

/// Canonical values in field order; arrays retain their tokens for per-axis splices.
pub(crate) fn mount_fields(design: &Design, mount: &Mount, names: &HashMap<LinkId, String>, bound: &HashSet<String>) -> [Option<MountValue>; 3] {
    let pose = mount.xform.snapped();
    let on = match &mount.parent {
        Frame::World => None,
        Frame::Unit(id) => Some(unit_name(design, *id, bound)),
        Frame::Mate(id) => Some(names[id].clone()),
        Frame::Body { mechanism, body } => Some(format!("body({}, {})", quote(mechanism), quote(body))),
    }.map(MountValue::One);
    let at = (pose.t != [0; 3]).then(|| MountValue::Three(pose.t.map(length)));
    let rot = pose.euler();
    let angle = |v| angle(Angle::from_udeg(i64::from(v)));
    let rot = (rot != [0; 3]).then(|| {
        if rot[0] == 0 && rot[1] == 0 { MountValue::One(angle(rot[2])) }
        else { MountValue::Three(rot.map(angle)) }
    });
    [on, at, rot]
}

pub(crate) fn mount_statement(design: &Design, mount: &Mount, names: &HashMap<LinkId, String>, bound: &HashSet<String>) -> String {
    let fields: Vec<_> = MOUNT_FIELDS.into_iter().zip(mount_fields(design, mount, names, bound))
        .filter_map(|(key, value)| value.map(|v| format!("{key}: {}", v.text()))).collect();
    format!("mount({}, {{{}}})", unit_name(design, mount.unit, bound), fields.join(" "))
}

/// Canonical system text: links in model order, then non-default mounts in unit order.
pub fn write(design: &Design) -> String {
    let mut used: HashSet<String> = design.units.iter().map(|u| u.name.clone()).collect();
    let mut names = HashMap::new();
    for link in &design.links {
        if !design.assembly.mounts.iter().any(|m| m.parent == Frame::Mate(link.id)) { continue; }
        names.insert(link.id, link_name(design, link, &mut used));
    }
    let bound = names.values().cloned().collect();
    let mut out = String::new();
    for link in &design.links {
        if let Some(name) = names.get(&link.id) { out.push_str(&format!("let {name} = ")); }
        out.push_str(&link_statement(design, link, &bound));
        out.push('\n');
    }
    let mut mounts: Vec<_> = design.assembly.mounts.iter().collect();
    mounts.sort_by_key(|m| m.unit);
    for mount in mounts {
        if mount.parent == Frame::World && mount.xform.snapped() == Xform3::IDENTITY { continue; }
        out.push_str(&mount_statement(design, mount, &names, &bound));
        out.push('\n');
    }
    out
}

struct Read<'a> {
    doc: DocId,
    text: &'a str,
    offset: u32,
    units: &'a [Unit],
    names: &'a HashMap<String, Option<LinkId>>,
}

impl<'a> Read<'a> {
    fn span(&self, n: &Node) -> Range<u32> {
        self.offset + n.span.start as u32..self.offset + n.span.end as u32
    }
    fn error(&self, n: &Node, message: impl Into<String>) -> Diag {
        located(self.doc, self.span(n), message)
    }
    fn string<'n>(&self, n: &'n Node) -> Result<&'n str, Diag> {
        n.string().ok_or_else(|| self.error(n, "expected a string"))
    }
    fn uint(&self, n: &Node) -> Result<u32, Diag> {
        read_uint(n, self.text, "index", u32::MAX).map_err(|e| self.error(n, e))
    }
    fn array<'n>(&self, n: &'n Node, count: Option<usize>) -> Result<&'n [Node], Diag> {
        let Kind::Array { items, .. } = &n.kind else { return Err(self.error(n, "expected an array")); };
        if count.is_some_and(|c| items.len() != c) { return Err(self.error(n, format!("expected {} entries", count.unwrap()))); }
        Ok(items)
    }
    fn fields<'n>(&self, n: &'n Node, allowed: &[&str]) -> Result<&'n [Field], Diag> {
        let Kind::Object { ty: None, fields, .. } = &n.kind else { return Err(self.error(n, "options must be an object")); };
        for (i, f) in fields.iter().enumerate() {
            if f.op != ":" || !f.name.as_deref().is_some_and(|k| allowed.contains(&k)) {
                return Err(self.error(n, "unknown option or non-literal option field"));
            }
            if fields[..i].iter().any(|other| other.name == f.name) { return Err(self.error(n, "duplicate option field")); }
        }
        Ok(fields)
    }
    fn named_unit(&self, name: &str, explicit: bool, n: &Node) -> Result<&'a Unit, Diag> {
        if !explicit && self.names.contains_key(name) {
            return Err(self.error(n, format!("{name} is a binding; use unit({}) for the unit", quote(name))));
        }
        self.units.iter().find(|u| u.name == name).ok_or_else(|| self.error(n, format!("no unit has the name {name}")))
    }
    fn unit(&self, n: &Node) -> Result<&'a Unit, Diag> {
        if let Some(name) = n.ident().filter(|s| identifier(s)) { return self.named_unit(name, false, n); }
        if let Some([name]) = n.call("unit") { return self.named_unit(self.string(name)?, true, n); }
        Err(self.error(n, "expected a unit name or unit(string)"))
    }
    fn named_path(&self, name: &str, n: &Node) -> Result<(&'a Unit, String), Diag> {
        let (unit, path) = name.split_once('.').unwrap_or((name, ""));
        if !identifier(unit) || (!path.is_empty() && !path.split('.').all(identifier)) {
            return Err(self.error(n, "expected a unit and component path"));
        }
        Ok((self.named_unit(unit, false, n)?, path.to_string()))
    }
    fn path(&self, n: &Node) -> Result<(&'a Unit, String), Diag> {
        match &n.kind {
            Kind::Ident(name) => self.named_path(name, n),
            Kind::Member { base, name, .. } if identifier(name) => {
                let (unit, path) = self.path(base)?;
                Ok((unit, if path.is_empty() { name.clone() } else { format!("{path}.{name}") }))
            }
            Kind::Index { base, index, .. } => {
                let (unit, path) = self.path(base)?;
                if path.is_empty() { return Err(self.error(n, "an index belongs to a component path")); }
                Ok((unit, format!("{path}[{}]", self.uint(index)?)))
            }
            Kind::Call { .. } if n.callee_path() == Some("unit") => Ok((self.unit(n)?, String::new())),
            _ => Err(self.error(n, "expected a unit and component path")),
        }
    }
    fn pin(&self, part: &PartDef, name: &Node, occurrence: Option<&Node>) -> Result<PinIx, Diag> {
        let text = self.string(name)?;
        let occurrence = occurrence.map(|n| self.uint(n)).transpose()?.unwrap_or(0);
        part.pins.iter().enumerate().filter(|(_, p)| p.name == text).nth(occurrence as usize)
            .map(|(i, _)| PinIx(i as u16))
            .ok_or_else(|| self.error(name, format!("no pin has the name {text} with occurrence {occurrence}")))
    }
    fn end_path(&self, n: &Node) -> Result<(&'a Unit, String), Diag> {
        let path = if let Kind::Call { callee, args, .. } = &n.kind {
            let path = match &callee.kind {
                Kind::Member { base, name, .. } if name == "pin" => self.path(base),
                Kind::Ident(name) if name.ends_with(".pin") => self.named_path(&name[..name.len() - 4], callee),
                _ => return Err(self.error(n, "expected a component or component.pin(string, occurrence)")),
            };
            if args.is_empty() || args.len() > 2 { return Err(self.error(n, "pin takes a name and an optional occurrence")); }
            path
        } else { self.path(n) };
        path.map_err(|e| self.error(n, e.message))
    }
    fn end(&self, n: &Node) -> Result<LinkEnd, Diag> {
        let (unit, path) = self.end_path(n)?;
        let comp = unit.circuit.comp(&path).ok_or_else(|| self.error(n, format!("no component has the path {}.{path}", unit.name)))?;
        let c = CompRef { board: unit.id, comp: comp.id };
        match &n.kind {
            Kind::Call { args, .. } => Ok(LinkEnd::Pin(PinRef { board: c.board, comp: c.comp, pin: self.pin(&comp.part, &args[0], args.get(1))? })),
            _ => Ok(LinkEnd::Comp(c)),
        }
    }
    fn mapped_pin(&self, end: LinkEnd, n: &Node) -> Result<PinIx, Diag> {
        let c = component(end);
        let part = &self.units.iter().find(|u| u.id == c.board).expect("the end resolved").circuit.comps[c.comp.ix()].part;
        if n.string().is_some() { self.pin(part, n, None) }
        else {
            let pair = self.array(n, Some(2))?;
            self.pin(part, &pair[0], Some(&pair[1]))
        }
    }
    fn link(&self, n: &Node, id: LinkId) -> Result<Link, Diag> {
        let (name, args) = n.as_call().expect("a link is a call");
        if args.len() < 2 || args.len() > 3 { return Err(self.error(n, "a link takes two ends and optional pins")); }
        let a = self.end(&args[0])?;
        let b = self.end(&args[1])?;
        let mut pinmap = PinMap::Derived;
        if let Some(opts) = args.get(2) {
            let fields = self.fields(opts, &["pins"])?;
            if let Some(field) = fields.first() {
                let mut pairs = Vec::new();
                for pair in self.array(&field.value, None)? {
                    let pair = self.array(pair, Some(2))?;
                    pairs.push((self.mapped_pin(a, &pair[0])?, self.mapped_pin(b, &pair[1])?));
                }
                pinmap = PinMap::Given(pairs);
            }
        }
        Ok(Link { id, kind: if name == "mate" { LinkKind::Mate } else { LinkKind::Wire }, a, b, pinmap })
    }
    fn mount(&self, n: &Node, links: &[Link]) -> Result<Mount, Diag> {
        let Some([unit, opts]) = n.call("mount") else { return Err(self.error(n, "mount takes a unit and options")); };
        let mut mount = default_mount(self.unit(unit)?.id);
        let mut at = [0; 3];
        let mut rot = [0; 3];
        for field in self.fields(opts, &["on", "at", "rot"])? {
            let n = &field.value;
            match field.name.as_deref() {
                Some("on") => {
                    mount.parent = if let Some([mechanism, body]) = n.call("body") {
                        Frame::Body { mechanism: self.string(mechanism)?.into(), body: self.string(body)?.into() }
                    } else if let Some(id) = n.ident().and_then(|name| self.names.get(name)) {
                        let id = id.filter(|id| links.iter().any(|l| l.id == *id))
                            .ok_or_else(|| self.error(n, "the mount's link does not resolve"))?;
                        Frame::Mate(id)
                    } else { Frame::Unit(self.unit(n)?.id) };
                }
                Some("at") => {
                    for (v, n) in at.iter_mut().zip(self.array(n, Some(3))?) {
                        *v = read_length(n, self.text, "at").map_err(|e| self.error(n, e))?;
                    }
                }
                Some("rot") => {
                    if matches!(n.kind, Kind::Array { .. }) {
                        for (v, n) in rot.iter_mut().zip(self.array(n, Some(3))?) {
                            *v = read_angle(n, self.text, "rot").map_err(|e| self.error(n, e))?.udeg();
                        }
                    } else { rot[2] = read_angle(n, self.text, "rot").map_err(|e| self.error(n, e))?.udeg(); }
                }
                _ => unreachable!("fields were checked"),
            }
        }
        mount.xform = Xform3::from_euler(at, rot).snapped();
        Ok(mount)
    }
}

/// Read an action's end with the same resolution as a system declaration.
pub(crate) fn read_end(text: &str, units: &[Unit], n: &Node) -> Result<LinkEnd, Diag> {
    Read { doc: DocId(0), text, offset: 0, units, names: &HashMap::new() }.end(n)
}

/// Read an action's mount; the supplied binding names an existing link, if any.
pub(crate) fn read_mount(text: &str, units: &[Unit], links: &[Link], names: &HashMap<String, Option<LinkId>>, n: &Node) -> Result<Mount, Diag> {
    Read { doc: DocId(0), text, offset: 0, units, names }.mount(n, links)
}

/// Resolve the written path for a splice, even when its component or pin currently dangles.
pub(crate) fn end_path(doc: DocId, text: &str, units: &[Unit], names: &HashMap<String, Option<LinkId>>, n: &Node) -> Result<(BoardId, String), Diag> {
    let r = Read { doc, text, offset: 0, units, names };
    r.end_path(n).map(|(unit, path)| (unit.id, path))
}

pub(crate) fn declaration<'a>(whole: &'a Node, text: &str) -> (&'a Node, Option<&'a str>) {
    match &whole.kind {
        Kind::Let { keyword, names, pattern_span, value: Some(value) }
            if keyword == "let" && names.len() == 1 && text[pattern_span.clone()] == names[0] => (value, Some(&names[0])),
        _ => (whole, None),
    }
}

/// The mount statements whose unit resolves (the reader's resolution, including `unit("..")`
/// roots), each with the diag that keeps it from being edited (malformed options). A statement
/// whose unit does not resolve is left out: it dangles with its diag, as lowering skips it.
pub(crate) fn mount_statements<'n>(doc: DocId, text: &str, units: &[Unit], statements: &'n [Node], bound: &HashSet<String>) -> Vec<(BoardId, &'n Node, Option<Diag>)> {
    let names = bound.iter().map(|name| (name.clone(), None)).collect();
    let r = Read { doc, text, offset: 0, units, names: &names };
    let mut mounts = Vec::new();
    for n in statements.iter().filter(|n| n.callee_path() == Some("mount")) {
        let Some([unit, opts]) = n.call("mount") else { continue; };
        let Ok(unit) = r.unit(unit) else { continue; };
        mounts.push((unit.id, n, r.fields(opts, &MOUNT_FIELDS).err()));
    }
    mounts
}

fn mount_loop(unit: BoardId, mounts: &[Mount], links: &[Link]) -> bool {
    let mut pending = vec![unit];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) { continue; }
        let Some(mount) = mounts.iter().find(|m| m.unit == id) else { continue; };
        let parents = match mount.parent {
            Frame::Unit(parent) => vec![parent],
            Frame::Mate(link) => {
                let link = links.iter().find(|l| l.id == link).expect("the mount's link resolved");
                let (a, b) = (component(link.a).board, component(link.b).board);
                // A mounted end depends on the opposite end. A third unit depends on both.
                if a == id { vec![b] } else if b == id { vec![a] } else { vec![a, b] }
            }
            Frame::World | Frame::Body { .. } => vec![],
        };
        if parents.contains(&unit) { return true; }
        pending.extend(parents);
    }
    false
}

/// Lower declarations independently. Even an unresolved link reserves its identity and span.
pub fn lower(doc: DocId, text: &str, units: &[Unit], ledger: Option<&SysLedger>) -> SysLowered {
    let mut out = SysLowered {
        links: vec![], mounts: units.iter().map(|u| default_mount(u.id)).collect(), spans: vec![], diags: vec![],
    };
    out.mounts.sort_by_key(|m| m.unit);
    let table = self::units();
    let mut chunks = Vec::new();
    match syntax::parse_with(text, &table) {
        Ok(tree) => chunks.push((0, text, tree)),
        Err(_) => {
            // Canonical statements occupy a line; a malformed line must not hide the rest.
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                match syntax::parse_with(line, &table) {
                    Ok(tree) => chunks.push((offset, line, tree)),
                    // a sim line's syntax error is the settings reader's diag (crate::sys_sim::read)
                    Err(_) if crate::sys_sim::is_sim_line(line) => {}
                    Err(e) => {
                        let start = offset + e.at as u32;
                        let size = line.get(e.at..).and_then(|s| s.chars().next()).map_or(0, |c| c.len_utf8()) as u32;
                        out.diags.push(located(doc, start..start + size, format!("syntax: {}", e.message)));
                    }
                }
                offset += line.len() as u32;
            }
        }
    }
    let old: HashMap<_, _> = ledger.into_iter().flat_map(|l| l.links.iter().copied()).collect();
    let mut next = next_link_id(old.values().copied()).0;
    let mut ids = HashMap::new();
    let mut names = HashMap::new();
    let mut duplicates = HashSet::new();
    for (offset, text, tree) in &chunks {
        for whole in tree.statements() {
            let (n, _) = declaration(whole, text);
            let start = offset + whole.span.start as u32;
            let id = matches!(n.callee_path(), Some("mate" | "wire")).then(|| {
                let id = old.get(&start).copied().unwrap_or_else(|| { let id = LinkId(next); next += 1; id });
                ids.insert(start, id);
                out.spans.push((Sel::Link(id), start..offset + whole.span.end as u32));
                id
            });
            if let Kind::Let { keyword, names: bound, .. } = &whole.kind {
                if keyword != "let" { continue; }
                for name in bound {
                    if names.contains_key(name) {
                        duplicates.insert(start);
                    } else { names.insert(name.clone(), id); }
                }
            }
        }
    }
    let mut pending = Vec::new();
    let mut mounted = HashSet::new();
    let mut repeated = HashSet::new();
    for (offset, text, tree) in &chunks {
        let r = Read { doc, text, offset: *offset, units, names: &names };
        for whole in tree.statements() {
            let (n, binding) = declaration(whole, text);
            let start = r.span(whole).start;
            if duplicates.contains(&start) {
                out.diags.push(r.error(whole, "a link needs a distinct, single binding"));
            } else if let Some(id) = ids.get(&start) {
                match r.link(n, *id) {
                    Ok(link) => out.links.push(link),
                    Err(e) => out.diags.push(e),
                }
            } else if n.callee_path() == Some("mount") && binding.is_none() {
                pending.push((r.offset, r.text, whole));
            } else if n.callee_path() == Some("sim") && binding.is_none() {
                // the project's run settings: read with their own diags on every text change (crate::sys_sim)
            } else {
                out.diags.push(r.error(whole, "the system document takes declarations"));
            }
        }
    }
    let mut mounts = Vec::new();
    for (offset, text, whole) in pending {
        let r = Read { doc, text, offset, units, names: &names };
        // Reserve even a malformed mount's unit, so two statements never choose a winner.
        if let Some(unit) = whole.call("mount").and_then(|args| args.first()).and_then(|n| r.unit(n).ok()) {
            if !mounted.insert(unit.id) {
                repeated.insert(unit.id);
                out.diags.push(r.error(whole, "a unit has a second mount"));
                continue;
            }
        }
        match r.mount(whole, &out.links) {
            Ok(mount) => mounts.push((mount, r.span(whole))),
            Err(e) => out.diags.push(e),
        }
    }
    for (mount, span) in mounts {
        if repeated.contains(&mount.unit) { continue; }
        let unit = mount.unit;
        let i = out.mounts.binary_search_by_key(&unit, |m| m.unit).expect("the mounted unit resolved");
        out.mounts[i] = mount;
        if mount_loop(unit, &out.mounts, &out.links) {
            out.mounts[i] = default_mount(unit);
            out.diags.push(located(doc, span, "the mounts form a loop or mount a unit on itself"));
        }
    }
    out.diags.sort_by_key(|d| d.sel.first().and_then(|s| match s { Sel::Source { start, .. } => Some(*start), _ => None }).unwrap_or(0));
    out
}
