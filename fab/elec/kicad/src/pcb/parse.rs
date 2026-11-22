//! Streaming board parser. Unknown and unsupported lists take the depth-counting lexer path.

use super::file::*;
use crate::report::{GapKind, ImportReport, Refusal};
use crate::sexpr::{mm_to_nm, Kind, Lexer, Token, NEWEST_BOARD_VERSION};
use fab_elec_board::{Finish, Flash, Islands, LayerRole, StackKind, StackLayer, Tent, ViaKind, ZoneSettings, ARC_ERR};
use fab_elec_core::geom::ArcGeom;
use fab_elec_core::{Angle, LayerId, LayerKind, LayerSet, LayerSide, Nm, Path, PolySet, Seg, Shape, Side, P};
use fab_elec_parts::{Drill, Footprint, Forbid, FpAttrs, FpMount, FpText, FpTextKind, Gfx, Keepout,
    Pad, PadKind, PadProp, PadShape, PadStack, PasteRule, PinKind, ThermalRule, ZoneConnect};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Copy, Clone)]
struct Head<'a> { token: Token, key: &'a [u8], list: bool }
impl Head<'_> { fn loc(self) -> Location { self.token.location() } }

struct Parser<'a> {
    lex: Lexer<'a>,
    path: Vec<&'a [u8]>,
    report: ImportReport,
    layers: LayerTable,
    have_layers: bool,
    field_names: HashMap<Arc<str>, Arc<str>>,
}

/// Parse a complete board without resolving nets, merging routes or constructing a `Board`.
pub fn parse_kicad_pcb(bytes: &[u8]) -> Result<PcbFile, Refusal> {
    let mut report = ImportReport::default();
    report.version = 20201115;
    let mut parser = Parser { lex: Lexer::new(bytes), path: Vec::new(), report,
        layers: LayerTable::default(), have_layers: false, field_names: HashMap::new() };
    match parser.board() {
        Ok(file) => Ok(file),
        Err(mut error) => {
            if parser.report.too_recent {
                error.message.push_str(&format!("; file format {} is newer than reader {}",
                    parser.report.version, NEWEST_BOARD_VERSION));
                if let Some(v) = &parser.report.generator_version {
                    error.message.push_str(&format!(" (generator version {v})"));
                }
            }
            Err(error)
        }
    }
}

impl<'a> Parser<'a> {
    fn take(&mut self) -> Result<Token, Refusal> { self.lex.next_token() }
    fn peek(&mut self) -> Result<Token, Refusal> { self.lex.peek() }
    fn error(&self, t: Token, message: impl Into<String>) -> Refusal { Refusal::new(t.location(), message) }
    fn require_list(&self, h: Head<'a>) -> Result<(), Refusal> {
        if h.list { Ok(()) } else { Err(self.error(h.token, "need a list value")) }
    }
    fn child(&mut self) -> Result<Option<Head<'a>>, Refusal> {
        let t = self.take()?;
        match t.kind {
            Kind::Close => { self.path.pop(); Ok(None) }
            Kind::Eof => Err(self.error(t, "end of input inside a list")),
            Kind::Open => {
                let t = self.take()?;
                if !matches!(t.kind, Kind::Symbol | Kind::Number | Kind::String | Kind::Bar) {
                    return Err(self.error(t, "need a list keyword"));
                }
                self.path.push(self.lex.raw(t));
                Ok(Some(Head { token: t, key: if t.kind == Kind::String { b"" } else { self.lex.raw(t) }, list: true }))
            }
            _ => {
                self.path.push(self.lex.raw(t));
                Ok(Some(Head { token: t, key: if t.kind == Kind::String { b"" } else { self.lex.raw(t) }, list: false }))
            }
        }
    }
    fn skip(&mut self, h: Head<'a>) -> Result<(), Refusal> {
        if h.list {
            let old = self.lex.bar_aware();
            if h.key == b"embedded_files" { self.lex.set_bar_aware(true); }
            self.lex.skip_list()?;
            self.lex.set_bar_aware(old);
        }
        self.path.pop();
        Ok(())
    }
    fn unknown(&mut self, h: Head<'a>) -> Result<(), Refusal> {
        let path = self.path.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(">");
        self.report.unknown(path, h.loc());
        self.skip(h)
    }
    fn rest(&mut self) -> Result<(), Refusal> {
        while let Some(h) = self.child()? { self.unknown(h)?; }
        Ok(())
    }
    fn gap(&mut self, feature: &'static str, at: Location) { self.report.gap(feature, GapKind::Skipped, at); }
    fn approx(&mut self, feature: &'static str, at: Location) { self.report.gap(feature, GapKind::Approximated, at); }
    fn dropped(&mut self, feature: &'static str, at: Location) { self.report.gap(feature, GapKind::Dropped, at); }
    fn skipped(&mut self, h: Head<'a>, feature: &'static str) -> Result<(), Refusal> {
        self.gap(feature, h.loc()); self.skip(h)
    }
    fn name_token(&mut self, what: &str) -> Result<Token, Refusal> {
        let t = self.take()?;
        if matches!(t.kind, Kind::Symbol | Kind::Number | Kind::String) { Ok(t) }
        else { Err(self.error(t, format!("need a name for '{what}'"))) }
    }
    fn name(&mut self, what: &str) -> Result<String, Refusal> {
        let t = self.name_token(what)?; self.lex.text(t)
    }
    fn enum_value(&mut self, what: &str) -> Result<&'a [u8], Refusal> {
        let t = self.take()?;
        if t.kind != Kind::Symbol { return Err(self.error(t, format!("need an unquoted value for '{what}'"))); }
        Ok(self.lex.raw(t))
    }
    fn number(&mut self, what: &str) -> Result<f64, Refusal> {
        let t = self.take()?; self.lex.real(t, what)
    }
    fn integer(&mut self, what: &str) -> Result<i64, Refusal> {
        let t = self.take()?; self.lex.integer(t, what)
    }
    fn length(&mut self, what: &str) -> Result<Nm, Refusal> { Ok(mm_to_nm(self.number(what)?)) }
    fn scalar(&mut self, h: Head<'a>) -> Result<f64, Refusal> {
        self.require_list(h)?;
        let n = self.number(std::str::from_utf8(h.key).unwrap_or("value"))?;
        self.rest()?; Ok(n)
    }
    fn nm(&mut self, h: Head<'a>) -> Result<Nm, Refusal> { Ok(mm_to_nm(self.scalar(h)?)) }
    fn int(&mut self, h: Head<'a>) -> Result<i64, Refusal> {
        self.require_list(h)?;
        let n = self.integer(std::str::from_utf8(h.key).unwrap_or("value"))?;
        self.rest()?; Ok(n)
    }
    fn string(&mut self, h: Head<'a>) -> Result<String, Refusal> {
        self.require_list(h)?; let s = self.name("value")?; self.rest()?; Ok(s)
    }
    fn enumeration(&mut self, h: Head<'a>) -> Result<&'a [u8], Refusal> {
        self.require_list(h)?; let s = self.enum_value("value")?; self.rest()?; Ok(s)
    }
    fn flag(&mut self, h: Head<'a>) -> Result<bool, Refusal> {
        if !h.list { self.path.pop(); return Ok(true); }
        if self.peek()?.kind == Kind::Close { self.take()?; self.path.pop(); return Ok(true); }
        let t = self.take()?;
        let value = match (t.kind, self.lex.raw(t)) {
            (Kind::Symbol, b"yes" | b"true") => true,
            (Kind::Symbol, b"no" | b"false") => false,
            _ => return Err(self.error(t, "need yes or no")),
        };
        self.rest()?; Ok(value)
    }
    /// `(model "<path>" (offset (xyz ..)) (scale (xyz ..)) (rotate (xyz ..)) (hide) (opacity ..))`,
    /// its path taken. `(at (xyz ..))`, before KiCad 5, is inches. Opacity is reported, not kept.
    fn model(&mut self, h: Head<'a>) -> Result<Model3d, Refusal> {
        let path = self.name("model path")?;
        let mut model = Model3d { at: h.loc(), path, offset: [0.0; 3], scale: [1.0; 3], rotate: [0.0; 3], hidden: false };
        while let Some(c) = self.child()? {
            match c.key {
                b"offset" | b"at" | b"scale" | b"rotate" => {
                    self.require_list(c)?;
                    let mut xyz = None;
                    while let Some(d) = self.child()? {
                        if d.key == b"xyz" && d.list {
                            let v = [self.number("x")?, self.number("y")?, self.number("z")?];
                            self.rest()?;
                            xyz = Some(v);
                        } else { self.unknown(d)?; }
                    }
                    let Some(v) = xyz else { return Err(self.error(c.token, "need (xyz x y z)")) };
                    match c.key {
                        b"offset" => model.offset = v,
                        b"at" => model.offset = v.map(|inches| inches * 25.4),
                        b"scale" => model.scale = v,
                        _ => model.rotate = v,
                    }
                }
                b"hide" => model.hidden = self.flag(c)?,
                b"opacity" => self.skipped(c, "model.opacity")?,
                _ => self.unknown(c)?,
            }
        }
        Ok(model)
    }
    fn pair(&mut self, h: Head<'a>, flip_y: bool) -> Result<P, Refusal> {
        self.require_list(h)?;
        let p = self.point(flip_y)?; self.rest()?; Ok(p)
    }
    fn point(&mut self, flip_y: bool) -> Result<P, Refusal> {
        let x = self.length("x")?; let y = self.length("y")?;
        Ok(P::new(x, if flip_y { -y } else { y }))
    }
    fn position(&mut self, h: Head<'a>) -> Result<(P, Angle, bool), Refusal> {
        self.require_list(h)?;
        let p = self.point(true)?;
        let a = if self.peek()?.kind == Kind::Number { angle(self.number("angle")?) } else { Angle::ZERO };
        let mut unlocked = false;
        while let Some(c) = self.child()? {
            if c.key == b"unlocked" { unlocked |= self.flag(c)?; } else { self.unknown(c)?; }
        }
        Ok((p, a, unlocked))
    }
    fn required<T>(&self, value: Option<T>, h: Head<'a>, what: &str) -> Result<T, Refusal> {
        value.ok_or_else(|| self.error(h.token, format!("missing required '{what}'")))
    }
    fn board(&mut self) -> Result<PcbFile, Refusal> {
        let start = self.take()?;
        if start.kind != Kind::Open { return Err(self.error(start, "not a board file")); }
        let root = self.take()?;
        if root.kind != Kind::Symbol || self.lex.raw(root) != b"kicad_pcb" {
            return Err(self.error(root, "not a board file"));
        }
        self.path.push(b"kicad_pcb");
        let mut file = PcbFile { location: root.location(), version: VersionInfo { version: 20201115,
            generator: None, generator_version: None, too_recent: false }, layers: LayerTable::default(),
            thickness: None, setup: Setup::default(), stackup: None, nets: Vec::new(), classes: Vec::new(),
            footprints: Vec::new(), tracks: Vec::new(), vias: Vec::new(), zones: Vec::new(), keepouts: Vec::new(),
            graphics: Vec::new(), edges: Vec::new(), texts: Vec::new(), report: ImportReport::default() };
        let mut first = true;
        let mut codes = HashSet::new();
        let mut classes = HashSet::new();
        let mut tents = Vec::new();
        while let Some(h) = self.child()? {
            if !h.list { self.unknown(h)?; first = false; continue; }
            match h.key {
                b"version" => {
                    if !first { return Err(self.error(h.token, "version must precede other board content")); }
                    self.report.version = self.int(h)?;
                    self.report.too_recent = self.report.version > NEWEST_BOARD_VERSION;
                    self.lex.set_bar_aware(self.report.version >= 20240706);
                }
                b"generator" => self.report.generator = Some(self.string(h)?),
                b"generator_version" => self.report.generator_version = Some(self.string(h)?),
                b"host" => {
                    self.report.generator = Some(self.name("host")?);
                    if self.report.version < 20200825 { self.report.generator_version = Some(self.name("host version")?); }
                    self.rest()?;
                }
                b"general" => {
                    while let Some(c) = self.child()? {
                        match c.key {
                            b"thickness" => file.thickness = Some(Located { at: c.loc(), value: self.nm(c)? }),
                            b"legacy_teardrops" => self.skipped(c, "teardrops")?,
                            _ => self.skip(c)?,
                        }
                    }
                }
                b"layers" => self.layer_table(h)?,
                b"setup" => self.setup(h, &mut file.setup, &mut file.stackup)?,
                b"net" => {
                    let code = self.integer("net code")?;
                    let name = self.name("net name")?;
                    self.rest()?; self.report.count_read("nets");
                    if codes.insert(code) { file.nets.push(FileNet { location: h.loc(), code, name }); }
                    else { self.gap("net.duplicate_code", h.loc()); }
                }
                b"net_class" => {
                    let class = self.net_class(h)?;
                    if !classes.insert(class.name.clone()) {
                        return Err(self.error(h.token, format!("duplicate net class {}", class.name)));
                    }
                    self.report.count_read("classes"); file.classes.push(class);
                }
                b"footprint" | b"module" => { if let Some(fp) = self.footprint(h)? { file.footprints.push(fp); } }
                b"segment" | b"arc" => { if let Some(t) = self.track(h)? { file.tracks.push(t); } }
                b"via" => {
                    let (via, tent) = self.via(h)?;
                    file.vias.push(via); tents.push(tent);
                }
                b"zone" => match self.zone(h, false)? {
                    ZoneRead::Zone(z) => file.zones.push(z), ZoneRead::Keepout(k) => file.keepouts.push(k),
                    ZoneRead::Skipped => {}
                },
                b"gr_text" => {
                    let content = self.name("text")?;
                    let t = self.text(h, content)?;
                    if let Some(text) = t.value { file.texts.push(text); }
                }
                key if key.starts_with(b"gr_") && graphic_kind(key).is_some() => {
                    let shape = self.graphic(h, false)?;
                    for l in shape.layers.iter() {
                        if l == LayerId::EDGE { file.edges.push(shape.edge(h.loc())); }
                        else {
                            if l.is_copper() { self.gap("graphic.on_copper", h.loc()); }
                            for gfx in shape.graphics(l) {
                                file.graphics.push(FileGraphic { location: h.loc(), layer: l, gfx, locked: shape.locked });
                            }
                        }
                    }
                }
                b"generated" => self.generated(h)?,
                b"embedded_fonts" => self.skip(h)?,
                _ => {
                    if let Some(feature) = top_skip(h.key) { self.skipped(h, feature)?; }
                    else { self.unknown(h)?; }
                }
            }
            first = false;
        }
        if let Some(at) = self.lex.trailing_content() { self.dropped("file.trailing_content", at); }
        let (lossy, first_lossy) = self.lex.lossy_texts();
        if lossy > 0 {
            self.approx("text.invalid_utf8", first_lossy);
            if let Some(g) = self.report.gaps.get_mut("text.invalid_utf8") { g.count = lossy; }
        }
        if file.classes.is_empty() { self.gap("rules.project_file", root.location()); }
        for (via, tent) in file.vias.iter_mut().zip(tents) {
            via.tent = tent.resolve(file.setup.tenting.unwrap_or(Tent::Both));
        }
        for zone in &mut file.zones {
            if zone.settings.clearance.is_none() { zone.settings.clearance = file.setup.zone_clearance; }
        }
        file.version = VersionInfo { version: self.report.version, generator: self.report.generator.clone(),
            generator_version: self.report.generator_version.clone(), too_recent: self.report.too_recent };
        file.layers = std::mem::take(&mut self.layers);
        file.report = std::mem::take(&mut self.report);
        Ok(file)
    }

    fn layer_table(&mut self, h: Head<'a>) -> Result<(), Refusal> {
        let mut table = LayerTable { location: h.loc(), ..LayerTable::default() };
        for i in 0..=255u8 {
            let id = LayerId(i);
            if i <= 145 || i >= 160 { table.names.insert(id.name(), id); }
        }
        let mut copper_run = true;
        while let Some(c) = self.child()? {
            if c.token.kind != Kind::Number || !c.list { self.unknown(c)?; continue; }
            self.lex.integer(c.token, "layer number")?;
            let token = self.name_token("layer name")?;
            let name = self.lex.text(token)?;
            let typ = self.enum_value("layer type")?;
            let copper = copper_run && matches!(typ, b"signal" | b"power" | b"mixed" | b"jumper");
            copper_run &= copper;
            let mut user_name = None;
            if self.peek()?.kind == Kind::String && self.report.version >= 20200922 {
                user_name = Some(self.name("layer user name")?);
            }
            while let Some(extra) = self.child()? {
                if extra.key == b"hide" { self.skip(extra)?; } else { self.unknown(extra)?; }
            }
            let role = match typ { b"power" => LayerRole::Plane, b"mixed" => LayerRole::Mixed, _ => LayerRole::Signal };
            if typ == b"jumper" { self.gap("layer.jumper", c.loc()); }
            let id = if copper { table.copper_count += 1; None }
            else {
                let canonical = if self.report.version < 20200922 { legacy_layer(&name) } else { &name };
                if let Some(id) = LayerId::parse(canonical) { Some(id) }
                else if beyond_layer(&name) { table.beyond.insert(name.clone()); None }
                else { return Err(self.error(token, format!("layer {name} is not a known board layer"))); }
            };
            table.entries.push(LayerEntry { location: c.loc(), name, user_name, id, copper, role });
        }
        if table.copper_count < 2 || table.copper_count % 2 != 0 {
            return Err(self.error(h.token, format!("{} is not a valid copper layer count", table.copper_count)));
        }
        for (i, row) in table.entries.iter_mut().enumerate() {
            if row.copper {
                row.id = if i == 0 { Some(LayerId::F_CU) }
                else if i + 1 == table.copper_count { Some(LayerId::B_CU) }
                else if let Some(id) = LayerId::parse(&row.name).filter(|id| id.inner_index().is_some()) {
                    if id.0 as usize != i { self.approx("layers.inner_order", row.location); }
                    Some(id)
                } else if i <= 126 { Some(LayerId(i as u8)) } else { None };
                self.report.count_read("copper_layers");
                if let Some(id) = row.id { table.copper.push(id); }
                else { table.beyond.insert(row.name.clone()); }
            }
            if let Some(id) = row.id {
                table.names.insert(row.name.clone(), id);
                table.roles.entry(id).or_insert(row.role);
            }
        }
        self.layers = table; self.have_layers = true; Ok(())
    }
    fn layer_name(&mut self, t: Token) -> Result<Option<LayerId>, Refusal> {
        let name = self.lex.text(t)?;
        if !self.have_layers { return Err(self.error(t, "layer name before layers table")); }
        if let Some(&l) = self.layers.names.get(&name) { return Ok(Some(l)); }
        if beyond_layer(&name) || self.layers.beyond.contains(&name) {
            self.report.gap_value("layer.beyond_model", GapKind::Skipped, t.location(), name);
            return Ok(None);
        }
        if let Some(n) = name.strip_prefix("Inner").and_then(|s| s.strip_suffix(".Cu"))
            .and_then(|s| s.parse::<u8>().ok()).filter(|n| (1..=14).contains(n)) {
            self.approx("layers.legacy_inner_name", t.location()); return Ok(Some(LayerId(15 - n)));
        }
        Err(self.error(t, format!("item on undefined layer {name}")))
    }
    fn layer_mask(&mut self, h: Head<'a>) -> Result<LayerSet, Refusal> {
        Ok(self.layer_mask_details(h)?.0)
    }
    fn layer_mask_details(&mut self, h: Head<'a>) -> Result<(LayerSet, bool), Refusal> {
        self.require_list(h)?;
        let mut set = LayerSet::EMPTY;
        let mut all_copper = false;
        while !matches!(self.peek()?.kind, Kind::Close | Kind::Open | Kind::Eof) {
            let t = self.name_token("layer")?;
            let name = self.lex.text(t)?;
            if !self.have_layers { return Err(self.error(t, "layer name before layers table")); }
            all_copper |= name == "*.Cu";
            let wildcard = match name.as_str() {
                "*.Cu" => Some(self.layers.copper_set()),
                "*In.Cu" => Some(self.layers.copper_set().without(LayerId::F_CU).without(LayerId::B_CU)),
                "F&B.Cu" => Some(LayerSet::of(&[LayerId::F_CU, LayerId::B_CU])),
                "*.Adhes" => Some(LayerSet::of(&[LayerId::F_ADHES, LayerId::B_ADHES])),
                "*.Paste" => Some(LayerSet::of(&[LayerId::F_PASTE, LayerId::B_PASTE])),
                "*.Mask" => Some(LayerSet::of(&[LayerId::F_MASK, LayerId::B_MASK])),
                "*.SilkS" => Some(LayerSet::of(&[LayerId::F_SILK, LayerId::B_SILK])),
                "*.Fab" => Some(LayerSet::of(&[LayerId::F_FAB, LayerId::B_FAB])),
                "*.CrtYd" => Some(LayerSet::of(&[LayerId::F_CRTYD, LayerId::B_CRTYD])),
                _ => None,
            };
            if let Some(s) = wildcard { set = set.union(s); }
            else if let Some(l) = self.layer_name(t)? { set = set.with(l); }
        }
        self.rest()?; Ok((set, all_copper))
    }
    fn single_layer(&mut self, h: Head<'a>) -> Result<Option<LayerId>, Refusal> {
        self.require_list(h)?; let t = self.name_token("layer")?;
        let l = self.layer_name(t)?; self.rest()?; Ok(l)
    }
    fn net_ref(&mut self, h: Head<'a>) -> Result<NetRef, Refusal> {
        self.require_list(h)?;
        let t = self.name_token("net")?;
        let second = matches!(self.peek()?.kind, Kind::Symbol | Kind::String | Kind::Number);
        let mut net = NetRef { location: Some(h.loc()), ..NetRef::default() };
        if second || (self.report.version < 20251028 && t.kind == Kind::Number) {
            net.code = Some(self.lex.integer(t, "net code")?);
            if second { net.name = Some(self.name("net name")?); }
        } else { net.name = Some(self.lex.text(t)?); }
        self.rest()?; Ok(net)
    }
    fn net_class(&mut self, h: Head<'a>) -> Result<FileNetClass, Refusal> {
        let mut class = FileNetClass { location: h.loc(), name: self.name("class name")?,
            description: self.name("class description")?, clearance: None, trace_width: None, via_dia: None,
            via_drill: None, diff_pair_width: None, diff_pair_gap: None, add_net: Vec::new() };
        while let Some(c) = self.child()? {
            match c.key {
                b"clearance" => class.clearance = Some(self.nm(c)?),
                b"trace_width" => class.trace_width = Some(self.nm(c)?),
                b"via_dia" => class.via_dia = Some(self.nm(c)?),
                b"via_drill" => class.via_drill = Some(self.nm(c)?),
                b"diff_pair_width" => class.diff_pair_width = Some(self.nm(c)?),
                b"diff_pair_gap" => class.diff_pair_gap = Some(self.nm(c)?),
                b"add_net" => class.add_net.push(Located { at: c.loc(), value: self.string(c)? }),
                b"uvia_dia" | b"uvia_drill" => self.skipped(c, "class.microvia")?,
                _ => self.unknown(c)?,
            }
        }
        Ok(class)
    }
    fn setup(&mut self, h: Head<'a>, s: &mut Setup, stack: &mut Option<FileStackup>) -> Result<(), Refusal> {
        s.location = Some(h.loc());
        while let Some(c) = self.child()? {
            s.legacy_rules |= matches!(c.key,
                b"last_trace_width" | b"user_trace_width" | b"trace_clearance" | b"zone_clearance"
                | b"zone_45_only" | b"clearance_min" | b"trace_min" | b"via_size" | b"via_drill"
                | b"via_min_annulus" | b"via_min_size" | b"through_hole_min" | b"via_min_drill"
                | b"hole_to_hole_min" | b"user_via" | b"uvia_size" | b"uvia_drill" | b"uvias_allowed"
                | b"blind_buried_vias_allowed" | b"uvia_min_size" | b"uvia_min_drill" | b"user_diff_pair"
                | b"segment_width" | b"edge_width" | b"mod_edge_width" | b"pcb_text_width"
                | b"mod_text_width" | b"pcb_text_size" | b"mod_text_size" | b"defaults"
                | b"pad_size" | b"pad_drill" | b"max_error");
            match c.key {
                b"stackup" => *stack = Some(self.stackup(c)?),
                b"pad_to_mask_clearance" => s.mask_margin = Some(self.nm(c)?),
                b"pad_to_paste_clearance" => s.paste_margin = Some(self.nm(c)?),
                b"pad_to_paste_clearance_ratio" => s.paste_ratio_ppm = Some(ppm(self.scalar(c)?)),
                b"trace_min" => s.track = Some(self.nm(c)?),
                b"clearance_min" => s.clearance = Some(self.nm(c)?),
                b"via_min_annulus" => s.annular = Some(self.nm(c)?),
                b"via_min_drill" | b"through_hole_min" => s.drill = Some(self.nm(c)?),
                b"hole_to_hole_min" => s.hole_to_hole = Some(self.nm(c)?),
                b"trace_clearance" => s.trace_clearance = Some(self.nm(c)?),
                b"via_size" => s.via_size = Some(self.nm(c)?),
                b"via_drill" => s.via_drill = Some(self.nm(c)?),
                b"zone_clearance" => s.zone_clearance = Some(self.nm(c)?),
                b"tenting" => s.tenting = Some(self.tenting(c)?.resolve(Tent::Both)),
                b"solder_mask_min_width" => self.skipped(c, "rules.mask_min_width")?,
                b"allow_soldermask_bridges_in_footprints" => self.skipped(c, "rules.mask_bridges")?,
                b"covering" | b"plugging" | b"capping" | b"filling" => self.skipped(c, "via.protection")?,
                b"via_min_size" => self.skipped(c, "rules.via_min_size")?,
                b"uvia_size" | b"uvia_drill" | b"uvias_allowed" | b"uvia_min_size" | b"uvia_min_drill"
                    | b"blind_buried_vias_allowed" => self.skipped(c, "rules.microvia")?,
                b"user_trace_width" | b"user_via" | b"user_diff_pair" | b"last_trace_width" => self.skipped(c, "rules.size_presets")?,
                b"aux_axis_origin" => self.skipped(c, "fab.aux_origin")?,
                b"pcbplotparams" => self.skipped(c, "fab.plot_params")?,
                b"defaults" if c.list => {
                    while let Some(value) = self.child()? {
                        if value.key == b"edge_clearance" { s.edge_clearance = Some(self.nm(value)?); }
                        else { self.skip(value)?; }
                    }
                }
                b"defaults" | b"max_error" | b"grid_origin" | b"visible_elements" | b"zone_45_only" | b"segment_width"
                    | b"edge_width" | b"mod_edge_width" | b"pcb_text_width" | b"mod_text_width" | b"pcb_text_size"
                    | b"mod_text_size" | b"pad_size" | b"pad_drill" | b"filled_areas_thickness"
                    | b"drill_symbol_profile" | b"zone_defaults" => self.skip(c)?,
                _ => self.unknown(c)?,
            }
        }
        Ok(())
    }
    fn generated(&mut self, h: Head<'a>) -> Result<(), Refusal> {
        let mut tuning = false;
        while let Some(c) = self.child()? {
            if c.key == b"type" && c.list {
                tuning |= self.name("generated type")? == "tuning_pattern";
            }
            self.skip(c)?;
        }
        self.gap(if tuning { "tuning_pattern" } else { "generated" }, h.loc()); Ok(())
    }
}

fn angle(n: f64) -> Angle { Angle::deg(if n.is_finite() { n.rem_euclid(360.0) } else { 0.0 }) }
fn ppm(n: f64) -> i32 { (n * 1_000_000.0).round() as i32 }
fn beyond_layer(name: &str) -> bool {
    name == "Rescue" || name.strip_prefix("User.").is_some_and(|n| {
        !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) && n.parse::<u64>().map_or(true, |n| n > 96)
    })
}
fn top_skip(key: &[u8]) -> Option<&'static str> {
    Some(match key {
        b"paper" | b"page" | b"title_block" => "sheet", b"property" => "board.properties",
        b"dimension" => "dimension", b"group" => "group", b"gr_text_box" => "textbox",
        b"table" => "table", b"image" => "image", b"barcode" => "barcode", b"target" => "target",
        b"point" => "point", b"grid_item" => "grid_item", b"constraint" => "constraint",
        b"drill_chart" | b"drill_map" | b"stackup_table" => "generated_table",
        b"embedded_files" => "embedded_files", b"variants" => "variants", b"net_chains" => "net_chains",
        _ => return None,
    })
}
fn footprint_skip(key: &[u8]) -> Option<&'static str> {
    Some(match key {
        b"fp_text_box" => "textbox", b"table" => "table", b"image" => "image", b"barcode" => "barcode",
        b"dimension" => "dimension", b"group" => "group", b"point" => "point", b"constraint" => "constraint",
        b"embedded_files" => "embedded_files", b"custom_property" => "custom_property", _ => return None,
    })
}
fn legacy_layer(name: &str) -> &str {
    match name {
        "Adesivo.Retro" | "Kleju_Dolna" | "Dessous.Adhes" => "B.Adhes",
        "Adesivo.Fronte" | "Kleju_Gorna" | "Dessus.Adhes" => "F.Adhes",
        "Pasta.Retro" | "Pasty_Dolna" | "Dessous.Pate" => "B.Paste",
        "Pasta.Fronte" | "Pasty_Gorna" | "Dessus.Pate" => "F.Paste",
        "Serigrafia.Retro" | "Opisowa_Dolna" | "Dessous.SilkS" => "B.SilkS",
        "Serigrafia.Fronte" | "Opisowa_Gorna" | "Dessus.SilkS" => "F.SilkS",
        "Maschera.Retro" | "Maski_Dolna" | "Dessous.Masque" => "B.Mask",
        "Maschera.Fronte" | "Maski_Gorna" | "Dessus.Masque" => "F.Mask",
        "Grafica" | "Rysunkowa" | "Dessin.User" => "Dwgs.User",
        "Commenti" | "Komentarzy" => "Cmts.User", "Eco1" | "ECO1" => "Eco1.User",
        "Eco2" | "ECO2" => "Eco2.User", "Contorno.scheda" | "Krawedziowa" | "Contours.Ci" => "Edge.Cuts",
        _ => name,
    }
}

