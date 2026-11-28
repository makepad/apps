//! Literal board documents: canonical text, item spans and deferred rule functions.

use crate::map::SourceMap;
use crate::source::located;
use fab_elec_board::{
    Anchor, Area, Board, BoardKeepout, Cutout, Finish, Flash, Graphic, Hole, Islands,
    ItemKind, LabelPose, LayerRole, Origin, Placed, RuleCode, Rules, RuleSet, StackKind, StackLayer, Stackup, Tent, Text,
    Track, TuneSide, TuneStyle, TuneTarget, Tuning, Via, ViaKind, Waiver, Zone, ZoneSettings,
};
use fab_elec_circuit::{Circuit, ViaSize};
use fab_elec_core::{
    metres, Angle, BoardId, Contour, Diag, DocId, FeatureRef, ItemId, LayerId, LayerSet,
    NetId, Nm, PadIx, Path, PolySet, Seg, Sel, Severity, Shape, Side, SolverId, P,
};
use fab_elec_design::Unit;
use fab_elec_parts::{Footprint, Forbid, Gfx, ThermalRule, ZoneConnect};
use makepad_script::syntax::{self, write::Value, Field, Kind, Node};
use makepad_script::units::UnitTable;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;

/// Board units, with lengths in metres, angles in radians and times in seconds.
pub fn units() -> UnitTable {
    UnitTable::standard()
        .with("mil", 254.0, 10_000_000.0)
        .with("inch", 254.0, 10_000.0)
        .with("um", 1.0, 1e6)
        .with("ns", 1.0, 1e9)
        .with("ps", 1.0, 1e12)
}

/// Item identities from the source map, at their current statement starts.
pub struct Ledger {
    pub items: Vec<(u32, ItemId)>,
    pub next_item: u32,
}

impl Ledger {
    pub fn of_map(map: &SourceMap, unit: BoardId, doc: DocId, next_item: u32) -> Ledger {
        let items = map.of_doc(doc).into_iter().filter_map(|(sel, range)| match sel {
            Sel::Item(board, id) if board == unit => Some((range.span.start, id)),
            _ => None,
        }).collect();
        Ledger { items, next_item }
    }
}

/// A board and the source statements that made it.
#[derive(Debug)]
pub struct Lowered {
    /// The literal board items; custom functions are left to the evaluator.
    pub board: Board,
    /// The unit's factory profile, when its board document names one.
    pub factory: Option<String>,
    /// Item, board and outline selections with their whole statement spans.
    pub spans: Vec<(Sel, Range<u32>)>,
    /// Custom rules and checks, in document order.
    pub code: Vec<CodeStmt>,
}

/// A statement whose function the evaluator will lower later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeStmt {
    /// Whether this is a rule or a check.
    pub kind: CodeKind,
    /// The statement's name.
    pub name: String,
    /// The whole statement's byte range.
    pub span: Range<u32>,
}

/// The two statements that carry custom functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeKind {
    /// A conditional design rule.
    Rule,
    /// A board check.
    Check,
}