#[derive(Copy, Clone, Default)]
struct TentSpec { front: Option<bool>, back: Option<bool> }
impl TentSpec {
    fn resolve(self, default: Tent) -> Tent {
        let front = self.front.unwrap_or(matches!(default, Tent::Both | Tent::Front));
        let back = self.back.unwrap_or(matches!(default, Tent::Both | Tent::Back));
        match (front, back) { (true, true) => Tent::Both, (true, false) => Tent::Front,
            (false, true) => Tent::Back, (false, false) => Tent::None }
    }
}

enum ZoneRead { Zone(FileZone), Keepout(FileKeepout), Skipped }

impl<'a> Parser<'a> {
    fn tenting(&mut self, h: Head<'a>) -> Result<TentSpec, Refusal> {
        self.require_list(h)?;
        let mut tent = TentSpec { front: Some(false), back: Some(false) };
        let mut first = true;
        while let Some(c) = self.child()? {
            if first && c.list { tent = TentSpec::default(); }
            first = false;
            if !matches!(c.key, b"front" | b"back") { self.unknown(c)?; continue; }
            let value = if !c.list { self.skip(c)?; Some(true) }
            else {
                let t = self.take()?;
                let v = match (t.kind, self.lex.raw(t)) {
                    (Kind::Symbol, b"yes" | b"true") => Some(true),
                    (Kind::Symbol, b"no" | b"false") => Some(false),
                    (Kind::Symbol, b"none") => None,
                    _ => return Err(self.error(t, "need yes, no or none for tenting")),
                };
                self.rest()?; v
            };
            if c.key == b"front" { tent.front = value; } else { tent.back = value; }
        }
        Ok(tent)
    }
    fn stackup(&mut self, h: Head<'a>) -> Result<FileStackup, Refusal> {
        let mut stack = FileStackup { location: h.loc(), ..FileStackup::default() };
        while let Some(c) = self.child()? {
            match c.key {
                b"layer" => {
                    self.require_list(c)?;
                    let t = self.name_token("stack layer")?;
                    let name = self.lex.text(t)?;
                    let dielectric = name.starts_with("dielectric");
                    if name == "dielectric" && self.peek()?.kind == Kind::Number { self.take()?; }
                    let id = if dielectric { None } else { self.layer_name(t)? };
                    let kind = match id {
                        Some(l) if l.is_copper() => Some(StackKind::Copper(l)),
                        Some(l) => {
                            let side = if l.side() == LayerSide::Back { Side::Bottom } else { Side::Top };
                            match l.kind() { LayerKind::Silk => Some(StackKind::Silk(side)),
                                LayerKind::Mask => Some(StackKind::Mask(side)), LayerKind::Paste => Some(StackKind::Paste(side)),
                                _ => None }
                        }
                        None if dielectric => Some(StackKind::Dielectric { core: false }),
                        _ => None,
                    };
                    let Some(kind) = kind else { self.skip(c)?; continue; };
                    let mut layer = StackLayer { kind, thickness: 0, material: String::new(), er: None, loss_tan: None,
                        role: id.map(|l| self.layers.role(l)).unwrap_or_default(), velocity: None };
                    let mut at = c.loc();
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"type" => {
                                let typ = self.string(d)?;
                                if dielectric { layer.kind = StackKind::Dielectric { core: typ == "core" }; }
                            }
                            b"thickness" => {
                                layer.thickness = self.length("thickness")?;
                                while let Some(e) = self.child()? {
                                    if e.key == b"locked" { self.skipped(e, "stackup.extras")?; }
                                    else { self.unknown(e)?; }
                                }
                            }
                            b"material" => layer.material = self.string(d)?,
                            b"epsilon_r" => layer.er = Some(self.scalar(d)?),
                            b"loss_tangent" => layer.loss_tan = Some(self.scalar(d)?),
                            b"color" => {
                                let value = self.string(d)?;
                                if let Some(color) = stack_color(&value) {
                                    match layer.kind { StackKind::Mask(_) => stack.mask_color = Some(color),
                                        StackKind::Silk(_) => stack.silk_color = Some(color), _ => {} }
                                } else { self.gap("stackup.color", d.loc()); }
                            }
                            b"addsublayer" => {
                                stack.layers.push(Located { at, value: layer.clone() }); at = d.loc();
                                layer.thickness = 0; layer.material.clear(); layer.er = None; layer.loss_tan = None;
                                self.skip(d)?;
                            }
                            b"spec_frequency" | b"dielectric_model" | b"locked" => self.skipped(d, "stackup.extras")?,
                            _ => self.unknown(d)?,
                        }
                    }
                    stack.layers.push(Located { at, value: layer });
                }
                b"copper_finish" => {
                    let value = self.string(c)?;
                    stack.finish = Some(match value.as_str() {
                        "None" => Finish::None, "HAL SnPb" => Finish::Hasl, "HAL lead-free" => Finish::LeadFreeHasl,
                        "ENIG" => Finish::Enig, "OSP" | "HT_OSP" => Finish::Osp,
                        "Immersion silver" => Finish::ImmersionSilver, "Immersion tin" => Finish::ImmersionTin,
                        _ => { self.gap("stackup.finish", c.loc()); Finish::None }
                    });
                }
                b"dielectric_constraints" | b"edge_connector" | b"castellated_pads" | b"edge_plating"
                    | b"spec_frequency" | b"dielectric_model" | b"locked" => self.skipped(c, "stackup.extras")?,
                _ => self.unknown(c)?,
            }
        }
        Ok(stack)
    }
    fn track(&mut self, h: Head<'a>) -> Result<Option<TrackPiece>, Refusal> {
        let (mut start, mut mid, mut end, mut width, mut layer) = (None, None, None, None, None);
        let mut layer_seen = false;
        let mut locked = false;
        let mut net = NetRef::default();
        self.report.count_read(if h.key == b"arc" { "arcs" } else { "segments" });
        while let Some(c) = self.child()? {
            match c.key {
                b"start" => start = Some(self.pair(c, true)?), b"mid" => mid = Some(self.pair(c, true)?),
                b"end" => end = Some(self.pair(c, true)?), b"width" => width = Some(self.nm(c)?),
                b"layer" => { layer = self.single_layer(c)?; layer_seen = true; }
                b"layers" => {
                    let mask = self.layer_mask(c)?;
                    let cu = mask.copper_only();
                    if cu.len() != 1 { return Err(self.error(c.token, "track layers must contain exactly one copper layer")); }
                    layer = cu.iter().next(); layer_seen = true;
                    if !mask.minus(cu).is_empty() { self.gap("track.mask_opening", c.loc()); }
                }
                b"net" => net = self.net_ref(c)?, b"locked" => locked = self.flag(c)?,
                b"tstamp" | b"uuid" | b"status" => self.skip(c)?,
                b"solder_mask_margin" => self.skipped(c, "track.mask_opening")?,
                b"custom_property" => self.skipped(c, "custom_property")?, _ => self.unknown(c)?,
            }
        }
        let start = self.required(start, h, "start")?; let end = self.required(end, h, "end")?;
        let width = self.required(width, h, "width")?;
        if h.key == b"arc" { mid = Some(self.required(mid, h, "mid")?); }
        if !layer_seen { return Err(self.error(h.token, "missing required 'layer'")); }
        let Some(layer) = layer else { return Ok(None); };
        if !layer.is_copper() { self.dropped("track.non_copper", h.loc()); return Ok(None); }
        if mid.is_some_and(|m| ArcGeom::new(start, m, end).is_none()) { mid = None; }
        Ok(Some(TrackPiece { location: h.loc(), layer, net, width, locked, start, mid, end }))
    }
    fn via(&mut self, h: Head<'a>) -> Result<(FileVia, TentSpec), Refusal> {
        let (mut at, mut size, mut drill, mut span) = (None, None, None, None);
        let mut locked = false;
        let mut kind = ViaKind::Through;
        let mut net = NetRef::default();
        let mut tent = TentSpec::default();
        let mut stack_size: Option<Nm> = None;
        let (mut remove_unused, mut keep_ends, mut start_end) = (false, false, false);
        let mut zone_layers = None;
        while let Some(c) = self.child()? {
            match c.key {
                b"blind" | b"buried" | b"micro" if !c.list => {
                    kind = match c.key { b"blind" => ViaKind::Blind, b"buried" => ViaKind::Buried, _ => ViaKind::Micro };
                    self.skip(c)?;
                }
                b"at" => at = Some(self.pair(c, true)?), b"size" => size = Some(self.nm(c)?),
                b"drill" => drill = Some(self.nm(c)?), b"locked" => locked = self.flag(c)?,
                b"layers" => {
                    let a = self.name_token("via layer")?; let la = self.layer_name(a)?;
                    let b = self.name_token("via layer")?; let lb = self.layer_name(b)?;
                    let la = la.filter(|l| l.is_copper()).ok_or_else(|| self.error(a, "via layer must be copper"))?;
                    let lb = lb.filter(|l| l.is_copper()).ok_or_else(|| self.error(b, "via layer must be copper"))?;
                    span = Some((la.min(lb), la.max(lb))); self.rest()?;
                }
                b"net" => net = self.net_ref(c)?, b"tenting" => tent = self.tenting(c)?,
                b"padstack" => {
                    self.approx("via.stack", c.loc());
                    while let Some(d) = self.child()? {
                        if d.key == b"layer" && d.list {
                            self.name("via stack layer")?;
                            while let Some(e) = self.child()? {
                                if e.key == b"size" {
                                    let n = self.nm(e)?; stack_size = Some(stack_size.map_or(n, |old| old.max(n)));
                                } else { self.skip(e)?; }
                            }
                        } else { self.skip(d)?; }
                    }
                }
                b"remove_unused_layers" => remove_unused = self.flag(c)?,
                b"keep_end_layers" => keep_ends = self.flag(c)?,
                b"start_end_only" => start_end = self.flag(c)?,
                b"zone_layer_connections" => zone_layers = Some(self.layer_mask(c)?.intersect(self.layers.copper_set())),
                b"teardrops" => self.skipped(c, "teardrops")?,
                b"covering" | b"plugging" | b"filling" | b"capping" => self.skipped(c, "via.protection")?,
                b"backdrill" | b"tertiary_drill" | b"front_post_machining" | b"back_post_machining" => self.skipped(c, "via.drill_extras")?,
                b"free" => self.skipped(c, "via.free")?, b"tstamp" | b"uuid" | b"status" => self.skip(c)?,
                _ => self.unknown(c)?,
            }
        }
        let at = self.required(at, h, "at")?;
        let size = self.required(size, h, "size")?;
        let drill = self.required(drill, h, "drill")?;
        let mut span = self.required(span, h, "layers")?;
        if kind == ViaKind::Blind && self.report.version < 20250926
            && span.0 != LayerId::F_CU && span.1 != LayerId::B_CU { kind = ViaKind::Buried; }
        if kind == ViaKind::Through { span = (LayerId::F_CU, LayerId::B_CU); }
        self.report.count_read(match kind { ViaKind::Through => "vias.through", ViaKind::Blind => "vias.blind",
            ViaKind::Buried => "vias.buried", ViaKind::Micro => "vias.micro" });
        // End layers only wins over removal; keeping the end layers means nothing without removal.
        let flash = match (start_end, remove_unused, keep_ends) {
            (true, _, _) => Flash::Ends,
            (false, false, _) => Flash::All,
            (false, true, false) => Flash::Connected,
            (false, true, true) => Flash::ConnectedEnds,
        };
        match flash {
            Flash::All => {}
            Flash::Connected => self.report.count_read("vias.flash_connected"),
            Flash::ConnectedEnds => self.report.count_read("vias.flash_connected_ends"),
            Flash::Ends => self.report.count_read("vias.flash_ends"),
        }
        Ok((FileVia { location: h.loc(), at, size: stack_size.map_or(size, |n| n.max(size)), drill, span, kind,
            tent: Tent::Both, net, locked, flash, zone_layers }, tent))
    }
    fn points(&mut self, h: Head<'a>) -> Result<Option<Path>, Refusal> {
        self.require_list(h)?;
        let mut path: Option<Path> = None;
        while let Some(c) = self.child()? {
            match c.key {
                b"xy" => {
                    let p = self.pair(c, true)?;
                    if let Some(path) = &mut path { path.segs.push(Seg::Line(p)); } else { path = Some(Path::new(p)); }
                }
                b"arc" => {
                    let (mut a, mut m, mut b) = (None, None, None);
                    while let Some(d) = self.child()? {
                        match d.key { b"start" => a = Some(self.pair(d, true)?),
                            b"mid" => m = Some(self.pair(d, true)?), b"end" => b = Some(self.pair(d, true)?),
                            _ => self.unknown(d)?, }
                    }
                    let a = self.required(a, c, "start")?; let m = self.required(m, c, "mid")?;
                    let b = self.required(b, c, "end")?;
                    let p = path.get_or_insert_with(|| Path::new(a));
                    if p.end() != a { p.segs.push(Seg::Line(a)); }
                    p.segs.push(arc_seg(a, m, b));
                }
                _ => self.unknown(c)?,
            }
        }
        Ok(path)
    }
    // Buffer only the candidate item's tokens. Stored fills never enter the buffer.
    fn capture_body(&mut self, omit_fills: bool) -> Result<(Vec<Token>, Option<Location>), Refusal> {
        let mut body = Vec::new();
        let mut depth = 1usize;
        let mut root_keyword = false;
        let mut fill = None;
        loop {
            let t = self.take()?;
            if root_keyword && omit_fills && t.kind == Kind::Symbol
                && matches!(self.lex.raw(t), b"filled_polygon" | b"fill_segments") {
                body.pop();
                fill.get_or_insert(t.location());
                self.lex.skip_list()?;
                depth -= 1;
                root_keyword = false;
                continue;
            }
            root_keyword = t.kind == Kind::Open && depth == 1;
            body.push(t);
            match t.kind {
                Kind::Open => depth += 1,
                Kind::Close => { depth -= 1; if depth == 0 { break; } }
                Kind::Eof => return Err(self.error(t, "end of input inside a list")),
                _ => {}
            }
        }
        Ok((body, fill))
    }
    fn body_children(&self, body: &[Token]) -> Vec<(Head<'a>, usize, usize)> {
        let mut children = Vec::new();
        let mut i = 0;
        while let Some(&t) = body.get(i) {
            if t.kind == Kind::Close { break; }
            if t.kind == Kind::Open {
                let Some(&key) = body.get(i + 1) else { break; };
                let mut end = i + 1;
                let mut depth = 1usize;
                while let Some(t) = body.get(end) {
                    match t.kind { Kind::Open => depth += 1, Kind::Close => depth -= 1, _ => {} }
                    end += 1;
                    if depth == 0 { break; }
                }
                children.push((Head { token: key, key: if key.kind == Kind::Symbol { self.lex.raw(key) } else { b"" },
                    list: true }, (i + 2).min(body.len()), end));
                i = end;
            } else {
                children.push((Head { token: t, key: if t.kind == Kind::Symbol { self.lex.raw(t) } else { b"" },
                    list: false }, i + 1, i + 1));
                i += 1;
            }
        }
        children
    }
    fn hidden_field(&self, body: &[Token]) -> bool {
        let mut hidden = false;
        let mut effects_hidden = false;
        for (h, start, end) in self.body_children(body) {
            if h.key == b"hide" { hidden = self.buffered_flag(h, &body[start..end]); }
            else if h.key == b"effects" && h.list {
                for (c, lo, hi) in self.body_children(&body[start..end]) {
                    if c.key == b"hide" { effects_hidden = self.buffered_flag(c, &body[start + lo..start + hi]); }
                }
            }
        }
        hidden || effects_hidden
    }
    fn buffered_flag(&self, h: Head<'a>, values: &[Token]) -> bool {
        !h.list || values.first().is_some_and(|t| t.kind == Kind::Close
            || (t.kind == Kind::Symbol && matches!(self.lex.raw(*t), b"yes" | b"true")))
    }
    fn zone(&mut self, h: Head<'a>, footprint: bool) -> Result<ZoneRead, Refusal> {
        self.require_list(h)?;
        let (body, fill) = self.capture_body(true)?;
        let children = self.body_children(&body);
        let keepout = children.iter().any(|(c, _, _)| c.key == b"keepout");
        let placement = children.iter().any(|(c, _, _)| c.key == b"placement");
        let feature = if !keepout && placement { Some("zone.placement_area") }
            else if !keepout && footprint { Some("footprint.zone") } else { None };
        if let Some(feature) = feature {
            self.report.count_read("zones"); self.gap(feature, h.loc()); self.path.pop(); return Ok(ZoneRead::Skipped);
        }
        let mut layers = LayerSet::EMPTY;
        for &(c, start, end) in &children {
            if matches!(c.key, b"layer" | b"layers") {
                self.path.push(c.key);
                self.lex.replay_tokens(body[start..end].iter().copied());
                layers = if c.key == b"layer" { self.single_layer(c)?.map_or(LayerSet::EMPTY, LayerSet::one) }
                    else { self.layer_mask(c)? };
            }
        }
        if !keepout && !layers.is_empty() && layers.copper_only().is_empty() {
            self.report.count_read("zones"); self.gap("zone.non_copper", h.loc()); self.path.pop();
            return Ok(ZoneRead::Skipped);
        }
        if let Some(at) = fill { self.gap("zone.stored_fill", at); }
        self.lex.replay_tokens(body);
        self.zone_body(h, footprint, layers, keepout)
    }
    fn zone_body(&mut self, h: Head<'a>, footprint: bool, selected_layers: LayerSet, rule_area: bool)
        -> Result<ZoneRead, Refusal>
    {
        let mut net = NetRef::default();
        let mut legacy_name = None;
        let mut layers = selected_layers;
        let mut name = String::new();
        let mut locked = false;
        let mut priority = 0;
        let mut settings = ZoneSettings::default();
        let mut outlines = Vec::new();
        let mut keepout = None;
        let mut placement = false;
        while let Some(c) = self.child()? {
            if rule_area && matches!(c.key, b"net" | b"net_name" | b"connect_pads" | b"min_thickness" | b"fill" | b"priority") {
                self.skip(c)?; continue;
            }
            match c.key {
                b"net" => net = self.net_ref(c)?, b"net_name" => legacy_name = Some(self.string(c)?),
                b"layer" | b"layers" => self.skip(c)?, b"name" => name = self.string(c)?,
                b"locked" => locked = self.flag(c)?, b"priority" => priority = self.int(c)?.clamp(0, u16::MAX as i64) as u16,
                b"min_thickness" => settings.min_width = self.nm(c)?,
                b"connect_pads" => {
                    if self.peek()?.kind == Kind::Symbol {
                        let t = self.take()?;
                        settings.connect = match self.lex.raw(t) {
                            b"yes" => ZoneConnect::Solid, b"no" => ZoneConnect::Off,
                            b"thru_hole_only" => ZoneConnect::ThruThermal,
                            _ => { self.report.unknown(format!("{}>{}", self.path_string(), self.lex.text(t)?), t.location()); ZoneConnect::Thermal }
                        };
                    }
                    while let Some(d) = self.child()? {
                        if d.key == b"clearance" { settings.clearance = Some(self.nm(d)?); } else { self.unknown(d)?; }
                    }
                }
                b"fill" => self.zone_fill(c, &mut settings)?,
                b"polygon" => {
                    while let Some(d) = self.child()? {
                        if d.key == b"pts" {
                            if let Some(p) = self.points(d)? { outlines.push(close_path(p)); }
                        } else { self.unknown(d)?; }
                    }
                }
                b"keepout" => {
                    let mut forbid = Forbid::default();
                    while let Some(d) = self.child()? {
                        let bit = match d.key { b"tracks" => Forbid::TRACKS, b"vias" => Forbid::VIAS,
                            b"pads" => Forbid::PADS, b"copperpour" => Forbid::ZONES, b"footprints" => Forbid::FOOTPRINTS,
                            _ => { self.unknown(d)?; continue; }
                        };
                        let t = self.take()?;
                        match (t.kind, self.lex.raw(t)) {
                            (Kind::Symbol, b"not_allowed") => forbid = forbid.with(bit),
                            (Kind::Symbol, b"allowed") => {},
                            _ => return Err(self.error(t, "need allowed or not_allowed")),
                        }
                        self.rest()?;
                    }
                    keepout = Some(forbid);
                }
                b"placement" => { placement = true; self.skip(c)?; }
                b"attr" => {
                    while let Some(d) = self.child()? {
                        if d.key == b"teardrop" { self.skipped(d, "teardrops")?; } else { self.unknown(d)?; }
                    }
                }
                b"hatch" | b"filled_areas_thickness" | b"uuid" | b"tstamp" => self.skip(c)?,
                b"property" => self.skipped(c, "zone.layer_props")?,
                b"custom_property" => self.skipped(c, "custom_property")?, _ => self.unknown(c)?,
            }
        }
        if keepout.is_some() { self.report.count_read("keepouts"); } else { self.report.count_read("zones"); }
        if outlines.is_empty() { self.dropped("zone.empty", h.loc()); return Ok(ZoneRead::Skipped); }
        if placement && keepout.is_none() { self.gap("zone.placement_area", h.loc()); return Ok(ZoneRead::Skipped); }
        if footprint && keepout.is_none() { self.gap("footprint.zone", h.loc()); return Ok(ZoneRead::Skipped); }
        if layers.is_empty() { self.dropped("zone.no_layers", h.loc()); return Ok(ZoneRead::Skipped); }
        if keepout.is_none() && layers.copper_only().is_empty() {
            self.gap("zone.non_copper", h.loc()); return Ok(ZoneRead::Skipped);
        }
        if keepout.is_none() { layers = layers.intersect(self.layers.copper_set()); }
        if layers.is_empty() { self.dropped("zone.no_layers", h.loc()); return Ok(ZoneRead::Skipped); }
        let rings: Vec<_> = outlines.iter().map(path_ring).collect();
        let area = PolySet::from_rings_even_odd(&rings);
        if let Some(forbid) = keepout {
            return Ok(ZoneRead::Keepout(FileKeepout { location: h.loc(), area, layers, forbid, reason: name }));
        }
        net.zone_name = legacy_name;
        Ok(ZoneRead::Zone(FileZone { location: h.loc(), net, name, layers, area, priority, settings, locked }))
    }
    fn path_string(&self) -> String {
        self.path.iter().map(|p| String::from_utf8_lossy(p)).collect::<Vec<_>>().join(">")
    }
    fn zone_fill(&mut self, _h: Head<'a>, s: &mut ZoneSettings) -> Result<(), Refusal> {
        while let Some(c) = self.child()? {
            match c.key {
                b"thermal_gap" => s.thermal.gap = self.nm(c)?,
                b"thermal_bridge_width" => s.thermal.spoke = self.nm(c)?,
                b"island_removal_mode" => {
                    s.islands = match self.int(c)? { 1 => Islands::Keep,
                        2 => { self.approx("zone.island_area", c.loc()); Islands::Remove }, _ => Islands::Remove };
                }
                b"mode" => match self.enumeration(c)? {
                    b"hatch" => self.approx("zone.hatch_fill", c.loc()),
                    b"thieving" => self.gap("zone.thieving", c.loc()), _ => {}
                },
                b"smoothing" => {
                    if matches!(self.enumeration(c)?, b"chamfer" | b"fillet") { self.gap("zone.smoothing", c.loc()); }
                }
                b"yes" | b"arc_segments" | b"island_area_min" | b"radius" => self.skip(c)?,
                key if key.starts_with(b"hatch_") => self.skip(c)?,
                _ => self.unknown(c)?,
            }
        }
        s.thermal.angle = Angle::ZERO; Ok(())
    }
}

fn stack_color(s: &str) -> Option<u32> {
    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() { 6 => u32::from_str_radix(hex, 16).ok().map(|n| (n << 8) | 255),
            8 => u32::from_str_radix(hex, 16).ok(), _ => None };
    }
    Some(match s { "Green" => 0x1a5c2eff, "Red" => 0xb02020ff, "Blue" => 0x1a3c8cff,
        "Yellow" => 0xd8c02aff, "Purple" => 0x5a2a82ff, "Black" => 0x141414ff, "White" => 0xf0f0f0ff,
        "Matte Green" => 0x2a5a34ff, "Matte Black" => 0x202020ff, _ => return None })
}
fn arc_seg(a: P, mid: P, end: P) -> Seg {
    if ArcGeom::new(a, mid, end).is_some() { Seg::Arc { mid, end } } else { Seg::Line(end) }
}
fn close_path(mut p: Path) -> Path {
    if p.end() != p.start { p.segs.push(Seg::Line(p.start)); } p
}
fn path_ring(path: &Path) -> Vec<P> {
    let mut ring = vec![path.start];
    for (a, seg) in path.steps() {
        match seg {
            Seg::Arc { mid, end } => if let Some(arc) = ArcGeom::new(a, mid, end) { arc.chord_points(ARC_ERR, &mut ring); }
                else { ring.push(end); },
            Seg::Line(b) => ring.push(b),
        }
    }
    if ring.len() > 1 && ring.first() == ring.last() { ring.pop(); }
    ring
}