trait Spelling: Sized {
    fn spelling(&self) -> &'static str;
    fn from_spelling(s: &str) -> Option<Self>;
}
macro_rules! spellings {
    ($ty:ty { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl Spelling for $ty {
            fn spelling(&self) -> &'static str { match self { $(Self::$variant => $s),+ } }
            fn from_spelling(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}
spellings!(Finish { None => "none", Hasl => "hasl", LeadFreeHasl => "lead_free_hasl", Enig => "enig", Osp => "osp", ImmersionSilver => "immersion_silver", ImmersionTin => "immersion_tin" });
spellings!(Side { Top => "top", Bottom => "bottom" });
spellings!(LayerRole { Signal => "signal", Plane => "plane", Mixed => "mixed" });
spellings!(Severity { Info => "info", Warning => "warning", Error => "error" });
spellings!(ViaKind { Through => "through", Blind => "blind", Buried => "buried", Micro => "micro" });
spellings!(Tent { Both => "both", Front => "front", Back => "back", None => "none" });
spellings!(ZoneConnect { Thermal => "thermal", Solid => "solid", ThruThermal => "thru_thermal", Off => "off" });
spellings!(Islands { Remove => "remove", Keep => "keep" });
spellings!(TuneSide { Left => "left", Right => "right", Both => "both" });
spellings!(TuneStyle { Organic => "organic", AnyAngle => "any_angle", FortyFive => "forty_five" });

pub(crate) fn string(s: &str) -> String {
    syntax::write::write(&Value::str(s)).expect("a string has a literal spelling")
}
fn tag(v: &impl Spelling) -> String { format!("@{}", v.spelling()) }
fn decimal(v: i32, suffix: &str) -> String {
    let v = i64::from(v);
    let n = v.abs();
    let whole = n / 1_000_000;
    let fraction = n % 1_000_000;
    let sign = if v < 0 { "-" } else { "" };
    if fraction == 0 { format!("{sign}{whole}{suffix}") }
    else { format!("{sign}{whole}.{}{suffix}", format!("{fraction:06}").trim_end_matches('0')) }
}
pub(crate) fn length(v: Nm) -> String { decimal(v, "mm") }
pub(crate) fn angle(v: Angle) -> String {
    let n = v.udeg();
    decimal(if n > 180_000_000 { n - 360_000_000 } else { n }, "deg")
}
fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(", "))
}
fn object(fields: Vec<(&str, String)>) -> String {
    format!("{{{}}}", fields.into_iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(" "))
}
fn call(name: &str, args: Vec<String>, opts: Vec<(&str, String)>) -> String {
    let mut args = args;
    if !opts.is_empty() { args.push(object(opts)); }
    format!("{name}({})", args.join(", "))
}
fn point(p: P) -> String { array([length(p.x), length(p.y)]) }
/// A placement's own label pose: its centre, then its turn and its pin only when set.
fn label_pose(l: &LabelPose) -> String {
    let mut fields = vec![("at", point(l.at))];
    if l.rot != Angle::ZERO { fields.push(("rot", angle(l.rot))); }
    if l.pinned { fields.push(("pinned", "true".into())); }
    object(fields)
}
fn layer(l: LayerId) -> String { string(&l.name()) }
fn layers(ls: LayerSet) -> String { array(ls.iter().map(layer)) }
fn net(b: &Board, n: NetId) -> String { string(&b.nets[n.ix()].name) }
fn maybe_net(b: &Board, n: Option<NetId>) -> String { n.map_or_else(|| "nil".into(), |n| net(b, n)) }
fn maybe_length(n: Option<Nm>) -> String { n.map_or_else(|| "nil".into(), length) }
/// A detached block item names its block by instance path, never by `BlockId` (ids follow the
/// circuit's evaluation order). A block the circuit no longer has leaves the item authored.
fn provenance(opts: &mut Vec<(&str, String)>, origin: &Origin, c: &Circuit) {
    match origin {
        Origin::Solved { by } => opts.push(("solved", by.0.to_string())),
        Origin::Block { block, detached: true } => {
            if let Some(b) = c.tree.get(block.ix()) { opts.push(("block", string(&b.path))); }
        }
        _ => {}
    }
}
fn item_options(opts: &mut Vec<(&str, String)>, locked: bool, origin: &Origin, c: &Circuit) {
    if locked { opts.push(("locked", "true".into())); }
    provenance(opts, origin, c);
}
/// Endpoints count physical pads with the exact same number, in footprint order.
fn pads_numbered<'a>(footprint: &'a Footprint, no: &'a str) -> impl Iterator<Item = usize> + 'a {
    footprint.pads.iter().enumerate().filter_map(move |(i, pad)| (pad.no == no).then_some(i))
}

fn format_pad_end(placed: &Placed, pad: PadIx) -> Option<String> {
    let no = &placed.footprint.pads.get(pad.ix())?.no;
    let base = string(&format!("{}:{no}", placed.path));
    let mut matches = pads_numbered(&placed.footprint, no);
    let occurrence = matches.position(|i| i == pad.ix())?;
    Some(if occurrence > 0 || matches.next().is_some() { format!("{base}[{occurrence}]") } else { base })
}

fn resolve_pad_end(placed: &Placed, no: &str, ordinal: &str) -> Result<PadIx, String> {
    // A decimal too large for usize is past every occurrence; keep its spelling in the error.
    let wanted = ordinal.parse::<usize>().ok();
    let mut count = 0;
    for i in pads_numbered(&placed.footprint, no) {
        if Some(count) == wanted {
            return u16::try_from(i).map(PadIx).map_err(|_| "footprint pad index is out of range".to_owned());
        }
        count += 1;
    }
    let name = &placed.path;
    if count == 0 { return Err(format!("{name} has no pad {no}")); }
    let noun = if count == 1 { "pad" } else { "pads" };
    Err(format!("{name} has {count} {noun} numbered {no} (index {ordinal} is out of range)"))
}

fn pad_end(b: &Board, p: P, a: Anchor) -> String {
    if let Anchor::Pad { placed, pad } = a {
        if let Some(end) = b.placed_item(placed).and_then(|placed| format_pad_end(placed, pad)) {
            return end;
        }
    }
    point(p)
}
fn path(p: &Path, ends: Option<(&Board, [Anchor; 2])>) -> String {
    let end = |p, i| ends.map_or_else(|| point(p), |(b, a)| pad_end(b, p, a[i]));
    let mut out = vec![end(p.start, 0)];
    for (i, seg) in p.segs.iter().enumerate() {
        let to = if i + 1 == p.segs.len() { end(seg.end(), 1) } else { point(seg.end()) };
        out.push(match seg {
            Seg::Line(_) => to,
            Seg::Arc { mid, .. } => call("arc", vec![point(*mid), to], vec![]),
        });
    }
    array(out)
}
fn poly(p: &PolySet) -> String {
    array(p.polygons().iter().map(|p| {
        let mut fields = vec![("outer", array(p.outer.iter().copied().map(point)))];
        if !p.holes.is_empty() { fields.push(("holes", array(p.holes.iter().map(|h| array(h.iter().copied().map(point)))))); }
        object(fields)
    }))
}
fn shape(s: &Shape) -> String {
    let (name, args) = match s {
        Shape::Circle { c, r } => ("circle", vec![point(*c), length(*r)]),
        Shape::Segment { a, b, w } => ("segment", vec![point(*a), point(*b), length(*w)]),
        Shape::Arc { a, mid, b, w } => ("arc", vec![point(*a), point(*mid), point(*b), length(*w)]),
        Shape::Rect { c, half, rot, corner } => ("rect", vec![point(*c), point(*half), angle(*rot), length(*corner)]),
        Shape::Oval { c, half, rot } => ("oval", vec![point(*c), point(*half), angle(*rot)]),
        Shape::Poly(p) => ("poly", vec![poly(p)]),
    };
    call(name, args, vec![])
}
/// The source selection for an unknown profile in this board document.
pub fn factory_site(doc: DocId, text: &str) -> Option<Sel> {
    let tree = syntax::parse_with(text, &units()).ok()?;
    let node = tree.statements().iter().find(|n| n.callee_path() == Some("factory"))?;
    Some(Sel::Source { doc, start: node.span.start as u32, end: node.span.end as u32 })
}

fn rule_fields(r: &RuleSet, default: &RuleSet) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for (key, value, base) in [
        ("clearance", r.clearance, default.clearance), ("track", r.track, default.track),
        ("drill", r.drill, default.drill), ("annular", r.annular, default.annular),
        ("edge", r.edge, default.edge), ("hole_to_hole", r.hole_to_hole, default.hole_to_hole),
        ("hole_clearance", r.hole_clearance, default.hole_clearance),
        ("silk_clearance", r.silk_clearance, default.silk_clearance),
        ("pair_gap", r.pair_gap, default.pair_gap),
    ] {
        if value != base { out.push((key, maybe_length(value))); }
    }
    if r.via != default.via {
        let at = out.iter().take_while(|(key, _)| matches!(*key, "clearance" | "track")).count();
        out.insert(at, ("via", r.via.map_or_else(|| "nil".into(), |v| object(vec![("drill", length(v.drill)), ("pad", length(v.pad))]))));
    }
    out
}
fn stack(s: &Stackup) -> String {
    let ls = array(s.layers.iter().map(|l| {
        let first = match l.kind {
            StackKind::Copper(id) => ("copper", layer(id)),
            StackKind::Dielectric { core } => ("dielectric", if core { "@core" } else { "@prepreg" }.into()),
            StackKind::Mask(side) => ("mask", tag(&side)),
            StackKind::Silk(side) => ("silk", tag(&side)),
            StackKind::Paste(side) => ("paste", tag(&side)),
        };
        let mut fields = vec![first, ("thickness", length(l.thickness)), ("material", string(&l.material))];
        for (key, value) in [("er", l.er), ("loss_tan", l.loss_tan), ("velocity", l.velocity)] {
            if let Some(v) = value { fields.push((key, v.to_string())); }
        }
        if l.role != LayerRole::Signal { fields.push(("role", tag(&l.role))); }
        object(fields)
    }));
    object(vec![("finish", tag(&s.finish)), ("thickness", length(s.thickness)),
        ("mask", format!("#{:08x}", s.mask_color)), ("silk", format!("#{:08x}", s.silk_color)), ("layers", ls)])
}

/// The kinds of statements before the board items, in canonical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HeaderKind {
    /// The board name.
    Board,
    /// The unit's factory profile.
    Factory,
    /// The layer stack.
    Stackup,
    /// The board edge.
    Outline,
    /// The base design rules.
    Rules,
    /// A waived violation.
    Waive,
    /// A custom rule function.
    Rule,
    /// A custom check function.
    Check,
}

/// Canonical header statements, supplying custom function text when available.
pub(crate) fn header_statements(b: &Board, _c: &Circuit, factory: Option<&str>, code: &dyn Fn(&RuleCode) -> Option<String>) -> Vec<(HeaderKind, String)> {
    let mut out = Vec::new();
    let mut emit = |kind, text| out.push((kind, text));
    emit(HeaderKind::Board, call("board", vec![string(&b.name)], vec![]));
    if let Some(profile) = factory { emit(HeaderKind::Factory, call("factory", vec![string(profile)], vec![])); }
    if b.stackup != Stackup::standard_2_layer() { emit(HeaderKind::Stackup, call("stackup", vec![stack(&b.stackup)], vec![])); }
    if !b.outline.is_empty() { emit(HeaderKind::Outline, call("outline", vec![array(b.outline.iter().map(|c| path(&c.path, None)))], vec![])); }
    let fields = rule_fields(&b.rules.base, &Rules::standard().base);
    if !fields.is_empty() { emit(HeaderKind::Rules, call("rules", vec![object(fields)], vec![])); }
    for w in &b.rules.waivers { emit(HeaderKind::Waive, call("waive", vec![string(&format!("{:016x}", w.violation)), string(&w.reason)], vec![])); }
    for r in &b.rules.custom {
        let mut fields = vec![("when", code(&r.when).unwrap_or_else(|| "nil".into()))];
        if let Some(ls) = r.layers { fields.push(("layers", layers(ls))); }
        fields.push(("set", object(rule_fields(&r.set, &RuleSet::default()))));
        if r.severity != Severity::default() { fields.push(("severity", tag(&r.severity))); }
        emit(HeaderKind::Rule, call("rule", vec![string(&r.name), object(fields)], vec![]));
    }
    for c in &b.rules.checks {
        let opts = if c.severity == Severity::default() { vec![] } else { vec![("severity", tag(&c.severity))] };
        emit(HeaderKind::Check, call("check", vec![string(&c.name), code(&c.code).unwrap_or_else(|| "nil".into())], opts));
    }
    out
}

/// Canonical bindings for tracks referenced by authored tuning statements.
pub(crate) fn track_names(b: &Board) -> HashMap<ItemId, String> {
    b.tracks.iter().enumerate().filter(|(_, t)| {
        b.tunings.iter().any(|u| u.origin.has_statement() && (u.host.contains(&t.id) || u.copper.contains(&t.id)))
    }).map(|(i, t)| (t.id, format!("t{i}"))).collect()
}