#[derive(Copy, Clone, PartialEq)]
enum GraphicKind { Line, Arc, Circle, Rect, Poly, Curve, Ellipse, EllipseArc }
fn graphic_kind(key: &[u8]) -> Option<GraphicKind> {
    Some(match key {
        b"gr_line" | b"fp_line" | b"gr_vector" => GraphicKind::Line,
        b"gr_arc" | b"fp_arc" => GraphicKind::Arc, b"gr_circle" | b"fp_circle" => GraphicKind::Circle,
        b"gr_rect" | b"fp_rect" | b"gr_bbox" => GraphicKind::Rect,
        b"gr_poly" | b"fp_poly" => GraphicKind::Poly, b"gr_curve" | b"fp_curve" => GraphicKind::Curve,
        b"gr_ellipse" | b"fp_ellipse" => GraphicKind::Ellipse,
        b"gr_ellipse_arc" | b"fp_ellipse_arc" => GraphicKind::EllipseArc, _ => return None,
    })
}
impl GraphicKind {
    fn count(self) -> &'static str {
        match self { Self::Line => "graphics.line", Self::Arc => "graphics.arc", Self::Circle => "graphics.circle",
            Self::Rect => "graphics.rect", Self::Poly => "graphics.poly", Self::Curve => "graphics.curve",
            Self::Ellipse => "graphics.ellipse", Self::EllipseArc => "graphics.ellipse_arc" }
    }
}

struct GraphicRead {
    kind: GraphicKind,
    layers: LayerSet,
    path: Path,
    solid: Option<Shape>,
    closed: bool,
    fill: Option<bool>,
    width: Nm,
    locked: bool,
}
impl GraphicRead {
    fn filled(&self, layer: LayerId) -> bool {
        self.fill.unwrap_or(match self.kind { GraphicKind::Poly => layer != LayerId::EDGE,
            GraphicKind::Rect | GraphicKind::Circle => self.width == 0, _ => false })
    }
    fn graphics(&self, layer: LayerId) -> Vec<Gfx> {
        let filled = self.filled(layer);
        let width = if !filled && self.width <= 0 { 100_000 } else { self.width };
        let mut out = Vec::new();
        if filled {
            if let Some(shape) = &self.solid {
                let pen = width / 2;
                let grown = match shape {
                    Shape::Circle { c, r } => Shape::Circle { c: *c, r: r.saturating_add(pen) },
                    Shape::Rect { c, half, rot, corner } => Shape::Rect { c: *c,
                        half: P::new(half.x.saturating_add(pen), half.y.saturating_add(pen)),
                        rot: *rot, corner: corner.saturating_add(pen) },
                    _ => shape.clone(),
                };
                out.push(Gfx::Fill(grown));
                if self.kind != GraphicKind::Poly { return out; }
            }
        }
        if !filled || self.solid.is_none() || width > 0 {
            out.push(Gfx::Stroke { path: self.path.clone(), w: width });
        }
        out
    }
    fn edge(&self, location: Location) -> EdgePiece {
        let width = if !self.filled(LayerId::EDGE) && self.width <= 0 { 100_000 } else { self.width };
        EdgePiece { location, path: self.path.clone(), closed: self.closed, width, locked: self.locked }
    }
    fn flip_y(&mut self) {
        self.path = self.path.mapped(flip_y);
        self.solid = self.solid.as_ref().map(shape_flip_y);
    }
}

impl<'a> Parser<'a> {
    fn graphic(&mut self, h: Head<'a>, primitive: bool) -> Result<GraphicRead, Refusal> {
        let kind = graphic_kind(h.key).ok_or_else(|| self.error(h.token, "need a graphic"))?;
        self.report.count_read(kind.count());
        let (mut start, mut mid, mut end, mut center, mut points) = (None, None, None, None, None);
        let mut width = 0;
        let mut radius = 0;
        let mut sweep = None;
        let mut fill = None;
        let mut locked = false;
        let mut layers = LayerSet::EMPTY;
        let (mut major, mut minor, mut rotation, mut start_angle, mut end_angle) = (None, None, 0.0, 0.0, 360.0);
        while let Some(c) = self.child()? {
            match c.key {
                b"start" => start = Some(self.pair(c, true)?), b"mid" => mid = Some(self.pair(c, true)?),
                b"end" => end = Some(self.pair(c, true)?), b"center" => center = Some(self.pair(c, true)?),
                b"pts" => points = self.points(c)?, b"width" => width = self.nm(c)?, b"radius" => radius = self.nm(c)?,
                b"angle" if kind == GraphicKind::Arc => sweep = Some(self.scalar(c)?),
                b"angle" | b"tstamp" | b"uuid" | b"status" => self.skip(c)?,
                b"layer" => layers = self.single_layer(c)?.map_or(LayerSet::EMPTY, LayerSet::one),
                b"layers" => layers = self.layer_mask(c)?, b"locked" => locked = self.flag(c)?,
                b"fill" => {
                    let t = self.take()?;
                    if t.kind != Kind::Symbol { return Err(self.error(t, "need an unquoted fill value")); }
                    fill = Some(match self.lex.raw(t) {
                        b"yes" | b"solid" => true, b"no" | b"none" => false,
                        b"hatch" | b"reverse_hatch" | b"cross_hatch" => { self.approx("graphic.hatch_fill", c.loc()); true }
                        _ => { self.report.unknown(format!("{}>{}", self.path_string(), self.lex.text(t)?), t.location()); false }
                    }); self.rest()?;
                }
                b"stroke" => {
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"width" => width = self.nm(d)?,
                            b"type" => {
                                if !matches!(self.enumeration(d)?, b"solid" | b"default") { self.gap("graphic.dashed", d.loc()); }
                            }
                            b"color" => self.skip(d)?, _ => self.unknown(d)?,
                        }
                    }
                }
                b"major_radius" => major = Some(self.nm(c)?), b"minor_radius" => minor = Some(self.nm(c)?),
                b"rotation_angle" => rotation = self.scalar(c)?, b"start_angle" => start_angle = self.scalar(c)?,
                b"end_angle" => end_angle = self.scalar(c)?,
                b"net" => self.skipped(c, "graphic.copper_net")?,
                b"solder_mask_margin" => self.skipped(c, "graphic.mask_margin")?,
                b"start_shape" | b"end_shape" => self.skipped(c, "graphic.line_ending")?,
                b"custom_property" => self.skipped(c, "custom_property")?, _ => self.unknown(c)?,
            }
        }
        if primitive { layers = LayerSet::one(LayerId::F_CU); }
        let mut closed = false;
        let mut solid = None;
        let path = match kind {
            GraphicKind::Line => Path::new(self.required(start, h, "start")?).line(self.required(end, h, "end")?),
            GraphicKind::Arc => {
                let a = self.required(start, h, "start")?; let b = self.required(end, h, "end")?;
                let (a, m, b) = if self.report.version <= 20210925 {
                    let sweep = self.required(sweep, h, "angle")?;
                    (b, turn_about(b, a, angle(-sweep / 2.0)), turn_about(b, a, angle(-sweep)))
                } else { (a, self.required(mid, h, "mid")?, b) };
                Path { start: a, segs: vec![arc_seg(a, m, b)] }
            }
            GraphicKind::Circle => {
                let c = self.required(center, h, "center")?; let e = self.required(end, h, "end")?;
                let r = ((e.x as f64 - c.x as f64).hypot(e.y as f64 - c.y as f64)).round() as Nm;
                closed = true; solid = Some(Shape::Circle { c, r }); circle_path(c, r)
            }
            GraphicKind::Rect => {
                let a = self.required(start, h, "start")?; let b = self.required(end, h, "end")?;
                let min = P::new(a.x.min(b.x), a.y.min(b.y)); let max = P::new(a.x.max(b.x), a.y.max(b.y));
                let c = P::from_f((min.x as f64 + max.x as f64) / 2.0, (min.y as f64 + max.y as f64) / 2.0);
                let half = P::new(((max.x as i64 - min.x as i64) / 2) as Nm, ((max.y as i64 - min.y as i64) / 2) as Nm);
                solid = Some(Shape::Rect { c, half, rot: Angle::ZERO, corner: radius }); closed = true;
                rect_path(min, max, radius)
            }
            GraphicKind::Poly => {
                let p = close_path(self.required(points, h, "pts")?); closed = true;
                solid = Some(Shape::Poly(PolySet::from_rings_even_odd(&[path_ring(&p)]))); p
            }
            GraphicKind::Curve => {
                let p = self.required(points, h, "pts")?;
                let pts: Vec<_> = std::iter::once(p.start).chain(p.segs.iter().map(Seg::end)).collect();
                if pts.len() != 4 { return Err(self.error(h.token, "a cubic curve needs four points")); }
                self.approx("graphic.bezier_approx", h.loc()); bezier_path([pts[0], pts[1], pts[2], pts[3]])
            }
            GraphicKind::Ellipse | GraphicKind::EllipseArc => {
                let c = self.required(center, h, "center")?;
                let a = self.required(major, h, "major_radius")?; let b = self.required(minor, h, "minor_radius")?;
                closed = kind == GraphicKind::Ellipse;
                self.approx("graphic.ellipse_approx", h.loc());
                let p = ellipse_path(c, a, b, angle(rotation), start_angle, end_angle, closed);
                if closed { solid = Some(Shape::Poly(PolySet::from_rings_even_odd(&[path_ring(&p)]))); } p
            }
        };
        Ok(GraphicRead { kind, layers, path, solid, closed, fill, width, locked })
    }
}

// Derived geometry uses wide arithmetic: valid clamped coordinates can still be far apart.
fn safe_point(x: f64, y: f64) -> P {
    let bound = crate::sexpr::NM_LIMIT as f64;
    P::new(x.clamp(-bound, bound).round() as Nm, y.clamp(-bound, bound).round() as Nm)
}
fn add(a: P, b: P) -> P { safe_point(a.x as f64 + b.x as f64, a.y as f64 + b.y as f64) }
fn flip_y(p: P) -> P { P::new(p.x, p.y.saturating_neg()) }
fn turn_about(p: P, c: P, a: Angle) -> P {
    let (s, co) = a.sin_cos();
    let (x, y) = (p.x as f64 - c.x as f64, p.y as f64 - c.y as f64);
    safe_point(c.x as f64 + co * x - s * y, c.y as f64 + s * x + co * y)
}
fn shape_flip_y(s: &Shape) -> Shape {
    match s {
        Shape::Circle { c, r } => Shape::Circle { c: flip_y(*c), r: *r },
        Shape::Segment { a, b, w } => Shape::Segment { a: flip_y(*a), b: flip_y(*b), w: *w },
        Shape::Arc { a, mid, b, w } => Shape::Arc { a: flip_y(*a), mid: flip_y(*mid), b: flip_y(*b), w: *w },
        Shape::Rect { c, half, rot, corner } => Shape::Rect { c: flip_y(*c), half: *half, rot: rot.neg(), corner: *corner },
        Shape::Oval { c, half, rot } => Shape::Oval { c: flip_y(*c), half: *half, rot: rot.neg() },
        Shape::Poly(p) => Shape::Poly(map_poly(p, flip_y)),
    }
}
fn map_poly(p: &PolySet, map: impl Fn(P) -> P) -> PolySet {
    PolySet::from_rings_even_odd(&p.rings().map(|r| r.iter().map(|p| map(*p)).collect()).collect::<Vec<_>>())
}
fn circle_path(c: P, r: Nm) -> Path {
    let p = |x: f64, y: f64| safe_point(c.x as f64 + x, c.y as f64 + y);
    let r = r as f64;
    Path::new(p(r, 0.0)).arc(p(0.0, r), p(-r, 0.0)).arc(p(0.0, -r), p(r, 0.0))
}
fn rect_path(min: P, max: P, radius: Nm) -> Path {
    let (x, y, xx, yy) = (min.x as f64, min.y as f64, max.x as f64, max.y as f64);
    let r = (radius as f64).max(0.0).min(((xx - x) / 2.0).min((yy - y) / 2.0));
    if r == 0.0 { return close_path(Path::new(min).line(P::new(max.x, min.y)).line(max).line(P::new(min.x, max.y))); }
    let k = r * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
    close_path(Path::new(safe_point(x + r, y)).line(safe_point(xx - r, y))
        .arc(safe_point(xx - k, y + k), safe_point(xx, y + r)).line(safe_point(xx, yy - r))
        .arc(safe_point(xx - k, yy - k), safe_point(xx - r, yy)).line(safe_point(x + r, yy))
        .arc(safe_point(x + k, yy - k), safe_point(x, yy - r)).line(safe_point(x, y + r))
        .arc(safe_point(x + k, y + k), safe_point(x + r, y)))
}
fn bezier_path(points: [P; 4]) -> Path {
    let mut out = Path::new(points[0]);
    let mut pending = vec![points.map(|p| (p.x as f64, p.y as f64))];
    while let Some(p) = pending.pop() {
        let distance = |q: (f64, f64)| {
            let (dx, dy) = (p[3].0 - p[0].0, p[3].1 - p[0].1);
            let d = dx * dx + dy * dy;
            let t = if d == 0.0 { 0.0 } else { ((q.0 - p[0].0) * dx + (q.1 - p[0].1) * dy) / d }.clamp(0.0, 1.0);
            (q.0 - p[0].0 - t * dx).hypot(q.1 - p[0].1 - t * dy)
        };
        if distance(p[1]).max(distance(p[2])) <= ARC_ERR as f64 {
            out.segs.push(Seg::Line(safe_point(p[3].0, p[3].1))); continue;
        }
        let avg = |a: (f64, f64), b: (f64, f64)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let a = avg(p[0], p[1]); let b = avg(p[1], p[2]); let c = avg(p[2], p[3]);
        let d = avg(a, b); let e = avg(b, c); let m = avg(d, e);
        pending.push([m, e, c, p[3]]); pending.push([p[0], a, d, m]);
    }
    out
}
fn ellipse_path(c: P, a: Nm, b: Nm, rot: Angle, start: f64, end: f64, closed: bool) -> Path {
    let start = angle(start).radians();
    let sweep = if closed { std::f64::consts::TAU } else { (angle(end).radians() - start).rem_euclid(std::f64::consts::TAU) };
    let radius = (a as f64).abs().max((b as f64).abs());
    let step = if radius > ARC_ERR as f64 { (8.0 * ARC_ERR as f64 / radius).sqrt() } else { std::f64::consts::FRAC_PI_2 };
    let n = (sweep / step).ceil().max(1.0) as usize;
    let (s, co) = rot.sin_cos();
    let at = |t: f64| {
        let (x, y) = (a as f64 * t.cos(), b as f64 * t.sin());
        safe_point(c.x as f64 + co * x - s * y, c.y as f64 + s * x + co * y)
    };
    let mut path = Path::new(at(start));
    for i in 1..=n { path.segs.push(Seg::Line(at(start + sweep * i as f64 / n as f64))); }
    if closed { close_path(path) } else { path }
}