/// One canonical item statement, including its binding, or no authored statement.
pub(crate) fn item_statement(b: &Board, c: &Circuit, id: ItemId, names: &HashMap<ItemId, String>) -> Option<String> {
    let (kind, index) = b.locate(id)?;
    let statement = match kind {
        ItemKind::Placed => {
            let p = &b.placed[index];
            if !p.origin.has_statement() { return None; }
            let mut opts = vec![];
            if p.rot != Angle::ZERO { opts.push(("rot", angle(p.rot))); }
            if p.side != Side::Top { opts.push(("side", tag(&p.side))); }
            if let Some((lo, hi)) = p.body_z {
                let body = c.comps.get(p.comp.ix()).and_then(|comp| comp.part.body.as_ref());
                if p.body_z != Placed::band_of(body) { opts.push(("body_z", array([length(lo), length(hi)]))); }
            }
            if let Some(l) = &p.ref_label { opts.push(("ref_label", label_pose(l))); }
            if let Some(l) = &p.value_label { opts.push(("value_label", label_pose(l))); }
            item_options(&mut opts, p.locked, &p.origin, c);
            call("place", vec![string(&p.path), point(p.at)], opts)
        }
        ItemKind::Track => {
            let t = &b.tracks[index];
            if !t.origin.has_statement() { return None; }
            let mut opts = vec![];
            if let Some(ws) = &t.widths { opts.push(("widths", array(ws.iter().copied().map(length)))); }
            item_options(&mut opts, t.locked, &t.origin, c);
            call("track", vec![net(b, t.net), layer(t.layer), length(t.w), path(&t.path, Some((b, t.ends)))], opts)
        }
        ItemKind::Via => {
            let v = &b.vias[index];
            if !v.origin.has_statement() { return None; }
            let mut opts = vec![("drill", length(v.drill)), ("pad", length(v.pad))];
            if v.span != (LayerId::F_CU, LayerId::B_CU) { opts.push(("span", array([layer(v.span.0), layer(v.span.1)]))); }
            if v.kind != ViaKind::Through { opts.push(("kind", tag(&v.kind))); }
            if v.tent != Tent::Both { opts.push(("tent", tag(&v.tent))); }
            if let Some((s, l)) = v.backdrill { opts.push(("backdrill", array([tag(&s), layer(l)]))); }
            item_options(&mut opts, v.locked, &v.origin, c);
            call("via", vec![net(b, v.net), point(v.at)], opts)
        }
        ItemKind::Zone => {
            let z = &b.zones[index];
            if !z.origin.has_statement() { return None; }
            let mut opts = vec![];
            let default = ZoneSettings::default();
            if z.name != z.net.map_or("", |n| b.nets[n.ix()].name.as_str()) { opts.push(("name", string(&z.name))); }
            if z.priority != 0 { opts.push(("priority", z.priority.to_string())); }
            if z.settings.clearance != default.clearance { opts.push(("clearance", maybe_length(z.settings.clearance))); }
            if z.settings.min_width != default.min_width { opts.push(("min_width", length(z.settings.min_width))); }
            if z.settings.connect != default.connect { opts.push(("connect", tag(&z.settings.connect))); }
            if z.settings.thermal != default.thermal {
                let t = &z.settings.thermal;
                opts.push(("thermal", object(vec![("gap", length(t.gap)), ("spoke", length(t.spoke)), ("angle", angle(t.angle))])));
            }
            if z.settings.islands != default.islands { opts.push(("islands", tag(&z.settings.islands))); }
            item_options(&mut opts, z.locked, &z.origin, c);
            let area = match &z.area { Area::Board => "@board".into(), Area::Poly(p) => poly(p) };
            call("zone", vec![maybe_net(b, z.net), layers(z.layers), area], opts)
        }
        ItemKind::Keepout => {
            let k = &b.keepouts[index];
            if !k.origin.has_statement() { return None; }
            let mut opts = vec![];
            if k.forbid != Forbid::ALL {
                opts.push(("forbid", array(forbids().into_iter().filter(|(_, f)| k.forbid.has(*f)).map(|(s, _)| format!("@{s}")))));
            }
            if !k.reason.is_empty() { opts.push(("reason", string(&k.reason))); }
            if let Some((lo, hi)) = k.z { opts.push(("z", array([length(lo), length(hi)]))); }
            provenance(&mut opts, &k.origin, c);
            call("keepout", vec![poly(&k.area), layers(k.layers)], opts)
        }
        ItemKind::Hole => {
            let h = &b.holes[index];
            if !h.origin.has_statement() { return None; }
            let mut opts = vec![];
            if let Some(pad) = h.pad { opts.push(("pad", length(pad))); }
            if let Some(n) = h.net { opts.push(("net", net(b, n))); }
            if h.plated { opts.push(("plated", "true".into())); }
            provenance(&mut opts, &h.origin, c);
            call("hole", vec![string(&h.name), point(h.at), length(h.drill)], opts)
        }
        ItemKind::Cutout => {
            let k = &b.cutouts[index];
            if !k.origin.has_statement() { return None; }
            let mut opts = vec![];
            provenance(&mut opts, &k.origin, c);
            call("cutout", vec![string(&k.name), path(&k.contour.path, None)], opts)
        }
        ItemKind::Graphic => {
            let g = &b.graphics[index];
            if !g.origin.has_statement() { return None; }
            let mut opts = vec![];
            item_options(&mut opts, g.locked, &g.origin, c);
            let gfx = match &g.gfx {
                Gfx::Stroke { path: p, w } => call("stroke", vec![path(p, None), length(*w)], vec![]),
                Gfx::Fill(s) => call("fill", vec![shape(s)], vec![]),
            };
            call("graphic", vec![layer(g.layer), gfx], opts)
        }
        ItemKind::Text => {
            let t = &b.texts[index];
            if !t.origin.has_statement() { return None; }
            let mut opts = vec![("size", length(t.size)), ("thick", length(t.thick))];
            if t.rot != Angle::ZERO { opts.push(("rot", angle(t.rot))); }
            if t.mirrored { opts.push(("mirrored", "true".into())); }
            item_options(&mut opts, t.locked, &t.origin, c);
            call("text", vec![string(&t.text), layer(t.layer), point(t.at)], opts)
        }
        ItemKind::Tuning => {
            let t = &b.tunings[index];
            if !t.origin.has_statement() { return None; }
            let mut opts = vec![];
            if let Some(n) = t.pair { opts.push(("pair", net(b, n))); }
            for (key, ids) in [("host", &t.host), ("copper", &t.copper)] {
                if !ids.is_empty() { opts.push((key, array(ids.iter().map(|i| names[i].clone())))); }
            }
            if t.side != TuneSide::default() { opts.push(("side", tag(&t.side))); }
            opts.push(("amp", array([length(t.amp.0), length(t.amp.1)])));
            opts.push(("pitch", length(t.pitch)));
            if t.style != TuneStyle::default() { opts.push(("style", tag(&t.style))); }
            provenance(&mut opts, &t.origin, c);
            let target = match &t.target {
                TuneTarget::Len(l) => call("len", vec![length(*l)], vec![]),
                TuneTarget::Delay(t) => call("delay", vec![format!("{}ns", t * 1e9)], vec![]),
                TuneTarget::Group(g) => call("group", vec![string(g)], vec![]),
                TuneTarget::Skew { to, tol } => call("skew", vec![net(b, *to), length(*tol)], vec![]),
            };
            call("tuning", vec![net(b, t.net), target], opts)
        }
    };
    Some(if let Some(name) = names.get(&id) { format!("let {name} = {statement}") } else { statement })
}

/// Write a board in canonical order, supplying custom function text when available. `circuit` is
/// the board's circuit: references to its blocks are written as instance paths.
pub fn write_with(board: &Board, circuit: &Circuit, code: &dyn Fn(&RuleCode) -> Option<String>) -> String {
    write_board(board, circuit, None, code)
}

/// Write a unit's board document, including its factory profile. A unit without a board writes
/// no document; custom functions use the same nil placeholders as the importer writer.
pub fn write_unit(unit: &Unit) -> String {
    unit.board.as_deref().map_or_else(String::new, |board| write_board(board, &unit.circuit, unit.factory.as_deref(), &|_| None))
}

fn write_board(board: &Board, circuit: &Circuit, factory: Option<&str>, code: &dyn Fn(&RuleCode) -> Option<String>) -> String {
    let mut out = String::new();
    for (_, statement) in header_statements(board, circuit, factory, code) {
        out.push_str(&statement);
        out.push('\n');
    }
    let names = track_names(board);
    let ids = board.placed.iter().map(|i| i.id)
        .chain(board.tracks.iter().map(|i| i.id))
        .chain(board.vias.iter().map(|i| i.id))
        .chain(board.zones.iter().map(|i| i.id))
        .chain(board.keepouts.iter().map(|i| i.id))
        .chain(board.holes.iter().map(|i| i.id))
        .chain(board.cutouts.iter().map(|i| i.id))
        .chain(board.graphics.iter().map(|i| i.id))
        .chain(board.texts.iter().map(|i| i.id))
        .chain(board.tunings.iter().map(|i| i.id));
    for id in ids {
        if let Some(statement) = item_statement(board, circuit, id, &names) {
            out.push_str(&statement);
            out.push('\n');
        }
    }
    out
}

fn forbids() -> [(&'static str, Forbid); 5] {
    [("tracks", Forbid::TRACKS), ("vias", Forbid::VIAS), ("pads", Forbid::PADS), ("zones", Forbid::ZONES), ("footprints", Forbid::FOOTPRINTS)]
}

fn read_number(n: &Node, text: &str, dimension: &str, label: &str) -> Result<f64, String> {
    let (number, literal) = match &n.kind {
        Kind::Number(v) => (*v, n),
        Kind::Unary { op, operand } if op == "-" => match operand.kind {
            Kind::Number(v) => (-v, operand.as_ref()),
            _ => return Err(format!("{label} must be {dimension}")),
        },
        _ => return Err(format!("{label} must be {dimension}")),
    };
    let table = units();
    let suffix = table.entries().iter().filter(|e| literal.text(text).ends_with(&e.suffix))
        .max_by_key(|e| e.suffix.len()).map(|e| e.suffix.as_str());
    let right = match dimension {
        "a length" => matches!(suffix, Some("m" | "cm" | "mm" | "mil" | "inch" | "um")),
        "an angle" => matches!(suffix, Some("deg" | "°" | "rad")),
        "a time" => matches!(suffix, Some("s" | "ms" | "ns" | "ps")),
        _ => suffix.is_none(),
    };
    if !right || !number.is_finite() { return Err(format!("{label} must be {dimension}")); }
    Ok(number)
}

pub(crate) fn read_length(n: &Node, text: &str, label: &str) -> Result<Nm, String> {
    let v = read_number(n, text, "a length", label)?;
    let nm = (v * 1e9).round();
    if nm < Nm::MIN as f64 || nm > Nm::MAX as f64 {
        return Err(format!("{label} is outside the length range"));
    }
    Ok(metres(v))
}

/// A tool's board point, with the board document's length units and millimetres when bare.
pub fn read_point_mm(text: &str) -> Option<P> {
    let pair = text.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    let (x, y) = pair.split_once(',').or_else(|| pair.split_once(char::is_whitespace))?;
    let table = units();
    let coord = |text: &str| {
        let text = text.trim();
        let text = text.strip_prefix('+').unwrap_or(text);
        let n = syntax::parse_value_with(text, &table).ok()?;
        if n.text(text) != text { return None; }
        if let Ok(length) = read_length(&n, text, "coordinate") {
            return Some(length);
        }
        let mm = read_number(&n, text, "a number", "coordinate").ok()?;
        let nm = (mm * 1e6).round();
        (nm >= Nm::MIN as f64 && nm <= Nm::MAX as f64).then_some(nm as Nm)
    };
    Some(P::new(coord(x)?, coord(y)?))
}

pub(crate) fn read_angle(n: &Node, text: &str, label: &str) -> Result<Angle, String> {
    read_number(n, text, "an angle", label).map(Angle::rad)
}

pub(crate) fn read_uint(n: &Node, text: &str, label: &str, max: u32) -> Result<u32, String> {
    let v = read_number(n, text, "a number", label)?;
    if v < 0.0 || v > max as f64 || v.fract() != 0.0 {
        return Err(format!("{label} must be an integer from 0 to {max}"));
    }
    Ok(v as u32)
}

/// A circuit name read from one whole board string token.
pub(crate) struct NameSite {
    pub span: Range<u32>,
    pub name: Name,
}

pub(crate) enum Name {
    Component(String),
    Block(String),
    Pad { path: String, pad: String },
    Net(String),
}

struct Read<'a> {
    doc: DocId,
    text: &'a str,
    offset: u32,
    prefix: String,
    ids: &'a HashMap<u32, ItemId>,
    errors: &'a RefCell<Vec<Diag>>,
    sites: Option<&'a RefCell<Vec<NameSite>>>,
}