struct TextRead { value: Option<FileText>, hidden: bool, unlocked: bool }
#[derive(Default)]
struct TextEffects {
    height: Option<Nm>, width: Option<Nm>, thick: Option<Nm>, mirrored: bool, hidden: bool,
    aspect: bool, justify: bool, font: bool, style: bool, spacing: bool,
}
impl<'a> Parser<'a> {
    fn text(&mut self, h: Head<'a>, content: String) -> Result<TextRead, Refusal> {
        self.report.count_read("texts");
        let mut at = P::ZERO;
        let mut rot = Angle::ZERO;
        let mut layer = None;
        let mut layer_seen = false;
        let mut hidden = false;
        let mut locked = false;
        let mut unlocked = false;
        let mut knockout = false;
        let mut effects = TextEffects::default();
        while let Some(c) = self.child()? {
            match c.key {
                b"at" => { let p = self.position(c)?; at = p.0; rot = p.1; unlocked |= p.2; }
                b"layer" => {
                    let t = self.name_token("text layer")?; layer = self.layer_name(t)?; layer_seen = true;
                    while let Some(d) = self.child()? {
                        if d.key == b"knockout" { knockout |= self.flag(d)?; } else { self.unknown(d)?; }
                    }
                }
                b"effects" => self.effects(&mut effects)?,
                b"hide" => hidden = self.flag(c)?, b"locked" => locked = self.flag(c)?,
                b"unlocked" => unlocked |= self.flag(c)?,
                b"tstamp" | b"uuid" | b"render_cache" => self.skip(c)?,
                b"custom_property" => self.skipped(c, "custom_property")?, _ => self.unknown(c)?,
            }
        }
        if !layer_seen && h.key != b"property" {
            return Err(self.error(h.token, "missing required 'layer'"));
        }
        if !layer_seen && h.key == b"property" {
            // A field with no layer is never drawn (current files still write library filters this way).
            return Ok(TextRead { value: None, hidden: true, unlocked });
        }
        let height = effects.height.unwrap_or(1_524_000);
        effects.aspect |= effects.width.unwrap_or(height) != height;
        for (used, feature) in [(effects.aspect, "text.aspect"), (effects.justify, "text.justify"),
            (effects.font, "text.font"), (effects.style, "text.style"), (effects.spacing, "text.line_spacing"),
            (knockout, "text.knockout")] { if used { self.gap(feature, h.loc()); } }
        if layer.is_some_and(LayerId::is_copper) { self.gap("text.on_copper", h.loc()); }
        Ok(TextRead { value: layer.map(|layer| FileText { location: h.loc(), layer, text: content, at, rot,
            size: height, thick: effects.thick.unwrap_or_else(|| (height as f64 * 0.15).round() as Nm),
            mirrored: effects.mirrored, locked }), hidden: hidden || effects.hidden, unlocked })
    }
    fn effects(&mut self, e: &mut TextEffects) -> Result<(), Refusal> {
        while let Some(c) = self.child()? {
            match c.key {
                b"font" => {
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"size" => {
                                e.height = Some(self.length("text height")?); e.width = Some(self.length("text width")?);
                                self.rest()?;
                            }
                            b"thickness" => e.thick = Some(self.nm(d)?),
                            b"face" => { e.font = true; self.skip(d)?; }
                            b"bold" | b"italic" => e.style |= self.flag(d)?,
                            b"line_spacing" => e.spacing |= self.scalar(d)? != 1.0,
                            _ => self.unknown(d)?,
                        }
                    }
                }
                b"justify" => {
                    while let Some(d) = self.child()? {
                        match d.key { b"left" | b"right" | b"top" | b"bottom" => { e.justify = true; self.skip(d)?; }
                            b"mirror" => e.mirrored = self.flag(d)?, _ => self.unknown(d)?, }
                    }
                }
                b"hide" => e.hidden = self.flag(c)?, b"line_spacing" => e.spacing |= self.scalar(c)? != 1.0,
                _ => self.unknown(c)?,
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct PadDefaults {
    mask: Option<Nm>, paste: Option<Nm>, ratio: Option<i32>, zone: Option<ZoneConnect>,
    gap: Option<Nm>, spoke: Option<Nm>,
}
#[derive(Copy, Clone, PartialEq)]
enum PadForm { Circle, Rect, Oval, Trapezoid, RoundRect, Custom }
impl PadForm {
    fn read(value: &[u8]) -> Option<Self> {
        Some(match value { b"circle" => Self::Circle, b"rect" => Self::Rect, b"oval" => Self::Oval,
            b"trapezoid" => Self::Trapezoid, b"roundrect" => Self::RoundRect, b"custom" => Self::Custom,
            _ => return None })
    }
    fn count(self) -> &'static str {
        match self { Self::Circle => "pads.circle", Self::Rect => "pads.rect", Self::Oval => "pads.oval",
            Self::Trapezoid => "pads.trapezoid", Self::RoundRect => "pads.roundrect", Self::Custom => "pads.custom" }
    }
}
#[derive(Clone, Default)]
struct PadGeometry {
    form: Option<PadForm>, size: Option<P>, delta: P, round: u32, chamfer: u32, corners: u8,
    offset: P, offset_set: bool, round_anchor: bool, primitives: Vec<Shape>,
}
impl PadGeometry {
    fn shape(&self, bottom: bool, fallback: &Self) -> PadShape {
        let size = self.size.or(fallback.size).unwrap_or(P::new(1_000, 1_000));
        let form = self.form.or(fallback.form).unwrap_or(PadForm::Rect);
        if self.chamfer > 0 || self.corners != 0 {
            let corners = if bottom { ((self.corners & 3) << 2) | ((self.corners & 12) >> 2) } else { self.corners };
            return PadShape::Chamfer { size, ratio_ppm: self.round, chamfer_ppm: self.chamfer, corners };
        }
        match form {
            PadForm::Circle => PadShape::Circle { d: size.x }, PadForm::Rect => PadShape::Rect { size },
            PadForm::Oval => PadShape::Oval { size },
            PadForm::RoundRect => PadShape::RoundRect { size, ratio_ppm: self.round },
            PadForm::Trapezoid => PadShape::Trapezoid { size, delta: if bottom { flip_y(self.delta) } else { self.delta } },
            PadForm::Custom => PadShape::Custom {
                anchor: Box::new(if self.round_anchor { PadShape::Circle { d: size.x } } else { PadShape::Rect { size } }),
                shapes: self.primitives.iter().map(|s| if bottom { shape_flip_y(s) } else { s.clone() }).collect(),
            },
        }
    }
}
struct PadLayer { location: Location, name: String, geometry: PadGeometry }
struct PadRead {
    location: Location, no: String, kind: PadKind, geometry: PadGeometry, at: P, rot: Angle,
    drill: Option<P>, layers: LayerSet, all_copper: bool, net: NetRef, defaults: PadDefaults,
    thermal_angle: Option<f64>, die_len: Nm, prop: PadProp, stack: Vec<PadLayer>, custom_stack: bool,
    pin: Option<PadPin>,
}

impl<'a> Parser<'a> {
    fn margin(&mut self, h: Head<'a>) -> Result<Option<Nm>, Refusal> {
        let n = self.scalar(h)?;
        Ok(if n == 0.0 && self.report.version <= 20240201 { None } else { Some(mm_to_nm(n)) })
    }
    fn ratio(&mut self, h: Head<'a>) -> Result<Option<i32>, Refusal> {
        let n = self.scalar(h)?;
        Ok(if n == 0.0 && self.report.version <= 20240201 { None } else { Some(ppm(n)) })
    }
    fn connect(&mut self, h: Head<'a>) -> Result<Option<ZoneConnect>, Refusal> {
        Ok(match self.int(h)? { 0 => Some(ZoneConnect::Off), 1 => Some(ZoneConnect::Thermal),
            2 => Some(ZoneConnect::Solid), 3 => Some(ZoneConnect::ThruThermal), _ => None })
    }
    fn pad(&mut self, h: Head<'a>) -> Result<PadRead, Refusal> {
        let no = self.name("pad number")?;
        let t = self.take()?;
        if t.kind != Kind::Symbol { return Err(self.error(t, "need an unquoted pad type")); }
        let kind = match self.lex.raw(t) {
            b"smd" => PadKind::Smd, b"thru_hole" => PadKind::Thru, b"np_thru_hole" => PadKind::Npth,
            b"connect" => PadKind::Edge,
            b"non_component" => { self.approx("pad.non_component", h.loc()); PadKind::Smd }
            _ => return Err(self.error(t, "need smd, thru_hole, np_thru_hole, connect or non_component")),
        };
        let t = self.take()?;
        let form = if t.kind == Kind::Symbol { PadForm::read(self.lex.raw(t)) } else { None }
            .ok_or_else(|| self.error(t, "need a pad shape"))?;
        self.report.count_read(match kind { PadKind::Smd => "pads.smd", PadKind::Thru => "pads.thru",
            PadKind::Npth => "pads.npth", PadKind::Edge => "pads.edge" });
        self.report.count_read(form.count());
        let mut pad = PadRead { location: h.loc(), no, kind,
            geometry: PadGeometry { form: Some(form), ..PadGeometry::default() }, at: P::ZERO, rot: Angle::ZERO,
            drill: None, layers: LayerSet::EMPTY, all_copper: false, net: NetRef::default(), defaults: PadDefaults::default(),
            thermal_angle: None, die_len: 0, prop: PadProp::None, stack: Vec::new(), custom_stack: false, pin: None };
        while let Some(c) = self.child()? {
            if self.pad_geometry(c, &mut pad.geometry, false)? { continue; }
            match c.key {
                b"at" => { let (p, a, _) = self.position(c)?; pad.at = p; pad.rot = a; }
                b"drill" => {
                    let next = self.peek()?;
                    let oval = next.kind == Kind::Symbol && self.lex.raw(next) == b"oval";
                    if oval { self.take()?; }
                    let x = self.length("drill")?;
                    let y = if oval { self.length("oval drill height")? } else { x };
                    pad.drill = Some(P::new(x, y));
                    while let Some(d) = self.child()? {
                        if d.key == b"offset" { pad.geometry.offset = self.pair(d, true)?; pad.geometry.offset_set = true; }
                        else { self.unknown(d)?; }
                    }
                }
                b"layers" => { let (layers, all) = self.layer_mask_details(c)?; pad.layers = layers; pad.all_copper = all; }
                b"net" => pad.net = self.net_ref(c)?,
                b"solder_mask_margin" => pad.defaults.mask = self.margin(c)?,
                b"solder_paste_margin" => pad.defaults.paste = self.margin(c)?,
                b"solder_paste_margin_ratio" => pad.defaults.ratio = self.ratio(c)?,
                b"zone_connect" => pad.defaults.zone = self.connect(c)?,
                b"thermal_bridge_width" | b"thermal_width" => pad.defaults.spoke = Some(self.nm(c)?),
                b"thermal_gap" => pad.defaults.gap = Some(self.nm(c)?),
                b"thermal_bridge_angle" => pad.thermal_angle = Some(self.scalar(c)?),
                b"die_length" => pad.die_len = self.nm(c)?,
                b"property" => {
                    pad.prop = match self.enumeration(c)? {
                        b"pad_prop_bga" => PadProp::Bga, b"pad_prop_fiducial_glob" => PadProp::FiducialGlobal,
                        b"pad_prop_fiducial_loc" => PadProp::FiducialLocal, b"pad_prop_testpoint" => PadProp::TestPoint,
                        b"pad_prop_heatsink" => PadProp::HeatSink, b"pad_prop_castellated" => PadProp::Castellated,
                        b"pad_prop_mechanical" => PadProp::Mechanical, _ => { self.gap("pad.property", c.loc()); PadProp::None }
                    };
                }
                b"padstack" => {
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"mode" => pad.custom_stack = self.enumeration(d)? == b"custom",
                            b"layer" => {
                                let token = self.name_token("padstack layer")?;
                                let name = self.lex.text(token)?;
                                if name != "Inner" { self.layer_name(token)?; }
                                let mut geometry = PadGeometry::default();
                                while let Some(e) = self.child()? {
                                    if self.pad_geometry(e, &mut geometry, true)? { continue; }
                                    match e.key {
                                        b"thermal_bridge_width" | b"thermal_gap" | b"thermal_bridge_angle" | b"zone_connect"
                                            | b"clearance" | b"tenting" => self.skipped(e, "pad.stack_extras")?,
                                        _ => self.unknown(e)?,
                                    }
                                }
                                pad.stack.push(PadLayer { location: d.loc(), name, geometry });
                            }
                            _ => self.unknown(d)?,
                        }
                    }
                }
                b"clearance" => self.skipped(c, "pad.clearance")?, b"die_delay" => self.skipped(c, "pad.die_delay")?,
                b"pinfunction" => pad.pin.get_or_insert_with(PadPin::default).function = Some(self.string(c)?),
                b"pintype" => pad.pin.get_or_insert_with(PadPin::default).kind = Some(self.pin_type(c)?),
                b"sim_electrical_type" => {
                    let sim = match self.enumeration(c)? {
                        b"source" => SimHint::Source, b"sink" => SimHint::Sink,
                        _ => return Err(self.error(c.token, "need source or sink for sim_electrical_type")),
                    };
                    pad.pin.get_or_insert_with(PadPin::default).sim = Some(sim);
                }
                b"remove_unused_layers" | b"keep_end_layers" | b"zone_layer_connections" => self.skipped(c, "pad.layer_flashing")?,
                b"teardrops" => self.skipped(c, "teardrops")?, b"tenting" => self.skipped(c, "pad.tenting")?,
                b"backdrill" | b"tertiary_drill" | b"front_post_machining" | b"back_post_machining" => self.skipped(c, "pad.drill_extras")?,
                b"locked" | b"tstamp" | b"uuid" => self.skip(c)?,
                b"custom_property" => self.skipped(c, "custom_property")?, _ => self.unknown(c)?,
            }
        }
        self.required(pad.geometry.size, h, "size")?;
        if let Some(pin) = &pad.pin {
            for (present, key) in [(pin.function.is_some(), "pad.pin_function"), (pin.kind.is_some(), "pad.pin_type"),
                (pin.kind.as_ref().is_some_and(|t| t.no_connect), "pad.pin_no_connect"), (pin.sim.is_some(), "pad.sim_type")] {
                if present { self.report.count_read(key); }
            }
        }
        Ok(pad)
    }
    fn pin_type(&mut self, h: Head<'a>) -> Result<PinType, Refusal> {
        let raw = self.string(h)?;
        let (name, suffix) = raw.split_once('+').map_or((raw.as_str(), None), |(n, s)| (n, Some(s)));
        let known = match name {
            "input" => Some(PinKind::In), "output" => Some(PinKind::Out), "bidirectional" => Some(PinKind::Bidi),
            "tri_state" => Some(PinKind::TriState), "passive" => Some(PinKind::Passive), "power_in" => Some(PinKind::PowerIn),
            "power_out" => Some(PinKind::PowerOut), "open_collector" => Some(PinKind::OpenCollector),
            "open_emitter" => Some(PinKind::OpenEmitter), "no_connect" => Some(PinKind::NoConnect),
            "free" => Some(PinKind::Free), "unspecified" => Some(PinKind::Unspecified), _ => None,
        };
        if known.is_none() || suffix.is_some_and(|s| s != "no_connect") {
            self.report.gap_value("pad.pin_type_unknown", GapKind::Dropped, h.loc(), raw.clone());
        }
        let no_connect = suffix == Some("no_connect");
        Ok(PinType { kind: known.map_or_else(|| PadPinKind::Unknown(raw), PadPinKind::Known), no_connect })
    }
    fn pad_geometry(&mut self, h: Head<'a>, g: &mut PadGeometry, layer: bool) -> Result<bool, Refusal> {
        match h.key {
            b"shape" if layer => {
                let t = self.take()?;
                g.form = if t.kind == Kind::Symbol { PadForm::read(self.lex.raw(t)) } else { None };
                if g.form.is_none() { return Err(self.error(t, "need a pad shape")); }
                self.rest()?;
            }
            b"size" => {
                let mut size = self.pair(h, false)?;
                if size.x <= 0 || size.y <= 0 {
                    self.approx("pad.zero_size", h.loc());
                    if size.x <= 0 { size.x = 1_000; } if size.y <= 0 { size.y = 1_000; }
                }
                g.size = Some(size);
            }
            b"rect_delta" => g.delta = self.pair(h, false)?,
            b"offset" => { g.offset = self.pair(h, true)?; g.offset_set = true; }
            b"roundrect_rratio" => g.round = (self.scalar(h)? * 1_000_000.0).round() as u32,
            b"chamfer_ratio" => g.chamfer = (self.scalar(h)? * 1_000_000.0).round() as u32,
            b"chamfer" => {
                while let Some(c) = self.child()? {
                    let bit = match c.key { b"top_left" => 1, b"top_right" => 2, b"bottom_left" => 4,
                        b"bottom_right" => 8, _ => { self.unknown(c)?; continue; } };
                    g.corners |= bit; self.skip(c)?;
                }
            }
            b"options" => {
                while let Some(c) = self.child()? {
                    match c.key {
                        b"anchor" => g.round_anchor = self.enumeration(c)? == b"circle",
                        b"clearance" => {
                            if self.enumeration(c)? == b"convexhull" { self.gap("pad.custom_clearance_hull", c.loc()); }
                        }
                        _ => self.unknown(c)?,
                    }
                }
            }
            b"primitives" => {
                while let Some(c) = self.child()? {
                    match c.key {
                        b"gr_bbox" | b"gr_vector" => self.skipped(c, "pad.custom_proxies")?,
                        b"gr_line" | b"gr_arc" | b"gr_circle" | b"gr_rect" | b"gr_poly" | b"gr_curve" => {
                            let graphic = self.graphic(c, true)?;
                            for gfx in graphic.graphics(LayerId::F_CU) {
                                match (&gfx, graphic.kind) {
                                    (Gfx::Stroke { w, .. }, GraphicKind::Rect) => {
                                        if let Some(Shape::Rect { c, half, .. }) = &graphic.solid {
                                            let rect = rect_path(
                                                safe_point(c.x as f64 - half.x as f64, c.y as f64 - half.y as f64),
                                                safe_point(c.x as f64 + half.x as f64, c.y as f64 + half.y as f64), 0);
                                            g.primitives.extend(rect.steps().map(|(a, s)| Shape::Segment { a, b: s.end(), w: *w }));
                                        }
                                    }
                                    (Gfx::Stroke { path, w }, GraphicKind::Poly) => {
                                        let ring = path_ring(path);
                                        for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)).take(ring.len()) {
                                            g.primitives.push(Shape::Segment { a: *a, b: *b, w: *w });
                                        }
                                    }
                                    _ => g.primitives.extend(gfx.shapes()),
                                }
                            }
                        }
                        _ => self.unknown(c)?,
                    }
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn finish_pad(&mut self, p: &PadRead, defaults: &PadDefaults, theta: Angle, bottom: bool) -> Pad {
        let rot = if bottom { theta.sub(p.rot) } else { p.rot.sub(theta) };
        let position = if bottom { flip_y(p.at) } else { p.at };
        let offset = if bottom { flip_y(p.geometry.offset) } else { p.geometry.offset };
        let at = add(position, turn_about(offset, P::ZERO, rot));
        let count = self.layers.copper_count.min(128) as u8;
        let layers = if bottom { p.layers.flipped(count) } else { p.layers };
        let mut copper = layers.copper_only();
        let tech = layers.intersect(LayerSet::of(&[LayerId::F_MASK, LayerId::B_MASK, LayerId::F_PASTE, LayerId::B_PASTE]));
        if !layers.minus(copper).minus(tech).is_empty() { self.gap("pad.other_layers", p.location); }
        if p.kind == PadKind::Thru || p.all_copper { copper = LayerSet::ALL_COPPER; }
        let mut drill = match p.kind { PadKind::Smd | PadKind::Edge => None,
            _ => p.drill.map(|size| Drill { size, offset: P::new(-offset.x, -offset.y) }) };
        if p.kind == PadKind::Thru && drill.is_none() {
            self.approx("pad.no_drill", p.location);
            drill = Some(Drill { size: P::new(1, 1), offset: P::new(-offset.x, -offset.y) });
        }
        if p.kind == PadKind::Npth {
            let size = p.geometry.size.unwrap_or_default(); let hole = p.drill.unwrap_or_default();
            if !copper.is_empty() && (size.x > hole.x || size.y > hole.y) { self.gap("pad.npth_copper", p.location); }
            copper = LayerSet::EMPTY;
        }
        let own = p.geometry.shape(bottom, &p.geometry);
        let mut front = own.clone();
        let mut back = None;
        let mut inner = None;
        for entry in &p.stack {
            let geometry = &entry.geometry;
            if geometry.offset_set && geometry.offset != p.geometry.offset { self.gap("pad.stack_extras", entry.location); }
            let shape = geometry.shape(bottom, &p.geometry);
            let target = if entry.name == "Inner" { None } else { self.layers.names.get(&entry.name).copied() };
            match target {
                Some(LayerId::F_CU) => front = shape,
                Some(LayerId::B_CU) => back = Some(shape),
                Some(l) if l.inner_index().is_some() => {
                    if let Some(previous) = &inner {
                        if p.custom_stack && previous != &shape { self.approx("pad.custom_layers", entry.location); }
                    } else { inner = Some(shape); }
                }
                None if entry.name == "Inner" => inner = Some(shape),
                _ => { self.gap("pad.stack_extras", entry.location); }
            }
        }
        if bottom && !p.stack.is_empty() {
            let old_front = front;
            front = back.take().unwrap_or_else(|| old_front.clone());
            if inner.is_none() && front != old_front { inner = Some(old_front.clone()); }
            back = Some(old_front);
        }
        let gap = p.defaults.gap.or(defaults.gap); let spoke = p.defaults.spoke.or(defaults.spoke);
        let thermal = if gap.is_some() || spoke.is_some() {
            if gap.is_none() || spoke.is_none() { self.approx("pad.thermal_partial", p.location); }
            let custom_round = p.geometry.form == Some(PadForm::Custom) && p.geometry.round_anchor;
            let default_angle = if p.geometry.form == Some(PadForm::Circle)
                || (custom_round && self.report.version > 20211014) { 45.0 } else { 90.0 };
            Some(ThermalRule { gap: gap.unwrap_or(500_000), spoke: spoke.unwrap_or(500_000),
                angle: angle(p.thermal_angle.unwrap_or(default_angle).rem_euclid(90.0)) })
        } else { None };
        Pad { no: p.no.clone(), kind: p.kind, stack: PadStack { copper, tech, front, inner, back }, at, rot, drill,
            mask_margin: p.defaults.mask.or(defaults.mask),
            paste: PasteRule { margin: p.defaults.paste.or(defaults.paste), ratio_ppm: p.defaults.ratio.or(defaults.ratio) },
            thermal, zone_connect: p.defaults.zone.or(defaults.zone), die_len: p.die_len, prop: p.prop }
    }
}

impl<'a> Parser<'a> {
    fn field_name(&mut self, name: &str) -> Arc<str> {
        if let Some(name) = self.field_names.get(name) { return name.clone(); }
        let name: Arc<str> = name.into();
        self.field_names.insert(name.clone(), name.clone()); name
    }
    // Hidden user fields need no numeric drawing conversion, but extensions still get reported.
    fn hidden_field_rest(&mut self, parent: &[u8]) -> Result<(), Refusal> {
        while let Some(c) = self.child()? {
            let container = matches!((parent, c.key), (b"property", b"effects") | (b"effects", b"font" | b"justify"));
            let known = match parent {
                b"property" => matches!(c.key, b"at" | b"layer" | b"hide" | b"locked" | b"unlocked" | b"tstamp" | b"uuid" | b"render_cache"),
                b"effects" => matches!(c.key, b"hide" | b"line_spacing"),
                b"font" => matches!(c.key, b"size" | b"thickness" | b"face" | b"bold" | b"italic" | b"line_spacing"),
                b"justify" => matches!(c.key, b"left" | b"right" | b"top" | b"bottom" | b"mirror"),
                _ => false,
            };
            if container && c.list { self.hidden_field_rest(c.key)?; }
            else if known { self.skip(c)?; }
            else if c.key == b"custom_property" && parent == b"property" { self.skipped(c, "custom_property")?; }
            else { self.unknown(c)?; }
        }
        Ok(())
    }
    fn units(&mut self, h: Head<'a>, data: &mut PlacementData, locations: &mut Vec<Location>) -> Result<(), Refusal> {
        self.require_list(h)?;
        while let Some(c) = self.child()? {
            if c.key != b"unit" || !c.list { self.unknown(c)?; continue; }
            let mut name = String::new();
            let mut pins = Vec::new();
            while let Some(d) = self.child()? {
                match d.key {
                    b"name" => name = self.string(d)?,
                    b"pins" => {
                        self.require_list(d)?;
                        while self.peek()?.kind != Kind::Close {
                            let t = self.name_token("unit pin")?;
                            pins.push(self.lex.text(t)?); locations.push(t.location());
                        }
                        self.take()?; self.path.pop();
                    }
                    _ => self.unknown(d)?,
                }
            }
            data.units.push((name, pins));
        }
        Ok(())
    }
    fn component_classes(&mut self, h: Head<'a>, data: &mut PlacementData, seen: &mut HashSet<String>) -> Result<(), Refusal> {
        self.require_list(h)?;
        while let Some(c) = self.child()? {
            if c.key != b"class" || !c.list { return Err(self.error(c.token, "need a class list")); }
            let name = self.name("component class")?;
            let end = self.take()?;
            if end.kind != Kind::Close { return Err(self.error(end, "need exactly one component class name")); }
            self.path.pop();
            if seen.insert(name.clone()) { data.classes.push(name); }
        }
        Ok(())
    }
    fn footprint(&mut self, h: Head<'a>) -> Result<Option<FileFootprint>, Refusal> {
        let name = self.name("footprint name")?;
        let mut at = P::ZERO;
        let mut theta = Angle::ZERO;
        let mut bottom = false;
        let mut locked = false;
        let mut path = String::new();
        let mut refdes = String::new();
        let mut data = PlacementData::default();
        let mut field_indices = HashMap::new();
        for name in ["Reference", "Value", "Datasheet", "Description"] {
            let name = self.field_name(name);
            field_indices.insert(name.clone(), data.fields.len());
            data.fields.push(Field { name, value: String::new(), drawn: false });
        }
        let mut field_texts: [Option<usize>; 4] = [None; 4];
        let mut unit_locations = Vec::new();
        let mut have_units = false;
        let mut classes = HashSet::new();
        let mut defaults = PadDefaults::default();
        let mut attrs = FpAttrs { mount: if self.report.version < 20200826 { FpMount::Thru } else { FpMount::Other },
            ..FpAttrs::default() };
        let mut pads = Vec::new();
        let mut graphics = Vec::new();
        let mut texts = Vec::new();
        let mut keepouts = Vec::new();
        while let Some(c) = self.child()? {
            match c.key {
                b"layer" => {
                    let Some(layer) = self.single_layer(c)? else { self.skip(h)?; return Ok(None); };
                    bottom = layer == LayerId::B_CU;
                }
                b"at" => { let p = self.position(c)?; at = p.0; theta = p.1; }
                b"transform" if self.report.version >= 20260616 => {
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"translate" => at = self.pair(d, true)?, b"rotate" => theta = angle(self.scalar(d)?),
                            b"scale" => {
                                let x = self.number("scale x")?; let y = self.number("scale y")?;
                                if x != 1.0 || y != 1.0 { self.gap("footprint.scale", d.loc()); }
                                self.rest()?;
                            }
                            _ => self.unknown(d)?,
                        }
                    }
                }
                b"locked" => locked = self.flag(c)?, b"path" => path = self.string(c)?,
                b"placed" | b"tedit" | b"tstamp" | b"uuid" | b"autoplace_cost90" | b"autoplace_cost180"
                    | b"version" | b"generator" | b"generator_version" | b"embedded_fonts" => self.skip(c)?,
                b"attr" => {
                    attrs.mount = FpMount::Other;
                    while let Some(d) = self.child()? {
                        match d.key {
                            b"smd" => { attrs.mount = FpMount::Smd; self.skip(d)?; }
                            b"through_hole" => { attrs.mount = FpMount::Thru; self.skip(d)?; }
                            b"board_only" => attrs.board_only = self.flag(d)?,
                            b"exclude_from_pos_files" => attrs.exclude_pos = self.flag(d)?,
                            b"exclude_from_bom" => attrs.exclude_bom = self.flag(d)?,
                            b"virtual" if self.report.version < 20200826 => {
                                attrs.exclude_bom = true; attrs.exclude_pos = true; self.skip(d)?;
                            }
                            b"exclude_from_sim" | b"dnp" | b"allow_missing_courtyard" | b"allow_soldermask_bridges" => self.skipped(d, "footprint.attr_extra")?,
                            _ => self.unknown(d)?,
                        }
                    }
                }
                b"solder_mask_margin" => defaults.mask = self.margin(c)?,
                b"solder_paste_margin" => defaults.paste = self.margin(c)?,
                b"solder_paste_margin_ratio" | b"solder_paste_ratio" => defaults.ratio = self.ratio(c)?,
                b"zone_connect" => defaults.zone = self.connect(c)?,
                b"thermal_gap" => defaults.gap = Some(self.nm(c)?), b"thermal_width" => defaults.spoke = Some(self.nm(c)?),
                b"pad" => pads.push(self.pad(c)?),
                b"fp_text" | b"property" => {
                    self.require_list(c)?;
                    let property = c.key == b"property";
                    let legacy = property && self.report.version < 20230620;
                    let label = self.name("text kind or property name")?;
                    let value = self.name("text value")?;
                    if property && label == "ki_fp_filters" {
                        data.fp_filters = value.split_whitespace().map(str::to_owned).collect(); self.rest()?; continue;
                    }
                    if property && label == "Footprint" && !value.is_empty() && value != name {
                        self.report.gap_value("footprint.field_mismatch", GapKind::Dropped, c.loc(), format!("{value} != {name}"));
                    }
                    if legacy {
                        match label.as_str() {
                            "ki_keywords" | "ki_locked" | "Footprint" => { self.rest()?; continue; }
                            "Sheetfile" | "Sheet file" => { data.sheet_file = value; self.rest()?; continue; }
                            "Sheetname" | "Sheet name" => { data.sheet_name = value; self.rest()?; continue; }
                            _ => {}
                        }
                    }
                    let kind = match label.as_str() { "reference" if c.key == b"fp_text" => FpTextKind::Reference,
                        "value" if c.key == b"fp_text" => FpTextKind::Value,
                        "Reference" if property => FpTextKind::Reference,
                        "Value" if property => FpTextKind::Value, _ => FpTextKind::User };
                    let field = if property {
                        if label == "Footprint" { None }
                        else { Some(if legacy && label == "ki_description" { "Description" } else { label.as_str() }) }
                    } else { match kind { FpTextKind::Reference => Some("Reference"), FpTextKind::Value => Some("Value"), _ => None } };
                    let mut hidden = legacy;
                    if property && !legacy && kind == FpTextKind::User {
                        let (body, _) = self.capture_body(false)?;
                        hidden = self.hidden_field(&body) || !self.body_children(&body).iter().any(|(h, _, _)| h.key == b"layer");
                        self.lex.replay_tokens(body);
                    }
                    if kind == FpTextKind::Reference { refdes = value.clone(); }
                    let text = if legacy { self.rest()?; None }
                    else if hidden { self.hidden_field_rest(b"property")?; None }
                    else {
                        let text = self.text(c, if matches!(kind, FpTextKind::Reference | FpTextKind::Value) { String::new() } else { value.clone() })?;
                        hidden = text.hidden;
                        Some((kind, text))
                    };
                    let mut standard = None;
                    if let Some(field) = field {
                        self.report.count_read("footprint.fields");
                        if hidden { self.report.count_read("footprint.fields.hidden"); }
                        let name = self.field_name(field);
                        let index = *field_indices.entry(name.clone()).or_insert(data.fields.len());
                        let field = Field { name, value, drawn: !hidden };
                        if index == data.fields.len() { data.fields.push(field); } else { data.fields[index] = field; }
                        if index < 4 { standard = Some(index); }
                    }
                    if let Some(index) = standard {
                        if let Some(slot) = field_texts[index] { texts[slot] = text; }
                        else if text.is_some() { field_texts[index] = Some(texts.len()); texts.push(text); }
                    } else if text.is_some() { texts.push(text); }
                }
                key if graphic_kind(key).is_some() && key.starts_with(b"fp_") => graphics.push((c.loc(), self.graphic(c, false)?)),
                b"zone" => { if let ZoneRead::Keepout(k) = self.zone(c, true)? { keepouts.push(k); } }
                b"model" => {
                    if matches!(self.peek()?.kind, Kind::Symbol | Kind::String | Kind::Number) {
                        let model = self.model(c)?;
                        if model.hidden { self.gap("model.hidden", model.at); }
                        else if data.models.iter().any(|m| !m.hidden) { self.report.gap_value("model.extra", GapKind::Skipped, model.at, model.path.clone()); }
                        data.models.push(model);
                    } else {
                        let mut extruded = false;
                        while let Some(d) = self.child()? {
                            if d.key == b"type" && d.list { extruded |= self.name("model type")? == "extruded"; }
                            self.skip(d)?;
                        }
                        self.gap(if extruded { "model.extruded" } else { "model" }, c.loc());
                    }
                }
                b"tags" => {
                    if self.report.version <= 20220815 {
                        let tags = self.name("tags")?;
                        if tags.starts_with("net tie") { self.gap("footprint.net_tie", c.loc()); }
                    }
                    self.skipped(c, "footprint.library_text")?;
                }
                b"descr" => self.skipped(c, "footprint.library_text")?,
                b"sheetname" => data.sheet_name = self.string(c)?, b"sheetfile" => data.sheet_file = self.string(c)?,
                b"units" => { have_units = true; self.units(c, &mut data, &mut unit_locations)?; }
                b"component_classes" => self.component_classes(c, &mut data, &mut classes)?,
                b"variant" => {
                    self.report.gap_value("footprint.schematic_links", GapKind::Skipped, c.loc(), "variant".into()); self.skip(c)?;
                }
                b"clearance" => self.skipped(c, "footprint.clearance")?,
                b"private_layers" => self.skipped(c, "footprint.private_layers")?,
                b"net_tie_pad_groups" => self.skipped(c, "footprint.net_tie")?,
                b"duplicate_pad_numbers_are_jumpers" | b"jumper_pad_groups" => self.skipped(c, "footprint.jumpers")?,
                b"height" => self.skipped(c, "footprint.height")?, b"stackup" => self.skipped(c, "footprint.stackup")?,
                _ => if let Some(feature) = footprint_skip(c.key) { self.skipped(c, feature)?; } else { self.unknown(c)?; },
            }
        }
        let rot = if bottom { theta.add(Angle::HALF) } else { theta };
        let side = if bottom { Side::Bottom } else { Side::Top };
        self.report.count_read(if bottom { "footprints.bottom" } else { "footprints.top" });
        if !data.sheet_name.is_empty() || !data.sheet_file.is_empty() { self.report.count_read("footprint.links.sheet"); }
        if have_units { self.report.count_read("footprint.links.units"); }
        if !data.classes.is_empty() { self.report.count_read("footprint.links.classes"); }
        let mut out = FileFootprint { location: h.loc(), footprint: Footprint { name, attrs, ..Footprint::default() },
            at, rot, side, locked, path, refdes, data, pad_nets: Vec::new(), pad_locations: Vec::new(),
            graphic_locations: Vec::new(), text_locations: Vec::new(), keepout_locations: Vec::new(), edges: Vec::new() };
        let mut pad_numbers = HashSet::new();
        for pad in pads {
            out.footprint.pads.push(self.finish_pad(&pad, &defaults, theta, bottom));
            out.pad_nets.push(pad.net); out.pad_locations.push(pad.location);
            out.data.pins.push(pad.pin); pad_numbers.insert(pad.no);
        }
        for (pin, at) in out.data.units.iter().flat_map(|(_, pins)| pins).zip(unit_locations) {
            if !pad_numbers.contains(pin) {
                self.report.gap_value("footprint.unit_pin_unknown", GapKind::Dropped, at, pin.clone());
            }
        }
        let count = self.layers.copper_count.min(128) as u8;
        let mut courtyard = Vec::new();
        let mut filled_courtyard = Vec::new();
        for (loc, mut shape) in graphics {
            if bottom { shape.flip_y(); shape.layers = shape.layers.flipped(count); }
            for layer in shape.layers.iter() {
                if layer == LayerId::EDGE { out.edges.push(shape.edge(loc)); }
                if layer == LayerId::F_CRTYD {
                    let mut filled = false;
                    for graphic in shape.graphics(layer) {
                        if let Gfx::Fill(s) = graphic { filled_courtyard.extend(solid_rings(&s)); filled = true; }
                    }
                    if !filled { courtyard.push(shape.edge(loc)); }
                }
                if layer == LayerId::B_CRTYD { self.gap("footprint.courtyard_back", loc); }
                if layer.is_copper() { self.gap("graphic.on_copper", loc); }
                for gfx in shape.graphics(layer) {
                    out.footprint.graphics.push((layer, gfx)); out.graphic_locations.push(loc);
                }
            }
        }
        let (mut rings, open) = courtyard_rings(courtyard);
        rings.extend(filled_courtyard);
        for loc in open { self.gap("footprint.courtyard_open", loc); }
        out.footprint.courtyard = PolySet::from_rings_even_odd(&rings);
        for (kind, text) in texts.into_iter().flatten() {
            let Some(text_value) = text.value else { continue; };
            if text_value.mirrored != bottom { self.gap("text.mirror_side", text_value.location); }
            if text.unlocked { self.gap("text.keep_upright", text_value.location); }
            let absolute = if self.report.version >= 20260616 { theta.add(text_value.rot) } else { text_value.rot };
            let relative = if bottom { theta.add(Angle::HALF).sub(absolute) } else { absolute.sub(theta) };
            out.text_locations.push(text_value.location);
            out.footprint.texts.push(FpText { kind, text: text_value.text,
                layer: if bottom { text_value.layer.flipped(count) } else { text_value.layer },
                at: if bottom { flip_y(text_value.at) } else { text_value.at }, rot: relative,
                size: text_value.size, thick: text_value.thick, visible: !text.hidden });
        }
        for keepout in keepouts {
            let area = if self.report.version < 20260616 {
                map_poly(&keepout.area, |p| {
                    let (s, c) = rot.neg().sin_cos();
                    let (x, y) = (p.x as f64 - at.x as f64, p.y as f64 - at.y as f64);
                    safe_point((if bottom { -1.0 } else { 1.0 }) * (c * x - s * y), s * x + c * y)
                })
            } else if bottom { map_poly(&keepout.area, flip_y) } else { keepout.area };
            out.keepout_locations.push(keepout.location);
            out.footprint.keepouts.push(Keepout { area,
                layers: if bottom { keepout.layers.flipped(count) } else { keepout.layers },
                forbid: keepout.forbid, reason: keepout.reason });
        }
        Ok(Some(out))
    }
}