impl Read<'_> {
    fn span(&self, n: &Node) -> Range<u32> {
        self.offset + n.span.start as u32..self.offset + n.span.end as u32
    }
    fn error<T>(&self, n: &Node, message: impl AsRef<str>) -> Option<T> {
        self.error_span(self.span(n), message);
        None
    }
    fn error_span(&self, span: Range<u32>, message: impl AsRef<str>) {
        self.errors.borrow_mut().push(located(self.doc, span, format!("{}: {}", self.prefix, message.as_ref())));
    }
    fn string(&self, n: &Node, label: &str) -> Option<String> {
        match &n.kind {
            Kind::Str(s) => Some(s.clone()),
            _ => self.error(n, format!("{label} must be a string")),
        }
    }
    fn name(&self, n: &Node, label: &str, kind: impl FnOnce(String) -> Name) -> Option<String> {
        let value = self.string(n, label)?;
        if let Some(sites) = self.sites {
            sites.borrow_mut().push(NameSite { span: self.span(n), name: kind(value.clone()) });
        }
        Some(value)
    }
    fn tag<T: Spelling>(&self, n: &Node, label: &str) -> Option<T> {
        n.name().and_then(T::from_spelling).or_else(|| self.error(n, format!("{} is not a valid {label}", n.text(self.text))))
    }
    fn nil(&self, n: &Node) -> bool { n.ident() == Some("nil") }
    fn boolean(&self, n: &Node, label: &str) -> Option<bool> {
        match n.ident() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => self.error(n, format!("{label} must be true or false")),
        }
    }
    fn number(&self, n: &Node, dimension: &str, label: &str) -> Option<f64> {
        read_number(n, self.text, dimension, label).map_err(|e| self.error::<()>(n, e)).ok()
    }
    fn length(&self, n: &Node, label: &str) -> Option<Nm> {
        read_length(n, self.text, label).map_err(|e| self.error::<()>(n, e)).ok()
    }
    fn maybe_length(&self, n: &Node, label: &str) -> Option<Option<Nm>> {
        if self.nil(n) { Some(None) } else { self.length(n, label).map(Some) }
    }
    fn angle(&self, n: &Node, label: &str) -> Option<Angle> {
        read_angle(n, self.text, label).map_err(|e| self.error::<()>(n, e)).ok()
    }
    fn uint(&self, n: &Node, label: &str, max: u32) -> Option<u32> {
        read_uint(n, self.text, label, max).map_err(|e| self.error::<()>(n, e)).ok()
    }
    fn array<'n>(&self, n: &'n Node, label: &str) -> Option<&'n [Node]> {
        match &n.kind {
            Kind::Array { items, .. } => Some(items),
            _ => self.error(n, format!("{label} must be an array")),
        }
    }
    fn pair<'n>(&self, n: &'n Node, label: &str) -> Option<&'n [Node]> {
        let ns = self.array(n, label)?;
        if ns.len() != 2 { return self.error(n, format!("{label} must have two entries")); }
        Some(ns)
    }
    fn band(&self, n: &Node, label: &str) -> Option<(Nm, Nm)> {
        let ns = self.pair(n, label)?;
        Some((self.length(&ns[0], label)?, self.length(&ns[1], label)?))
    }
    fn point(&self, n: &Node) -> Option<P> {
        let ns = self.pair(n, "point")?;
        Some(P::new(self.length(&ns[0], "x")?, self.length(&ns[1], "y")?))
    }
    fn fields<'n>(&self, n: &'n Node, allowed: &[&str]) -> Option<&'n [Field]> {
        let Kind::Object { ty: None, fields, .. } = &n.kind else { return self.error(n, "options must be an object"); };
        let before = self.errors.borrow().len();
        for (i, field) in fields.iter().enumerate() {
            let span = self.offset + field.name_span.start as u32..self.offset + field.name_span.end as u32;
            match field.name.as_deref() {
                Some(key) if allowed.contains(&key) => {
                    if fields[..i].iter().any(|f| f.name.as_deref() == Some(key)) {
                        self.error_span(span.clone(), format!("duplicate option field {key}"));
                    }
                    if field.op != ":" { self.error_span(span, "option fields use :"); }
                }
                Some(key) => self.error_span(span, format!("unknown option field {key}")),
                None => { self.error::<()>(&field.value, "an option needs a field name"); }
            }
        }
        if self.errors.borrow().len() == before { Some(fields) } else { None }
    }
    fn get<'n>(&self, fields: &'n [Field], key: &str) -> Option<&'n Node> {
        fields.iter().find(|f| f.name.as_deref() == Some(key)).map(|f| &f.value)
    }
    fn required<'n>(&self, fields: &'n [Field], key: &str, n: &Node) -> Option<&'n Node> {
        self.get(fields, key).or_else(|| self.error(n, format!("missing option field {key}")))
    }
    fn opt<T>(&self, fields: &[Field], key: &str, default: T, read: impl FnOnce(&Node) -> Option<T>) -> Option<T> {
        match self.get(fields, key) { Some(n) => read(n), None => Some(default) }
    }
    fn args<'n>(&self, n: &'n Node, count: usize, options: Option<&[&str]>) -> Option<(&'n [Node], &'n [Field])> {
        let (_, args) = n.as_call()?;
        if args.len() != count && !(options.is_some() && args.len() == count + 1) {
            return self.error(n, format!("expected {count} positional arguments{}", if options.is_some() { " and an optional options object" } else { "" }));
        }
        let fields = if args.len() > count { self.fields(&args[count], options.unwrap_or(&[]))? } else { &[] };
        Some((&args[..count], fields))
    }
    fn layer(&self, n: &Node) -> Option<LayerId> {
        let s = self.string(n, "layer")?;
        LayerId::parse(&s).or_else(|| self.error(n, format!("{} is not a layer", string(&s))))
    }
    fn layers(&self, n: &Node) -> Option<LayerSet> {
        let mut ls = LayerSet::EMPTY;
        for n in self.array(n, "layers")? { ls = ls.with(self.layer(n)?); }
        Some(ls)
    }
    fn net(&self, n: &Node, circuit: &Circuit) -> Option<NetId> {
        let s = self.name(n, "net", Name::Net)?;
        circuit.net(&s).map(|n| n.id).or_else(|| self.error(n, format!("no net has the name {s}")))
    }
    fn maybe_net(&self, n: &Node, circuit: &Circuit) -> Option<Option<NetId>> {
        if self.nil(n) { Some(None) } else { self.net(n, circuit).map(Some) }
    }
    fn origin(&self, opts: &[Field], circuit: &Circuit) -> Option<Origin> {
        if let Some(n) = self.get(opts, "solved") {
            if self.get(opts, "block").is_some() { return self.error(n, "solved and block cannot both be set"); }
            Some(Origin::Solved { by: SolverId(self.uint(n, "solved", u16::MAX as u32)? as u16) })
        } else if let Some(n) = self.get(opts, "block") {
            let path = self.name(n, "block", Name::Block)?;
            let Some(b) = circuit.tree.iter().find(|b| b.path == path) else {
                return self.error(n, format!("no block has the path {}", string(&path)));
            };
            Some(Origin::Block { block: b.id, detached: true })
        } else { Some(Origin::Authored) }
    }
    fn locked(&self, opts: &[Field]) -> Option<bool> {
        self.opt(opts, "locked", false, |n| self.boolean(n, "locked"))
    }
    fn endpoint(&self, n: &Node, board: Option<&Board>) -> Option<(P, Anchor)> {
        let (b, base, index) = match (board, &n.kind) {
            (Some(b), Kind::Str(_)) => (b, n, None),
            (Some(b), Kind::Index { base, index, .. }) => (b, base.as_ref(), Some(index.as_ref())),
            _ => return Some((self.point(n)?, Anchor::Free)),
        };
        let s = base.string().or_else(|| self.error(base, "pad endpoint must be a string"))?;
        let token = n.text(self.text);
        let Some((name, no)) = s.split_once(':') else { return self.error(n, format!("{token}: expected a placed path and pad number")); };
        if let Some(sites) = self.sites {
            sites.borrow_mut().push(NameSite { span: self.span(base), name: Name::Pad { path: name.into(), pad: no.into() } });
        }
        let ordinal = match index {
            Some(index) => {
                let digits = index.text(self.text);
                if !digits.bytes().all(|c| c.is_ascii_digit()) { return self.error(index, "pad index must be a nonnegative decimal integer"); }
                digits
            }
            None => "0",
        };
        let Some(p) = b.placed.iter().find(|p| p.path == name) else { return self.error(n, format!("{token}: {name} is not placed")); };
        let pad = match resolve_pad_end(p, no, ordinal) {
            Ok(pad) => pad,
            Err(e) => return self.error(n, format!("{token}: {e}")),
        };
        let pos = p.pad_at(pad).or_else(|| self.error(n, "pad has no position"))?;
        Some((pos, Anchor::Pad { placed: p.id, pad }))
    }
    fn path(&self, n: &Node, board: Option<&Board>) -> Option<(Path, [Anchor; 2])> {
        let ns = self.array(n, "path")?;
        let Some(start) = ns.first() else { return self.error(n, "a path needs a start point"); };
        let (start, first) = self.endpoint(start, board)?;
        let mut p = Path::new(start);
        let mut ends = [first, first];
        for (i, n) in ns.iter().enumerate().skip(1) {
            let end_board = if i + 1 == ns.len() { board } else { None };
            let (seg, anchor) = if n.callee_path() == Some("arc") {
                let (a, _) = self.args(n, 2, None)?;
                let mid = self.point(&a[0])?;
                let (end, anchor) = self.endpoint(&a[1], end_board)?;
                (Seg::Arc { mid, end }, anchor)
            } else {
                let (p, anchor) = self.endpoint(n, end_board)?;
                (Seg::Line(p), anchor)
            };
            p.segs.push(seg);
            ends[1] = anchor;
        }
        Some((p, ends))
    }
    fn contour(&self, n: &Node) -> Option<Contour> {
        self.path(n, None).map(|(p, _)| Contour::new(p))
    }
    fn ring(&self, n: &Node) -> Option<Vec<P>> {
        self.array(n, "ring")?.iter().map(|n| self.point(n)).collect()
    }
    fn poly(&self, n: &Node) -> Option<PolySet> {
        let mut set = PolySet::new();
        for n in self.array(n, "polygon set")? {
            let fields = self.fields(n, &["outer", "holes"])?;
            let mut p = PolySet::from_ring_nonzero(self.ring(self.required(fields, "outer", n)?)?);
            if let Some(holes) = self.get(fields, "holes") {
                for hole in self.array(holes, "holes")? {
                    p = p.difference(&PolySet::from_ring_nonzero(self.ring(hole)?));
                }
            }
            set = set.union(&p);
        }
        Some(set)
    }
    fn shape(&self, n: &Node) -> Option<Shape> {
        let Some((name, _)) = n.as_call() else { return self.error(n, "a shape must be a shape call"); };
        let count = match name { "circle" => 2, "segment" | "oval" => 3, "arc" | "rect" => 4, "poly" => 1, _ => return self.error(n, format!("unknown shape {name}")) };
        let (a, _) = self.args(n, count, None)?;
        Some(match name {
            "circle" => Shape::Circle { c: self.point(&a[0])?, r: self.length(&a[1], "radius")? },
            "segment" => Shape::Segment { a: self.point(&a[0])?, b: self.point(&a[1])?, w: self.length(&a[2], "width")? },
            "arc" => Shape::Arc { a: self.point(&a[0])?, mid: self.point(&a[1])?, b: self.point(&a[2])?, w: self.length(&a[3], "width")? },
            "rect" => Shape::Rect { c: self.point(&a[0])?, half: self.point(&a[1])?, rot: self.angle(&a[2], "rotation")?, corner: self.length(&a[3], "corner")? },
            "oval" => Shape::Oval { c: self.point(&a[0])?, half: self.point(&a[1])?, rot: self.angle(&a[2], "rotation")? },
            "poly" => Shape::Poly(self.poly(&a[0])?),
            _ => return None,
        })
    }
    fn gfx(&self, n: &Node) -> Option<Gfx> {
        match n.callee_path() {
            Some("stroke") => {
                let (a, _) = self.args(n, 2, None)?;
                Some(Gfx::Stroke { path: self.path(&a[0], None)?.0, w: self.length(&a[1], "width")? })
            }
            Some("fill") => {
                let (a, _) = self.args(n, 1, None)?;
                Some(Gfx::Fill(self.shape(&a[0])?))
            }
            _ => self.error(n, "a graphic must be stroke(path, width) or fill(shape)"),
        }
    }
    fn colour(&self, n: &Node) -> Option<u32> {
        let Kind::Color(s) = &n.kind else { return self.error(n, "colour must be a colour literal"); };
        let expanded = match s.len() {
            3 | 4 => s.chars().flat_map(|c| [c, c]).collect::<String>(),
            _ => s.clone(),
        };
        let expanded = if expanded.len() == 6 { format!("{expanded}ff") } else { expanded };
        u32::from_str_radix(&expanded, 16).ok().or_else(|| self.error(n, "invalid colour"))
    }
    fn stack(&self, n: &Node) -> Option<Stackup> {
        let f = self.fields(n, &["finish", "thickness", "mask", "silk", "layers"])?;
        let finish = self.tag(self.required(f, "finish", n)?, "finish")?;
        let thickness = self.length(self.required(f, "thickness", n)?, "thickness")?;
        let mask_color = self.colour(self.required(f, "mask", n)?)?;
        let silk_color = self.colour(self.required(f, "silk", n)?)?;
        let mut layers = vec![];
        for n in self.array(self.required(f, "layers", n)?, "layers")? {
            let f = self.fields(n, &["copper", "dielectric", "mask", "silk", "paste", "thickness", "material", "er", "loss_tan", "velocity", "role"])?;
            let kinds: Vec<_> = f.iter().filter(|f| matches!(f.name.as_deref(), Some("copper" | "dielectric" | "mask" | "silk" | "paste"))).collect();
            if kinds.len() != 1 { return self.error(n, "a stack layer needs exactly one kind"); }
            let k = kinds[0];
            let kind = match k.name.as_deref() {
                Some("copper") => StackKind::Copper(self.layer(&k.value)?),
                Some("dielectric") => StackKind::Dielectric { core: match k.value.name() {
                    Some("core") => true, Some("prepreg") => false,
                    _ => return self.error(&k.value, "dielectric must be @core or @prepreg"),
                } },
                Some("mask") => StackKind::Mask(self.tag(&k.value, "side")?),
                Some("silk") => StackKind::Silk(self.tag(&k.value, "side")?),
                Some("paste") => StackKind::Paste(self.tag(&k.value, "side")?),
                _ => return None,
            };
            layers.push(StackLayer {
                kind,
                thickness: self.length(self.required(f, "thickness", n)?, "thickness")?,
                material: self.string(self.required(f, "material", n)?, "material")?,
                er: self.opt(f, "er", None, |n| self.number(n, "a number", "er").map(Some))?,
                loss_tan: self.opt(f, "loss_tan", None, |n| self.number(n, "a number", "loss_tan").map(Some))?,
                velocity: self.opt(f, "velocity", None, |n| self.number(n, "a number", "velocity").map(Some))?,
                role: self.opt(f, "role", LayerRole::Signal, |n| self.tag(n, "role"))?,
            });
        }
        Some(Stackup { layers, finish, thickness, mask_color, silk_color })
    }
    fn rules(&self, n: &Node, mut base: RuleSet) -> Option<RuleSet> {
        let f = self.fields(n, &[
            "clearance", "track", "via", "drill", "annular", "edge", "hole_to_hole", "hole_clearance",
            "silk_clearance", "pair_gap",
        ])?;
        for (key, value) in [("clearance", &mut base.clearance), ("track", &mut base.track),
            ("drill", &mut base.drill), ("annular", &mut base.annular), ("edge", &mut base.edge),
            ("hole_to_hole", &mut base.hole_to_hole), ("hole_clearance", &mut base.hole_clearance),
            ("silk_clearance", &mut base.silk_clearance), ("pair_gap", &mut base.pair_gap)] {
            if let Some(n) = self.get(f, key) { *value = self.maybe_length(n, key)?; }
        }
        if let Some(n) = self.get(f, "via") {
            base.via = if self.nil(n) { None } else {
                let f = self.fields(n, &["drill", "pad"])?;
                Some(ViaSize { drill: self.length(self.required(f, "drill", n)?, "drill")?, pad: self.length(self.required(f, "pad", n)?, "pad")? })
            };
        }
        Some(base)
    }
    /// A label pose `{at: [x, y] rot: a pinned: true}` in the footprint frame; `rot` and `pinned`
    /// may be left out.
    fn label_pose(&self, n: &Node, label: &str) -> Option<LabelPose> {
        if !matches!(n.kind, Kind::Object { ty: None, .. }) { return self.error(n, format!("{label} must be an object")); }
        let f = self.fields(n, &["at", "rot", "pinned"])?;
        Some(LabelPose {
            at: self.point(self.required(f, "at", n)?)?,
            rot: self.opt(f, "rot", Angle::ZERO, |n| self.angle(n, &format!("{label} rot")))?,
            pinned: self.opt(f, "pinned", false, |n| self.boolean(n, &format!("{label} pinned")))?,
        })
    }
    fn thermal(&self, n: &Node, base: ThermalRule) -> Option<ThermalRule> {
        let f = self.fields(n, &["gap", "spoke", "angle"])?;
        Some(ThermalRule {
            gap: self.opt(f, "gap", base.gap, |n| self.length(n, "gap"))?,
            spoke: self.opt(f, "spoke", base.spoke, |n| self.length(n, "spoke"))?,
            angle: self.opt(f, "angle", base.angle, |n| self.angle(n, "angle"))?,
        })
    }
    fn track_refs(&self, n: &Node, names: &HashMap<String, ItemId>) -> Option<Vec<ItemId>> {
        self.array(n, "tracks")?.iter().map(|n| {
            n.ident().and_then(|s| names.get(s)).copied().or_else(|| self.error(n, format!("{} is not a bound track", n.text(self.text))))
        }).collect()
    }
    fn target(&self, n: &Node, c: &Circuit) -> Option<TuneTarget> {
        match n.callee_path() {
            Some("len") => { let (a, _) = self.args(n, 1, None)?; Some(TuneTarget::Len(self.length(&a[0], "length")?)) }
            Some("delay") => { let (a, _) = self.args(n, 1, None)?; Some(TuneTarget::Delay(self.number(&a[0], "a time", "delay")?)) }
            Some("group") => { let (a, _) = self.args(n, 1, None)?; Some(TuneTarget::Group(self.string(&a[0], "group")?)) }
            Some("skew") => {
                let (a, _) = self.args(n, 2, None)?;
                Some(TuneTarget::Skew { to: self.net(&a[0], c)?, tol: self.length(&a[1], "tolerance")? })
            }
            _ => self.error(n, "target must be len, delay, group or skew"),
        }
    }
    fn function(&self, n: &Node) -> Option<()> {
        if matches!(n.kind, Kind::Fn { .. }) || self.nil(n) { Some(()) }
        else { self.error(n, "code must be a function or nil") }
    }
}

impl Read<'_> {
    fn statement(&self, n: &Node, whole: &Node, out: &mut Lowered, c: &Circuit, names: &HashMap<String, ItemId>) -> Option<Option<Sel>> {
        let b = &mut out.board;
        let id = self.ids.get(&self.span(whole).start).copied().unwrap_or(ItemId(b.next_item));
        macro_rules! push {
            ($list:ident, $value:expr) => {{
                let value = $value;
                b.next_item = b.next_item.max(id.0 + 1);
                b.$list.push(value);
                Some(Some(Sel::Item(b.unit, id)))
            }};
        }
        match n.callee_path()? {
            "board" => {
                let (a, _) = self.args(n, 1, None)?;
                b.name = self.string(&a[0], "name")?;
                Some(Some(Sel::Unit(b.unit)))
            }
            "factory" => {
                let (a, _) = self.args(n, 1, None)?;
                let Some(profile) = a[0].string() else { return self.error(whole, "profile must be a string"); };
                out.factory = Some(profile.to_string());
                Some(None)
            }
            "stackup" => {
                let (a, _) = self.args(n, 1, None)?;
                b.stackup = self.stack(&a[0])?;
                Some(None)
            }
            "outline" => {
                let (a, _) = self.args(n, 1, None)?;
                b.outline = self.array(&a[0], "outline")?.iter().map(|n| self.contour(n)).collect::<Option<_>>()?;
                Some(Some(Sel::Feature(FeatureRef::Outline(b.unit))))
            }
            "rules" => {
                let (a, _) = self.args(n, 1, None)?;
                b.rules.base = self.rules(&a[0], Rules::standard().base)?;
                Some(None)
            }
            "waive" => {
                let (a, _) = self.args(n, 2, None)?;
                let s = self.string(&a[0], "violation")?;
                if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return self.error(&a[0], "violation must be 16 hexadecimal digits");
                }
                let violation = u64::from_str_radix(&s, 16).ok().or_else(|| self.error(&a[0], "invalid violation id"))?;
                b.rules.waivers.push(Waiver { violation, reason: self.string(&a[1], "reason")? });
                Some(None)
            }
            "rule" => {
                let (a, _) = self.args(n, 2, None)?;
                let name = self.string(&a[0], "name")?;
                let f = self.fields(&a[1], &["when", "layers", "set", "severity"])?;
                self.function(self.required(f, "when", &a[1])?)?;
                if let Some(n) = self.get(f, "layers") { self.layers(n)?; }
                self.rules(self.required(f, "set", &a[1])?, RuleSet::default())?;
                if let Some(n) = self.get(f, "severity") { self.tag::<Severity>(n, "severity")?; }
                out.code.push(CodeStmt { kind: CodeKind::Rule, name, span: self.span(whole) });
                Some(None)
            }
            "check" => {
                let (a, f) = self.args(n, 2, Some(&["severity"]))?;
                let name = self.string(&a[0], "name")?;
                self.function(&a[1])?;
                if let Some(n) = self.get(f, "severity") { self.tag::<Severity>(n, "severity")?; }
                out.code.push(CodeStmt { kind: CodeKind::Check, name, span: self.span(whole) });
                Some(None)
            }
            "place" => {
                let (a, f) = self.args(n, 2, Some(&["rot", "side", "body_z", "ref_label", "value_label", "locked", "solved", "block"]))?;
                let name = self.name(&a[0], "path", Name::Component)?;
                let comp = c.comp(&name).or_else(|| self.error(&a[0], format!("no component has the path {name}")))?;
                if b.placed_of(comp.id).is_some() { return self.error(&a[0], format!("{name} is already placed")); }
                let footprint = comp.part.footprint.as_ref().cloned().or_else(|| self.error(&a[0], format!("{name} has no footprint")))?;
                let pad_nets = comp.part.pad_pins().into_iter()
                    .map(|pin| pin.and_then(|pin| comp.conns.iter().find(|&&(p, _)| p == pin).map(|&(_, net)| net))).collect();
                // what the part does, from its own `label` field: its legend on the silkscreen
                let legend = comp.fields.get("label").filter(|text| !text.trim().is_empty())
                    .map(|text| fab_elec_board::text::legend_text(text, &footprint));
                push!(placed, Placed {
                    id, comp: comp.id, path: comp.path.clone(), refdes: comp.refdes.clone(), footprint, pad_nets,
                    at: self.point(&a[1])?,
                    rot: self.opt(f, "rot", Angle::ZERO, |n| self.angle(n, "rot"))?,
                    side: self.opt(f, "side", Side::Top, |n| self.tag(n, "side"))?,
                    body_z: self.opt(f, "body_z", Placed::band_of(comp.part.body.as_ref()), |n| self.band(n, "body_z").map(Some))?,
                    ref_label: self.opt(f, "ref_label", None, |n| self.label_pose(n, "ref_label").map(Some))?,
                    value_label: self.opt(f, "value_label", None, |n| self.label_pose(n, "value_label").map(Some))?,
                    legend,
                    value: None,
                    locked: self.locked(f)?, origin: self.origin(f, c)?,
                })
            }
            "track" => {
                let (a, f) = self.args(n, 4, Some(&["widths", "locked", "solved", "block"]))?;
                let net = self.net(&a[0], c)?;
                let layer = self.layer(&a[1])?;
                let w = self.length(&a[2], "width")?;
                let (path, ends) = self.path(&a[3], Some(b))?;
                let widths = if let Some(n) = self.get(f, "widths") {
                    let ws: Vec<Nm> = self.array(n, "widths")?.iter().map(|n| self.length(n, "width")).collect::<Option<_>>()?;
                    if ws.len() != path.segs.len() + 1 { return self.error(n, "widths needs one width per path vertex"); }
                    Some(ws)
                } else { None };
                push!(tracks, Track { id, net, layer, w, widths, path, ends, locked: self.locked(f)?, origin: self.origin(f, c)? })
            }
            "via" => {
                let (a, f) = self.args(n, 2, Some(&["drill", "pad", "span", "kind", "tent", "backdrill", "locked", "solved", "block"]))?;
                let net = self.net(&a[0], c)?;
                let at = self.point(&a[1])?;
                let drill = self.length(self.required(f, "drill", n)?, "drill")?;
                let pad = self.length(self.required(f, "pad", n)?, "pad")?;
                let span = self.opt(f, "span", (LayerId::F_CU, LayerId::B_CU), |n| {
                    let a = self.pair(n, "span")?; Some((self.layer(&a[0])?, self.layer(&a[1])?))
                })?;
                let backdrill = self.opt(f, "backdrill", None, |n| {
                    let a = self.pair(n, "backdrill")?; Some(Some((self.tag(&a[0], "side")?, self.layer(&a[1])?)))
                })?;
                push!(vias, Via {
                    id, net, at, drill, pad, span, backdrill,
                    kind: self.opt(f, "kind", ViaKind::Through, |n| self.tag(n, "via kind"))?,
                    tent: self.opt(f, "tent", Tent::Both, |n| self.tag(n, "tent"))?,
                    locked: self.locked(f)?, origin: self.origin(f, c)?,
                    flash: Flash::All,
                })
            }
            "zone" => {
                let (a, f) = self.args(n, 3, Some(&["name", "priority", "clearance", "min_width", "connect", "thermal", "islands", "locked", "solved", "block"]))?;
                let net = self.maybe_net(&a[0], c)?;
                let layers = self.layers(&a[1])?;
                let area = if a[2].name() == Some("board") { Area::Board } else { Area::Poly(self.poly(&a[2])?) };
                let d = ZoneSettings::default();
                let settings = ZoneSettings {
                    clearance: self.opt(f, "clearance", d.clearance, |n| self.maybe_length(n, "clearance"))?,
                    min_width: self.opt(f, "min_width", d.min_width, |n| self.length(n, "min_width"))?,
                    connect: self.opt(f, "connect", d.connect, |n| self.tag(n, "connection"))?,
                    thermal: match self.get(f, "thermal") { Some(n) => self.thermal(n, d.thermal)?, None => d.thermal },
                    islands: self.opt(f, "islands", d.islands, |n| self.tag(n, "islands"))?,
                };
                let name = self.opt(f, "name", net.map_or_else(String::new, |n| b.nets[n.ix()].name.clone()), |n| self.string(n, "name"))?;
                push!(zones, Zone {
                    id, net, name, layers, area, settings,
                    priority: self.opt(f, "priority", 0, |n| self.uint(n, "priority", u16::MAX as u32).map(|v| v as u16))?,
                    locked: self.locked(f)?, origin: self.origin(f, c)?,
                })
            }
            "keepout" => {
                let (a, f) = self.args(n, 2, Some(&["forbid", "reason", "z", "solved", "block"]))?;
                let forbid = self.opt(f, "forbid", Forbid::ALL, |n| {
                    let mut value = Forbid::default();
                    for n in self.array(n, "forbid")? {
                        let flag = forbids().into_iter().find(|(key, _)| Some(*key) == n.name()).map(|(_, f)| f)
                            .or_else(|| self.error(n, "forbid expects @tracks, @vias, @pads, @zones or @footprints"))?;
                        value = value.with(flag);
                    }
                    Some(value)
                })?;
                push!(keepouts, BoardKeepout {
                    id, area: self.poly(&a[0])?, layers: self.layers(&a[1])?, forbid,
                    z: self.opt(f, "z", None, |n| self.band(n, "z").map(Some))?,
                    reason: self.opt(f, "reason", String::new(), |n| self.string(n, "reason"))?, origin: self.origin(f, c)?,
                })
            }
            "hole" => {
                let (a, f) = self.args(n, 3, Some(&["pad", "net", "plated", "solved", "block"]))?;
                push!(holes, Hole {
                    id, name: self.string(&a[0], "name")?, at: self.point(&a[1])?, drill: self.length(&a[2], "drill")?,
                    pad: self.opt(f, "pad", None, |n| self.maybe_length(n, "pad"))?,
                    net: self.opt(f, "net", None, |n| self.maybe_net(n, c))?,
                    plated: self.opt(f, "plated", false, |n| self.boolean(n, "plated"))?, origin: self.origin(f, c)?,
                })
            }
            "cutout" => {
                let (a, f) = self.args(n, 2, Some(&["solved", "block"]))?;
                push!(cutouts, Cutout { id, name: self.string(&a[0], "name")?, contour: self.contour(&a[1])?, origin: self.origin(f, c)? })
            }
            "graphic" => {
                let (a, f) = self.args(n, 2, Some(&["locked", "solved", "block"]))?;
                push!(graphics, Graphic { id, layer: self.layer(&a[0])?, gfx: self.gfx(&a[1])?, locked: self.locked(f)?, origin: self.origin(f, c)? })
            }
            "text" => {
                let (a, f) = self.args(n, 3, Some(&["size", "thick", "rot", "mirrored", "locked", "solved", "block"]))?;
                push!(texts, Text {
                    id, text: self.string(&a[0], "text")?, layer: self.layer(&a[1])?, at: self.point(&a[2])?,
                    size: self.length(self.required(f, "size", n)?, "size")?,
                    thick: self.length(self.required(f, "thick", n)?, "thick")?,
                    rot: self.opt(f, "rot", Angle::ZERO, |n| self.angle(n, "rot"))?,
                    mirrored: self.opt(f, "mirrored", false, |n| self.boolean(n, "mirrored"))?,
                    locked: self.locked(f)?, origin: self.origin(f, c)?,
                })
            }
            "tuning" => {
                let (a, f) = self.args(n, 2, Some(&["pair", "host", "copper", "side", "amp", "pitch", "style", "solved", "block"]))?;
                let amp = self.pair(self.required(f, "amp", n)?, "amp")?;
                push!(tunings, Tuning {
                    id, net: self.net(&a[0], c)?, target: self.target(&a[1], c)?,
                    pair: self.opt(f, "pair", None, |n| self.maybe_net(n, c))?,
                    host: self.opt(f, "host", vec![], |n| self.track_refs(n, names))?,
                    copper: self.opt(f, "copper", vec![], |n| self.track_refs(n, names))?,
                    side: self.opt(f, "side", TuneSide::default(), |n| self.tag(n, "tuning side"))?,
                    amp: (self.length(&amp[0], "amplitude")?, self.length(&amp[1], "amplitude")?),
                    pitch: self.length(self.required(f, "pitch", n)?, "pitch")?,
                    style: self.opt(f, "style", TuneStyle::default(), |n| self.tag(n, "tuning style"))?, origin: self.origin(f, c)?,
                })
            }
            name => self.error(n, format!("unknown statement {name}")),
        }
    }
}