// Endpoint bins avoid a scan over all drawings each time a courtyard chain is extended.
fn courtyard_rings(pieces: Vec<EdgePiece>) -> (Vec<Vec<P>>, Vec<Location>) {
    let mut rings = Vec::new();
    let mut open = Vec::new();
    let mut bins: HashMap<(i64, i64), HashSet<(usize, bool)>> = HashMap::new();
    let cell = |p: P| ((p.x as i64).div_euclid(10_000), (p.y as i64).div_euclid(10_000));
    let mut used = vec![false; pieces.len()];
    for (i, piece) in pieces.iter().enumerate() {
        if piece.closed { rings.push(path_ring(&piece.path)); used[i] = true; }
        else {
            bins.entry(cell(piece.path.start)).or_default().insert((i, false));
            bins.entry(cell(piece.path.end())).or_default().insert((i, true));
        }
    }
    let remove = |i: usize, bins: &mut HashMap<(i64, i64), HashSet<(usize, bool)>>| {
        if let Some(bin) = bins.get_mut(&cell(pieces[i].path.start)) { bin.remove(&(i, false)); }
        if let Some(bin) = bins.get_mut(&cell(pieces[i].path.end())) { bin.remove(&(i, true)); }
    };
    for (i, piece) in pieces.iter().enumerate() {
        if used[i] { continue; }
        used[i] = true; remove(i, &mut bins);
        let mut chain = piece.path.clone();
        // Extend at both ends, since the first source piece can be in the middle of an open chain.
        for pass in 0..2 {
            loop {
                if chain.end().dist2(chain.start) <= 100_000_000 { break; }
                let end = chain.end(); let (x, y) = cell(end);
                let mut candidate = None;
                for dx in -1..=1 { for dy in -1..=1 {
                    if let Some(bin) = bins.get(&(x + dx, y + dy)) {
                        for &(j, reverse) in bin {
                            let point = if reverse { pieces[j].path.end() } else { pieces[j].path.start };
                            if point.dist2(end) <= 100_000_000 {
                                let key = (j, reverse);
                                if candidate.is_none_or(|old| key < old) { candidate = Some(key); }
                            }
                        }
                    }
                } }
                let Some((j, reverse)) = candidate else { break; };
                used[j] = true; remove(j, &mut bins);
                let next = if reverse { reverse_path(&pieces[j].path) } else { pieces[j].path.clone() };
                chain.segs.extend(next.segs);
            }
            if chain.end().dist2(chain.start) <= 100_000_000 { break; }
            if pass == 0 { chain = reverse_path(&chain); }
        }
        if chain.end().dist2(chain.start) <= 100_000_000 {
            if let Some(last) = chain.segs.last_mut() {
                match last { Seg::Line(p) => *p = chain.start, Seg::Arc { end, .. } => *end = chain.start }
            }
            rings.push(path_ring(&chain));
        } else { open.push(piece.location); }
    }
    (rings, open)
}
fn reverse_path(path: &Path) -> Path {
    let mut steps: Vec<_> = path.steps().collect(); steps.reverse();
    Path { start: path.end(), segs: steps.into_iter().map(|(a, seg)| match seg {
        Seg::Line(_) => Seg::Line(a), Seg::Arc { mid, .. } => Seg::Arc { mid, end: a },
    }).collect() }
}