fn lower_nodes(doc: DocId, text: &str, offset: u32, nodes: &[Node], c: &Circuit,
    out: &mut Lowered, names: &mut HashMap<String, ItemId>, ids: &HashMap<u32, ItemId>, errors: &RefCell<Vec<Diag>>, factory_seen: &mut bool,
    sites: Option<&RefCell<Vec<NameSite>>>) {
    for whole in nodes {
        let (n, binding) = match &whole.kind {
            Kind::Let { keyword, names, value: Some(value), .. } if keyword == "let" && names.len() == 1 => (value.as_ref(), Some(names[0].as_str())),
            _ => (whole, None),
        };
        let prefix = n.as_call().map_or_else(|| "statement".into(), |(name, args)| {
            format!("{name}({})", args.first().map_or("", |n| n.text(text)))
        });
        let r = Read { doc, text, offset, prefix, ids, errors, sites };
        if n.as_call().is_none() { r.error::<()>(n, "expected a board statement call"); continue; }
        if let Some(name) = binding {
            if n.callee_path() != Some("track") { r.error::<()>(whole, "only track statements may be bound"); continue; }
            if names.contains_key(name) { r.error::<()>(whole, format!("duplicate track binding {name}")); continue; }
        }
        // Header order is canonical when writing; lowering accepts it in any order.
        if n.callee_path() == Some("factory") && std::mem::replace(factory_seen, true) {
            r.error::<()>(whole, "duplicate factory statement");
            continue;
        }
        if let Some(Some(sel)) = r.statement(n, whole, out, c, names) {
            if let (Some(name), Sel::Item(_, id)) = (binding, &sel) { names.insert(name.to_string(), *id); }
            out.spans.push((sel, r.span(whole)));
        }
    }
}

fn infer_anchors(b: &mut Board) {
    for i in 0..b.tracks.len() {
        let t = &b.tracks[i];
        let mut ends = t.ends;
        for (end, p) in ends.iter_mut().zip([t.path.start, t.path.end()]) {
            if *end != Anchor::Free { continue; }
            *end = b.infer_anchor(t, p);
        }
        b.tracks[i].ends = ends;
    }
    b.snap_anchors();
}

fn empty(circuit: &Circuit, ledger: Option<&Ledger>) -> Lowered {
    let mut board = Board::new(circuit.unit, &circuit.name);
    board.next_item = ledger.map_or(0, |l| l.next_item);
    board.mirror(circuit);
    Lowered { board, factory: None, spans: vec![], code: vec![] }
}

/// Read name sites through the board reader, parsing once and using its statement positions.
pub(crate) fn name_sites(doc: DocId, text: &str, circuit: &Circuit) -> Vec<NameSite> {
    let Ok(tree) = syntax::parse_with(text, &units()) else { return Vec::new() };
    let sites = RefCell::new(vec![]);
    lower_nodes(doc, text, 0, tree.statements(), circuit, &mut empty(circuit, None),
        &mut HashMap::new(), &HashMap::new(), &RefCell::new(vec![]), &mut false, Some(&sites));
    let mut sites = sites.into_inner();
    sites.sort_by_key(|site| site.span.start);
    sites
}

/// Lower literal board statements, collecting located errors across the document.
pub fn lower(doc: DocId, text: &str, circuit: &Circuit, ledger: Option<&Ledger>) -> Result<Lowered, Vec<Diag>> {
    let mut out = empty(circuit, ledger);
    let ids = ledger.into_iter().flat_map(|l| l.items.iter().copied()).collect();
    let errors = RefCell::new(vec![]);
    let mut names = HashMap::new();
    let mut factory_seen = false;
    let units = units();
    match syntax::parse_with(text, &units) {
        Ok(tree) => lower_nodes(doc, text, 0, tree.statements(), circuit, &mut out, &mut names, &ids, &errors, &mut factory_seen, None),
        Err(_) => {
            // Statements occupy one line, so a broken line need not hide later errors.
            let mut offset = 0;
            for line in text.split_inclusive('\n') {
                match syntax::parse_with(line, &units) {
                    Ok(tree) => lower_nodes(doc, line, offset, tree.statements(), circuit, &mut out, &mut names, &ids, &errors, &mut factory_seen, None),
                    Err(e) => {
                        let start = offset + e.at as u32;
                        let size = line.get(e.at..).and_then(|s| s.chars().next()).map_or(0, |c| c.len_utf8()) as u32;
                        errors.borrow_mut().push(located(doc, start..start + size, format!("syntax: {}", e.message)));
                    }
                }
                offset += line.len() as u32;
            }
        }
    }
    let mut errors = errors.into_inner();
    if !errors.is_empty() {
        errors.sort_by_key(|e| e.sel.first().and_then(|s| match s { Sel::Source { start, .. } => Some(*start), _ => None }).unwrap_or(0));
        return Err(errors);
    }
    // The item index is derived: built once here, O(items), so `locate` is O(1) on a lowered
    // board instead of a scan (and the anchor pass below already uses it).
    out.board.reindex();
    infer_anchors(&mut out.board);
    fab_elec_board::text::place_legends(&mut out.board);
    Ok(out)
}