fn solid_rings(shape: &Shape) -> Vec<Vec<P>> {
    match shape {
        Shape::Circle { c, r } => vec![path_ring(&circle_path(*c, *r))],
        Shape::Rect { c, half, corner, .. } => vec![path_ring(&rect_path(
            safe_point(c.x as f64 - half.x as f64, c.y as f64 - half.y as f64),
            safe_point(c.x as f64 + half.x as f64, c.y as f64 + half.y as f64), *corner))],
        Shape::Poly(p) => p.rings().map(|r| r.to_vec()).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(fp: &FileFootprint, point: P) -> P {
        let p = if fp.side == Side::Bottom { P::new(-point.x, point.y) } else { point };
        p.rotated(fp.rot).add(fp.at)
    }
    fn gap(file: &PcbFile, key: &str) -> u32 { file.report.gaps.get(key).map_or(0, |g| g.count) }

    #[test]
    fn placement_fields_links_and_pins_preserve_sharing() {
        let mut source = String::from("(kicad_pcb (version 20241229) (layers (0 F.Cu signal) (31 B.Cu signal))");
        for i in 1..=3 {
            let value = if i == 3 { "different" } else { "IC" };
            let pin_type = ["input", "output", "future+no_connect"][i - 1];
            let other_type = ["passive+no_connect", "bidirectional", "power_in+other"][i - 1];
            let unit_pins = ["1 99", "2 1", "1 2"][i - 1];
            let hidden = ["hide", "(hide yes)", "(effects hide)"][i - 1];
            let header = if i == 1 { "wrong" } else { "lib:IC" };
            source.push_str(&format!(r#"
 (footprint lib:IC (layer F.Cu) (at {i} 0) (path {i})
  (property Reference U{i} (layer F.SilkS))
  (property Value obsolete (at 9 0) (layer F.SilkS))
  (property MPN stale (layer F.SilkS) hide)
  (property Layerless {i} (extension unparsed))
  (property Drawn "${{REFERENCE}}\nlabel" (layer F.SilkS))
  (property Value {value} (at 0 1) (layer F.SilkS))
  (property MPN M{i} (layer F.SilkS) {hidden})
  (property Datasheet obsolete (layer F.SilkS))
  (property Datasheet "sheet-{i}")
  (property Description "note-{i}" (layer F.SilkS) (effects hide (future_style unparsed)))
  (property Footprint {header}) (property Footprint "") (property ki_fp_filters "IC_*  Q?*")
  (sheetname "Sheet {i}") (sheetfile "sheet-{i}.kicad_sch")
  (units (future_unit unparsed) (unit (name "Unit {i}") (pins {unit_pins}) (future_pin unparsed)))
  (component_classes (class Common) (class C{i}) (class Common))
  (pad 1 smd rect (size 1 1) (layers F.Cu) (pinfunction "P{i}") (pintype {pin_type}) (sim_electrical_type source))
  (pad 2 smd rect (at 2 0) (size 1 1) (layers F.Cu) (pinfunction {i}) (pintype {other_type}) (sim_electrical_type sink))
  {})"#, if i == 1 { "(variant (name Alternate) (field (name MPN) (value ignored)))" } else { "" }));
        }
        source.push(')');
        let read = crate::read_kicad_pcb(source.as_bytes(), "placements").expect("placement board reads");
        assert_eq!(read.placements.len(), read.board.placed.len(), "placement data stays parallel to placed items");
        assert!(Arc::ptr_eq(&read.board.placed[0].footprint, &read.board.placed[1].footprint), "per-placement metadata and references do not split shared footprints");
        // Values belong to placements, so different values share the same footprint drawing.
        assert!(Arc::ptr_eq(&read.board.placed[0].footprint, &read.board.placed[2].footprint), "different placement values do not split shared footprints");
        for (index, data) in read.placements.iter().enumerate() {
            let i = index + 1;
            let placed = &read.board.placed[index];
            let value = if i == 3 { "different" } else { "IC" };
            assert_eq!(placed.value.as_deref(), Some(value), "each placement keeps its own Value for placement {i}");
            let fields: Vec<_> = data.fields.iter().map(|f| (f.name.as_ref(), f.value.clone(), f.drawn)).collect();
            assert_eq!(fields, vec![("Reference", format!("U{i}"), true), ("Value", value.into(), true),
                ("Datasheet", format!("sheet-{i}"), false), ("Description", format!("note-{i}"), false),
                ("MPN", format!("M{i}"), false), ("Layerless", i.to_string(), false),
                ("Drawn", "${REFERENCE}\nlabel".into(), true)], "F1/F2/F5 field order, last values, escapes and visibility for placement {i}");
            for (field, first) in data.fields.iter().zip(&read.placements[0].fields) {
                assert!(Arc::ptr_eq(&field.name, &first.name), "F31 field name {} is interned across placements", field.name);
            }
            assert_eq!((&placed.refdes, &placed.path), (&format!("U{i}"), &i.to_string()), "F4/F10 reference and numeric path for placement {i}");
            assert_eq!(data.fp_filters, ["IC_*", "Q?*"], "F6 ordered filter patterns for placement {i}");
            assert_eq!((&data.sheet_name, &data.sheet_file), (&format!("Sheet {i}"), &format!("sheet-{i}.kicad_sch")), "F11 sheet links for placement {i}");
            let pins = [vec!["1", "99"], vec!["2", "1"], vec!["1", "2"]][index].iter().map(|s| (*s).into()).collect();
            assert_eq!(data.units, vec![(format!("Unit {i}"), pins)], "F12 unit and pad order including unknown pad for placement {i}");
            assert_eq!(data.classes, ["Common".into(), format!("C{i}")], "F13 stable deduplicated classes for placement {i}");
            let kinds = [(PadPinKind::Known(PinKind::In), PadPinKind::Known(PinKind::Passive)),
                (PadPinKind::Known(PinKind::Out), PadPinKind::Known(PinKind::Bidi)),
                (PadPinKind::Unknown("future+no_connect".into()), PadPinKind::Known(PinKind::PowerIn))];
            assert_eq!(data.pins, vec![Some(PadPin { function: Some(format!("P{i}")),
                kind: Some(PinType { kind: kinds[index].0.clone(), no_connect: i == 3 }), sim: Some(SimHint::Source) }),
                Some(PadPin { function: Some(i.to_string()), kind: Some(PinType { kind: kinds[index].1.clone(), no_connect: i == 1 }),
                    sim: Some(SimHint::Sink) })], "F20-F23 pin values, suffix flags and simulation hints for placement {i}");
            assert_eq!(data.pins.len(), placed.pad_nets.len(), "F23 pins parallel pad nets for placement {i}");
            let texts: Vec<_> = placed.footprint.texts.iter().map(|t| (t.kind, t.text.as_str())).collect();
            assert_eq!(texts, [(FpTextKind::Reference, ""), (FpTextKind::Value, ""), (FpTextKind::User, "${REFERENCE}\nlabel")],
                "F5 duplicate standard texts replace in place and layerless replacement removes drawing");
            assert_eq!(placed.footprint.texts[1].at, P::mm(0.0, -1.0), "F5 duplicate Value replaces its drawing coordinates");
        }
        for (key, count) in [("footprint.fields", 30), ("footprint.fields.hidden", 15),
            ("footprint.links.sheet", 3), ("footprint.links.units", 3), ("footprint.links.classes", 3),
            ("pad.pin_function", 6), ("pad.pin_type", 6), ("pad.pin_no_connect", 2), ("pad.sim_type", 6)] {
            assert_eq!(read.report.read.get(key), Some(&count), "F7/F14 read count for {key}");
        }
        for (key, kind, values) in [
            ("footprint.field_mismatch", GapKind::Dropped, vec![("wrong != lib:IC".into(), 1)]),
            ("footprint.unit_pin_unknown", GapKind::Dropped, vec![("99".into(), 1)]),
            ("pad.pin_type_unknown", GapKind::Dropped, vec![("future+no_connect".into(), 1), ("power_in+other".into(), 1)]),
            ("footprint.schematic_links", GapKind::Skipped, vec![("variant".into(), 1)]),
        ] {
            let gap = &read.report.gaps[key];
            assert_eq!((gap.kind, &gap.values, gap.count), (kind, &values, values.len() as u32), "F4/F8/F12/F21 gap values and count for {key}");
        }
        for path in ["property>extension", "property>effects>future_style", "units>future_unit", "units>unit>future_pin"] {
            assert_eq!(read.report.unknown[&format!("kicad_pcb>footprint>{path}")].count, 3, "F30 unknown field/unit child {path} reported");
        }
        for key in ["footprint.fields", "pad.pin_info"] {
            assert!(!read.report.gaps.contains_key(key), "F30 retained {key} has no skipped gap");
        }
    }

    #[test]
    fn legacy_properties_and_five_x_text_fields() {
        for version in [20221018, 20171130] {
            let properties = if version == 20221018 { r#"
  (property ki_keywords ignored) (property ki_locked yes)
  (property ki_description "description")
  (property Sheetfile old) (property "Sheet file" "sheet.kicad_sch")
  (property Sheetname old) (property "Sheet name" "Sheet")
  (property MPN old) (property MPN "part") (property mpn "case-sensitive")
  (property ki_fp_filters "R_*  R_?") (property Footprint lib:R)"# } else { "" };
            let source = format!(r#"(kicad_pcb (version {version}) (layers (0 F.Cu signal) (31 B.Cu signal))
 (module lib:R (layer F.Cu) (path 12345678) {properties}
  (fp_text reference R1 (layer F.SilkS)) (fp_text value 10k (layer F.SilkS))
  (fp_text user drawing (layer F.SilkS))
  (units (unit (name A) (pins "1"))) (component_classes (class Passive) (class Passive))
  (pad 1 smd rect (size 1 1) (layers F.Cu))))"#);
            let read = crate::read_kicad_pcb(source.as_bytes(), "legacy fields").expect("legacy field board reads");
            let data = &read.placements[0];
            let placed = &read.board.placed[0];
            assert_eq!(data.fields.iter().take(4).map(|f| f.name.as_ref()).collect::<Vec<_>>(),
                ["Reference", "Value", "Datasheet", "Description"], "F1 standard fields always first at {version}");
            assert_eq!(data.fields.iter().take(3).map(|f| (f.value.as_str(), f.drawn)).collect::<Vec<_>>(),
                [("R1", true), ("10k", true), ("", false)], "F3/F7 drawn legacy fields and empty default at {version}");
            assert_eq!((placed.refdes.as_str(), placed.path.as_str()), ("R1", "12345678"), "F4/F10 legacy reference and numeric path at {version}");
            assert_eq!(placed.value.as_deref(), Some("10k"), "legacy Value is kept on the placement at {version}");
            assert_eq!(placed.footprint.texts.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
                ["", "", "drawing"], "legacy user drawing is retained and Reference and Value texts are empty at {version}");
            assert_eq!(data.pins, [None], "F23 pad without pin metadata has an empty parallel entry at {version}");
            assert_eq!(data.units, [("A".into(), vec!["1".into()])], "F12 units accepted in any version at {version}");
            assert_eq!(data.classes, ["Passive"], "F13 classes accepted and deduplicated in any version at {version}");
            if version == 20221018 {
                assert_eq!(data.fields.iter().skip(3).map(|f| (f.name.as_ref(), f.value.as_str(), f.drawn)).collect::<Vec<_>>(),
                    [("Description", "description", false), ("MPN", "part", false), ("mpn", "case-sensitive", false)], "F3/F5 legacy aliases, hidden fields and exact names");
                assert_eq!((&*data.sheet_name, &*data.sheet_file), ("Sheet", "sheet.kicad_sch"), "F11 legacy sheet aliases retain last values");
                assert_eq!(data.fp_filters, ["R_*", "R_?"], "F6 legacy filter patterns");
                assert_eq!(read.report.read.get("footprint.fields"), Some(&6), "F7 legacy field occurrences exclude special properties");
                assert_eq!(read.report.read.get("footprint.fields.hidden"), Some(&4), "F7 hidden legacy occurrences counted");
                assert_eq!(read.report.read.get("footprint.links.sheet"), Some(&1), "F14 legacy sheet count is per placement");
            } else {
                assert_eq!(data.fields.len(), 4, "F3 user text never creates a field in 5.x");
                assert_eq!((data.fields[3].value.as_str(), data.fields[3].drawn), ("", false), "F1 missing Description stays empty and hidden");
                assert_eq!(read.report.read.get("footprint.fields"), Some(&2), "F7 only reference and value texts count in 5.x");
                assert!(!read.report.read.contains_key("footprint.fields.hidden"), "F7 empty default fields are not counted");
            }
        }
    }

    #[test]
    fn placement_hidden_standard_texts_and_strict_links() {
        let file = parse_kicad_pcb(br#"(kicad_pcb (version 20230620) (layers (0 F.Cu signal) (31 B.Cu signal))
 (footprint lib:X (layer F.Cu)
  (property Reference old (layer F.SilkS)) (property Reference U1 (layer F.SilkS) hide)
  (property Value old (layer F.SilkS)) (property Value hidden (layer F.SilkS) (effects hide))
  (property Footprint lib:X (layer F.SilkS))
  (sheetname "") (sheetfile root.kicad_sch) (units) (component_classes)))"#)
            .expect("boundary version supports fields and hidden standard text");
        let fp = &file.footprints[0];
        assert_eq!(fp.footprint.texts.iter().map(|t| (t.text.as_str(), t.visible)).collect::<Vec<_>>(),
            [("", false), ("", false), ("lib:X", true)], "F2/F4/F5 hidden Reference and Value retain empty invisible drawings and Footprint retains its drawing");
        assert_eq!(fp.data.fields.iter().map(|f| (f.value.as_str(), f.drawn)).collect::<Vec<_>>(),
            [("U1", false), ("hidden", false), ("", false), ("", false)], "F1/F2/F4 hidden standards kept and Footprint omitted");
        assert_eq!(file.report.read.get("footprint.links.sheet"), Some(&1), "F14 root sheet file counts even with empty name");
        assert_eq!(file.report.read.get("footprint.links.units"), Some(&1), "F14 empty units list counts once");
        assert!(!file.report.read.contains_key("footprint.links.classes"), "F14 empty class list is not counted");
        for (child, reason) in [("(component_classes (other X))", "need a class list"),
            ("(component_classes class)", "need a class list"),
            ("(component_classes (class X Y))", "need exactly one component class name"),
            ("(component_classes (class))", "need a name"),
            ("(pad 1 smd rect (size 1 1) (layers F.Cu) (sim_electrical_type other))", "need source or sink")] {
            let source = format!("(kicad_pcb (version 20241229) (layers (0 F.Cu signal) (31 B.Cu signal))\n (footprint X (layer F.Cu)\n  {child}))");
            let refusal = parse_kicad_pcb(source.as_bytes()).expect_err("F13/F22 invalid class or simulation hint refuses");
            assert!(refusal.message.contains(reason), "F13/F22 refusal explains {child}: {}", refusal.message);
            assert_eq!(refusal.line, 3, "F13/F22 refusal points to offending child {child}");
            assert!(refusal.column > 2, "F13/F22 refusal locates offending token in {child}");
        }
    }

    #[test]
    fn old_board_layers_routes_and_back_footprint() {
        let file = parse_kicad_pcb(br#"(kicad_pcb (version 20171130) (host pcbnew "5.0.2")
 (general (thickness 1.6) (links words))
 (layers (0 TOP_L1.Sig signal) (1 L2.GND power) (2 L3 mixed) (31 BOTTOM signal)
  (32 B.Mask user) (33 F.Mask user) (44 Edge.Cuts user) (38 B.SilkS user))
 (setup (trace_clearance 0.2) (via_size 0.8) (via_drill 0.4) (trace_min 0.15))
 (net 0 "") (net 3 GND) (net 9 ~CLK~)
 (net_class Default "ordinary" (clearance 0.25) (trace_width 0.3) (via_dia 0.7) (add_net GND))
 (gr_line (start 0 0) (end 10 0) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 10 0) (end 10 8) (layer Edge.Cuts) (width 0.05))
 (gr_arc (start 8 8) (end 10 8) (angle 90) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 8 10) (end 0 10) (layer Edge.Cuts) (width 0.05))
 (gr_line (start 0 10) (end 0 0) (layer Edge.Cuts) (width 0.05))
 (module lib:back (layer BOTTOM) (at 20 30 90) locked
  (fp_text reference R1 (at 0 0 90) (layer B.SilkS) (effects (font (size 1 1)) (justify mirror)))
  (fp_arc (start 0 0) (end 2 0) (angle 90) (width 0.1) (layer B.SilkS))
  (pad 1 thru_hole circle (at 1 2 30) (size 2 2) (drill 0.8 (offset 0.4 0.2))
    (layers *.Cu *.Mask) (net 3 GND))
  (pad 2 smd trapezoid (at -2 1 120) (size 3 2) (rect_delta 0.4 0.6)
    (layers BOTTOM B.Mask) (net 9 ~CLK~)))
 (segment (start 1 0) (end 2 0) (width 0.3) (layer TOP_L1.Sig) (net 3))
 (segment (start 2 0) (end 3 0) (width 0.3) (layer TOP_L1.Sig) (net 3))
 (segment (start 3 0) (end 4 0) (width 0.3) (layer TOP_L1.Sig) (net 3))
 (via blind (at 4 0) (size 0.8) (drill 0.4) (layers L2.GND L3) (net 3))
 (zone (net 3) (layer TOP_L1.Sig) (name guard)
   (keepout (tracks not_allowed) (vias not_allowed) (copperpour allowed))
   (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))
 (zone (net 3) (layer TOP_L1.Sig) (polygon (pts (xy 2 2) (xy 3 2) (xy 3 3)))
   (filled_polygon (pts (xy not-a-number nan))) (fill_segments (anything 1e999999))))"#)
            .expect("old board parses without external input");
        assert_eq!(file.layers.copper, vec![LayerId::F_CU, LayerId(1), LayerId(2), LayerId::B_CU], "custom copper order");
        assert_eq!(file.layers.role(LayerId(1)), LayerRole::Plane, "power layer role");
        assert_eq!(file.nets[2].name, "~CLK~", "old net markup remains verbatim");
        assert_eq!(file.classes[0].add_net[0].value, "GND", "class retains net-name assignment");
        assert_eq!(file.setup.trace_clearance, Some(200_000), "legacy default clearance retained");
        assert_eq!(file.setup.track, Some(150_000), "base minimum track retained");
        assert!(file.stackup.is_none(), "build will synthesise absent stackup");
        assert_eq!(file.edges.len(), 5, "outline pieces stay separate for build");
        assert_eq!(file.edges[2].path.end(), P::mm(8.0, -10.0), "legacy board arc sweep flips sign");
        assert_eq!(file.tracks.len(), 3, "route pieces have not been merged");
        assert_eq!(file.tracks[0].net.code, Some(3), "track net remains a code");
        assert_eq!(file.vias[0].kind, ViaKind::Buried, "old blind via between inner layers is buried");
        assert!(file.keepouts[0].forbid.has(Forbid::TRACKS.with(Forbid::VIAS)), "keepout prohibitions");
        assert!(!file.keepouts[0].forbid.has(Forbid::PADS), "missing keepout pad permission is allowed");
        assert_eq!(gap(&file, "zone.stored_fill"), 1, "stored fills counted once per zone and not converted");
        let fp = &file.footprints[0];
        assert_eq!(fp.rot, Angle::deg(270.0), "bottom placement includes half turn");
        assert_eq!(fp.footprint.attrs.mount, FpMount::Thru, "old absent attributes mean through mount");
        assert_eq!(fp.refdes, "R1", "legacy reference becomes placement refdes");
        assert_eq!(fp.footprint.graphics[0].0, LayerId::F_SILK, "back silk returns to front form");
        let Gfx::Stroke { path, .. } = &fp.footprint.graphics[0].1 else { panic!("legacy footprint arc remains stroke"); };
        let Seg::Arc { mid, .. } = path.segs[0] else { panic!("legacy footprint arc keeps midpoint"); };
        assert_eq!(mid, P::new(1_414_214, 1_414_214), "legacy fp_arc midpoint after back conversion");
        let expected_holes = [P::mm(1.0, -2.0), P::mm(-2.0, -1.0)];
        for (i, pad) in fp.footprint.pads.iter().enumerate() {
            let hole = expected_holes[i].rotated(Angle::QUARTER).add(P::mm(20.0, -30.0));
            let expected = if i == 0 { hole.add(P::mm(0.4, -0.2).rotated(Angle::deg(30.0))) } else { hole };
            assert!(placed(fp, pad.at).dist2(expected) <= 4, "front-form pad {i} preserves board shape centre");
            if let Some(drill) = &pad.drill {
                let local_hole = pad.at.add(drill.offset.rotated(pad.rot));
                assert!(placed(fp, local_hole).dist2(hole) <= 4, "offset drill preserves board hole centre");
                assert_eq!(drill.offset, P::mm(-0.4, -0.2), "back drill offset is opposite shape offset");
            }
        }
        let PadShape::Trapezoid { delta, .. } = &fp.footprint.pads[1].stack.front else { panic!("trapezoid stays analytic"); };
        assert_eq!(*delta, P::mm(0.4, -0.6), "back trapezoid swaps delta y");
    }

    #[test]
    fn current_board_fields_stackup_padstack_and_gaps() {
        let file = parse_kicad_pcb(br##"(kicad_pcb (version 20261031) (generator "fab") (generator_version "1.0")
 (layers (0 "F.Cu" signal "Top") (1 "In1.Cu" power) (2 "In2.Cu" mixed) (31 "B.Cu" signal)
  (36 "F.SilkS" user) (38 "B.SilkS" user) (44 "Edge.Cuts" user))
 (footprint "lib:part" (layer "F.Cu") (transform (translate 5 6) (rotate 90) (scale 1 2))
  (attr smd exclude_from_bom)
  (property "Reference" "U1" (at 1 0 30) (layer "F.SilkS") (effects (font (size 1.2 1.2))))
  (property "Private" "memo" (effects (font (size unread unread))) (hide yes))
  (solder_mask_margin 0) (solder_paste_margin_ratio -0.1) (thermal_gap 0.25)
  (pad "1" thru_hole circle (at 1 2 90) (size 1.2 1.2) (drill 0.5) (layers "*.Cu" "*.Mask") (net "GND")
   (thermal_bridge_width 0.2)
   (padstack (mode front_inner_back) (layer "Inner" (shape circle) (size 1 1))
    (layer "B.Cu" (shape rect) (size 1.4 1.1) (thermal_gap unparsed))))
  (pad "2" non_component roundrect (size 0 1) (layers "F.Cu" "F.Mask") (roundrect_rratio 0.2) (net "0"))
  (zone (layer F.Cu) (fill (thermal_gap unread)) (polygon (pts (xy unread unread)))
    (filled_polygon (pts (xy unread unread))))
  (zone (layer F.Cu) (keepout (tracks not_allowed)) (net (ignored value))
    (fill (thermal_gap unread)) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))))
 (segment (start 0 0) (end 1 0) (width 0.2) (layers "F.Cu" "F.Mask") (net "GND"))
 (arc (start 1 0) (mid 2 0) (end 3 0) (width 0.2) (layer "F.Cu") (net "GND"))
 (via (at 1 0) (size 0.7) (drill 0.3) (layers "F.Cu" "B.Cu") (net "GND")
  (tenting (front none) (back no)) (padstack (layer "Inner" (size 0.9))))
 (zone (net "GND") (layers "*.Cu") (priority 70000)
  (fill (thermal_gap 0.31) (thermal_bridge_width 0.22) (island_removal_mode 2) (island_area_min whatever)
   (mode hatch) (smoothing fillet) (radius discarded))
  (polygon (pts (xy 0 0) (xy 4 0) (xy 4 4) (xy 0 4))))
 (gr_rect (start 0 0) (end 2 1) (radius 0.1) (stroke (width 0.2) (type dash))
   (fill solid) (layer "F.SilkS"))
 (gr_text "A\nB" (at 2 3 450) (layer "F.SilkS" knockout)
  (effects (font (face "other") (size 1 2) (bold yes) (italic yes)) (justify left) (line_spacing 2)))
 (zone (placement (enabled yes)) (layer F.Cu) (min_thickness unread) (polygon (pts (xy unread unread))))
 (generated (type tuning_pattern) (data arbitrary)) (group "g" (members arbitrary))
 (setup (zone_clearance 0.4) (tenting front back)
   (stackup (layer "F.Mask" (type "mask") (thickness 0.01) (color "#123456"))
    (layer "F.Cu" (thickness 0.035) (material "copper"))
    (layer "dielectric 1" (type "core") (thickness 0.5 locked) (material "FR-4") (epsilon_r 4.2)
      (addsublayer) (thickness 0.4) (material "resin") (loss_tangent 0.01))
    (copper_finish "ENIG"))))"##).expect("current board parses");
        assert_eq!(file.version.generator.as_deref(), Some("fab"), "generator preserved");
        let fp = &file.footprints[0];
        assert_eq!(fp.at, P::mm(5.0, -6.0), "transform translation changes frame");
        assert_eq!(fp.footprint.texts[0].rot, Angle::deg(30.0), "new relative text angle stays relative");
        assert_eq!(fp.refdes, "U1", "Reference property provides refdes");
        assert_eq!(fp.footprint.texts[0].text, "", "reference drawing resolves from placement");
        assert_eq!(fp.footprint.texts.len(), 1, "hidden user property is not drawn or numerically converted");
        assert_eq!((&*fp.data.fields[4].name, fp.data.fields[4].value.as_str(), fp.data.fields[4].drawn),
            ("Private", "memo", false), "hidden property survives without converting unused dimensions");
        assert_eq!(file.report.read.get("footprint.fields"), Some(&2), "provided fields counted without empty defaults");
        assert_eq!(file.report.read.get("footprint.fields.hidden"), Some(&1), "hidden field counted as read");
        assert_eq!(gap(&file, "footprint.fields"), 0, "retained fields no longer reported as a gap");
        assert_eq!(fp.footprint.keepouts.len(), 1, "footprint copper zone skipped but keepout retained");
        assert!(fp.footprint.keepouts[0].forbid.has(Forbid::TRACKS), "keepout ignores copper-zone settings");
        assert_eq!(gap(&file, "zone.stored_fill"), 0, "fills inside a skipped footprint zone do not count separately");
        let pad = &fp.footprint.pads[0];
        assert_eq!(pad.mask_margin, Some(0), "modern zero margin remains explicit");
        assert_eq!(pad.paste.ratio_ppm, Some(-100_000), "footprint paste ratio inherited");
        assert_eq!(fp.pad_nets[0].name.as_deref(), Some("GND"), "pad net is unresolved name");
        assert_eq!(fp.pad_nets[1].name.as_deref(), Some("0"), "quoted numeric net is a name");
        assert_eq!(pad.stack.inner, Some(PadShape::Circle { d: 1_000_000 }), "inner padstack shape");
        assert_eq!(pad.stack.back, Some(PadShape::Rect { size: P::mm(1.4, 1.1) }), "back padstack shape");
        let thermal = pad.thermal.as_ref().expect("combined pad and footprint thermal");
        assert_eq!((thermal.gap, thermal.spoke, thermal.angle), (250_000, 200_000, Angle::deg(45.0)), "inherited thermal values");
        assert_eq!(file.tracks[0].layer, LayerId::F_CU, "track keeps copper layer from mask-opening list");
        assert!(file.tracks[1].mid.is_none(), "collinear track arc becomes a line");
        assert_eq!((file.vias[0].size, file.vias[0].tent), (900_000, Tent::Front), "via uses max diameter and deferred tent default");
        assert_eq!(file.zones[0].settings.clearance, Some(400_000), "zone inherits later setup clearance");
        assert_eq!(file.zones[0].settings.thermal.spoke, 220_000, "zone thermal width mapped");
        assert_eq!(file.zones[0].priority, u16::MAX, "priority clamps to model range");
        let stack = file.stackup.as_ref().expect("physical stack retained");
        assert_eq!(stack.layers.len(), 4, "dielectric sublayer is separate");
        assert_eq!(stack.mask_color, Some(0x123456ff), "RGB color gets opaque alpha");
        assert_eq!(stack.finish, Some(Finish::Enig), "finish mapped");
        let Gfx::Fill(Shape::Rect { half, corner, .. }) = &file.graphics[0].gfx else { panic!("filled rectangle stays analytic"); };
        assert_eq!((*half, *corner), (P::mm(1.1, 0.6), 200_000), "filled rectangle grows by half stroke");
        assert_eq!(file.texts[0].text, "A\nB", "multiline text escape retained");
        assert_eq!(file.texts[0].rot, Angle::QUARTER, "text rotation normalised");
        for key in ["footprint.scale", "pad.stack_extras", "pad.non_component", "pad.zero_size",
            "track.mask_opening", "via.stack", "zone.hatch_fill", "zone.island_area", "zone.smoothing",
            "graphic.dashed", "text.aspect", "text.justify", "text.font", "text.style", "text.line_spacing",
            "text.knockout", "tuning_pattern", "group", "rules.project_file", "stackup.extras",
            "footprint.zone", "zone.placement_area"] {
            assert_eq!(gap(&file, key), 1, "feature count for {key}");
        }
        assert!(file.report.made.is_empty(), "parse does not claim build counts");
    }

    #[test]
    fn unknowns_keep_paths_locations_counts_and_following_items() {
        let file = parse_kicad_pcb(b"(kicad_pcb\n (version 20261031)\n (layers (0 F.Cu signal) (31 B.Cu signal))\n (footprint X (layer F.Cu)\n  (pad 1 smd rect (size 1 1) (layers F.Cu)\n   (frobnicate (nested bad-number))\n   (frobnicate again) mystery))\n (future (nested (data 1e99999)))\n (future twice)\n (segment (start 0 0) (end 1 0) (width 0.2) (layer F.Cu) (net GND)))")
            .expect("unknown extensions do not reject a board");
        let pad = &file.report.unknown["kicad_pcb>footprint>pad>frobnicate"];
        assert_eq!((pad.count, pad.first), (2, (crate::SourceFile::Board, 6, 5)), "pad extension path, count and first location");
        let top = &file.report.unknown["kicad_pcb>future"];
        assert_eq!((top.count, top.first), (2, (crate::SourceFile::Board, 8, 3)), "top extension path, count and first location");
        assert_eq!(file.report.unknown["kicad_pcb>footprint>pad>mystery"].count, 1, "bare unknown flag reported");
        assert_eq!(file.report.unknown.len(), 3, "unknown children inside skipped lists are not separately counted");
        assert_eq!(file.tracks.len(), 1, "known content following extensions retained");
        let text = file.report.render();
        assert!(text.contains("Unknown kicad_pcb>future: 2 at 8:3"), "plain text report includes first source location");
    }

    #[test]
    fn headers_version_gates_and_refusal_locations() {
        let old = parse_kicad_pcb(b"\xef\xbb\xbf# comment\n(kicad_pcb (version 20240705) (net 1 A|B))")
            .expect("old bar names and leading BOM parse");
        assert_eq!(old.nets[0].name, "A|B", "old version keeps bar within name");
        let new = parse_kicad_pcb(b"(kicad_pcb (version 20240706) (net 1 A|B))").expect("new bars are unknown tokens");
        assert_eq!(new.nets[0].name, "A", "new version separates bar from name");
        assert!(new.report.unknown.contains_key("kicad_pcb>net>|"), "separated bar is not silently discarded");
        assert_eq!(parse_kicad_pcb(b"(kicad_pcb)").expect("absent version").version.version, 20201115, "absent version default");
        let tail = parse_kicad_pcb(b"(kicad_pcb)\n</content>\n").expect("a tail after the board is not read");
        assert_eq!(tail.report.gaps["file.trailing_content"].first, (crate::SourceFile::Board, 2, 1), "trailing content counted at its location");
        let missing = parse_kicad_pcb(b"(kicad_pcb\n").expect_err("missing close refuses");
        assert_eq!((missing.line, missing.column), (2, 1), "missing parenthesis location");
        let quote = parse_kicad_pcb(b"(kicad_pcb\n (generator \"open\n)").expect_err("raw multiline string refuses");
        assert_eq!((quote.line, quote.column), (2, 13), "unterminated string opening location");
        let number = parse_kicad_pcb(b"(kicad_pcb\n (general (thickness nope)))").expect_err("length needs number token");
        assert_eq!((number.line, number.column), (2, 22), "bad length value location");
        assert!(number.message.contains("need a number"), "number refusal explains expected type");
        let recent = parse_kicad_pcb(b"(kicad_pcb (version 20990101) (generator_version 12.0) (general (thickness nope)))")
            .expect_err("future-version malformed value refuses");
        assert!(recent.message.contains("newer than reader") && recent.message.contains("12.0"), "future refusal carries provenance");
        for (source, reason) in [
            ("(other)", "not a board file"),
            ("(kicad_pcb (setup stackup)))", "unbalanced closing"),
            ("(kicad_pcb (layers (0 F.Cu signal)))", "valid copper layer count"),
            ("(kicad_pcb (layers (0 F.Cu signal) (31 B.Cu signal) (44 unknown user)))", "known board layer"),
            ("(kicad_pcb (net_class Default x) (net_class Default y))", "duplicate net class"),
            ("(kicad_pcb (segment (layer F.Cu)))", "before layers table"),
        ] {
            let err = parse_kicad_pcb(source.as_bytes()).expect_err("specified structural refusal");
            assert!(err.message.contains(reason), "structural refusal for {source}: {}", err.message);
        }
    }
    #[test]
    fn boundaries_preserve_bottom_padstacks_and_change_only_gated_values() {
        for version in [20211014, 20211015, 20240201, 20240202, 20250925, 20250926, 20260615, 20260616] {
            let source = format!(r#"(kicad_pcb (version {version})
 (layers (0 F.Cu signal) (1 In1.Cu signal) (2 In2.Cu signal) (31 B.Cu signal))
 (net 7 ROUTE)
 (footprint X (layer B.Cu) (at 10 20 30) (solder_mask_margin 0.2) (thermal_width 0.2)
  (fp_text user Label (at 0 0 10) (layer B.SilkS) (effects (justify mirror)))
  (pad 1 thru_hole custom (at 1 0 30) (size 1 1) (drill 0.3) (layers *.Cu)
   (options (anchor circle)) (thermal_gap 0.3) (solder_mask_margin 0) (net 7))
  (pad 2 thru_hole trapezoid (size 2 1) (rect_delta 0.2 0.4) (drill 0.3) (layers *.Cu)
   (padstack (mode front_inner_back) (layer Inner (shape rect) (size 1.2 0.7) (offset 0.1 0))
    (layer B.Cu (shape oval) (size 1.4 0.8)))))
 (via blind (at 1 2) (size 0.8) (drill 0.4) (layers In1.Cu In2.Cu) (net 7)))"#);
            let file = parse_kicad_pcb(source.as_bytes()).expect("version-boundary board parses");
            let fp = &file.footprints[0];
            let pad = &fp.footprint.pads[0];
            assert_eq!(pad.mask_margin, Some(if version <= 20240201 { 200_000 } else { 0 }),
                "zero local margin changes inheritance at version {version}");
            assert_eq!(pad.thermal.as_ref().expect("thermal inherited").angle,
                Angle::deg(if version <= 20211014 { 0.0 } else { 45.0 }),
                "custom round anchor thermal angle at version {version}");
            assert_eq!(fp.footprint.texts[0].rot, Angle::deg(if version < 20260616 { 200.0 } else { 170.0 }),
                "absolute versus relative bottom text angle at version {version}");
            assert_eq!(file.vias[0].kind, if version < 20250926 { ViaKind::Buried } else { ViaKind::Blind },
                "blind/buried interpretation at version {version}");
            if version < 20251028 {
                assert_eq!(fp.pad_nets[0].code, Some(7), "old numeric net token is a code at {version}");
            } else {
                assert_eq!(fp.pad_nets[0].name.as_deref(), Some("7"), "new numeric net token is a name at {version}");
            }
            let stack = &fp.footprint.pads[1].stack;
            assert_eq!(stack.front, PadShape::Oval { size: P::mm(1.4, 0.8) }, "file back shape becomes front at {version}");
            assert_eq!(stack.back, Some(PadShape::Trapezoid { size: P::mm(2.0, 1.0), delta: P::mm(0.2, -0.4) }),
                "file front shape and its trapezoid details become back at {version}");
            assert_eq!(stack.inner, Some(PadShape::Rect { size: P::mm(1.2, 0.7) }), "inner pad shape at {version}");
            assert_eq!(gap(&file, "pad.stack_extras"), 1, "different inner offset is counted at {version}");
        }
    }

}
