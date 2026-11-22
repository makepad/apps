//! One unit's circuit as the layout's own graph. Keys are the circuit's indices: node `i` is
//! `CompId(i)`, net `i` is `NetId(i)`, block `i` is `BlockId(i)`, so results map back to `Sel`
//! without a table. Symbol geometry comes from each part's first symbol unit: pins on the
//! connection grid, the body box from its graphics (rounded outward to the grid).
//!
//! Cost: linear in components, pins and connections.

use crate::graph::{Flow, GBlock, GBlockPort, GBus, GNet, GNode, GPort, LayoutGraph, NetKind, NetStyle, PortKind, Refit, Role};
use crate::layered::Side;
use fab_elec_circuit::{self as circuit, Circuit, Intent, SheetHints, Target};
use fab_elec_core::{CompId, SchPt};
use fab_elec_parts::{PartDef, PartKind, PinDir, PinKind, SymGraphic, SymPin, SymbolUnit};

/// The role a part plays, from what its definition says it is (`PartDef::kind`), never from
/// its reference designator.
pub fn role_of(part: &PartDef) -> Role {
    match &part.kind {
        PartKind::Resistor => Role::Resistor,
        PartKind::Capacitor => Role::Capacitor,
        PartKind::Inductor => Role::Inductor,
        PartKind::Fuse => Role::Fuse,
        PartKind::Diode => Role::Diode,
        PartKind::Led => Role::Led,
        PartKind::Regulator | PartKind::Mcu | PartKind::Ic | PartKind::Analog => Role::Ic,
        PartKind::Connector => Role::Connector,
        PartKind::Switch => Role::Switch,
        PartKind::Crystal => Role::Crystal,
        PartKind::Transistor => Role::Transistor,
        PartKind::PowerFlag if part.pins.len() == 1 => Role::Flag,
        PartKind::Potentiometer | PartKind::PowerFlag | PartKind::Other(_) => Role::Other,
    }
}

fn port_kind(kind: PinKind) -> PortKind {
    match kind {
        PinKind::In => PortKind::In,
        PinKind::Out => PortKind::Out,
        PinKind::Bidi => PortKind::Bidi,
        PinKind::TriState | PinKind::OpenCollector | PinKind::OpenEmitter => PortKind::WeakOut,
        PinKind::Passive | PinKind::Free | PinKind::Unspecified => PortKind::Passive,
        PinKind::PowerIn => PortKind::PowerIn,
        PinKind::PowerOut => PortKind::PowerOut,
        PinKind::NoConnect => PortKind::NoConnect,
    }
}

fn block_port_kind(kind: &circuit::PortKind) -> PortKind {
    match kind {
        circuit::PortKind::PowerIn | circuit::PortKind::Ground => PortKind::PowerIn,
        circuit::PortKind::PowerOut => PortKind::PowerOut,
        circuit::PortKind::In => PortKind::In,
        circuit::PortKind::Out => PortKind::Out,
        circuit::PortKind::Passive => PortKind::Passive,
        circuit::PortKind::Bidi | circuit::PortKind::Bus { .. } | circuit::PortKind::Pair | circuit::PortKind::Protocol(_) => PortKind::Bidi,
    }
}

fn side_of(side: circuit::PortSide) -> Side {
    match side {
        circuit::PortSide::Left => Side::Left,
        circuit::PortSide::Right => Side::Right,
        circuit::PortSide::Top => Side::Top,
        circuit::PortSide::Bottom => Side::Bottom,
    }
}

fn flow_of(flow: circuit::Flow) -> Flow {
    match flow {
        circuit::Flow::Right => Flow::Right,
        circuit::Flow::Down => Flow::Down,
        circuit::Flow::Left => Flow::Left,
        circuit::Flow::Up => Flow::Up,
    }
}

/// Pin name size as the explorer draws it, in grid steps.
const NAME_SIZE: f64 = 0.8;
/// A side name's distance from its body edge, the clear space between two names, and a centred
/// name's margin to the body's corners.
const NAME_SIDE: f64 = 0.4;
const NAME_GAP: f64 = 0.5;
const NAME_EDGE: f64 = 0.4;

/// The first symbol unit as the sheet draws it, `quarter` when its pose turns it an odd number of
/// quarter turns. A box symbol (a single rectangle with every pin on its edges and some pin names
/// shown) is redrawn so its names fit where they belong: the pins of each edge that reads across
/// the sheet are spaced by their names, the rows of the edges that read upright stay one step apart
/// below and above the across edges' name rows, and the body is as wide as its facing names need.
/// Pins keep their edge, order and length. Any other symbol is drawn as it is (None).
pub fn sheet_unit(part: &PartDef, quarter: bool) -> Option<SymbolUnit> {
    if role_of(part) == Role::Flag { return None }
    let unit = part.symbol.units.first()?;
    let [SymGraphic::Rect { min, max, w, fill }] = unit.graphics.as_slice() else { return None };
    let grid = SchPt::GRID;
    let name_w = |sp: &SymPin| {
        let name = part.pins.get(sp.pin.ix()).map_or("", |p| p.name.as_str());
        if sp.name_shown && !name.is_empty() && name != "~" {
            crate::text::width(name, NAME_SIZE)
        } else {
            0.0
        }
    };
    if !unit.pins.iter().any(|sp| name_w(sp) > 0.0) {
        return None;
    }
    // Edges in a frame where x reads across the sheet: 0 left, 1 right, 2 top, 3 bottom.
    let canon = |x: i32, y: i32| if quarter { (y, x) } else { (x, y) };
    let mut edges: [Vec<(usize, i32, f64)>; 4] = Default::default();
    for (i, sp) in unit.pins.iter().enumerate() {
        if sp.len <= 0 || sp.len % grid != 0 {
            return None;
        }
        let at = sp.at.to_p();
        let (dx, dy) = match sp.dir {
            PinDir::Right => (1, 0),
            PinDir::Left => (-1, 0),
            PinDir::Up => (0, 1),
            PinDir::Down => (0, -1),
        };
        let end = (at.x + dx * sp.len, at.y + dy * sp.len);
        let on_edge = match sp.dir {
            PinDir::Right => end.0 == min.x,
            PinDir::Left => end.0 == max.x,
            PinDir::Up => end.1 == min.y,
            PinDir::Down => end.1 == max.y,
        } && (min.x..=max.x).contains(&end.0) && (min.y..=max.y).contains(&end.1);
        if !on_edge {
            return None;
        }
        // The edge in the symbol's frame, then in the across frame.
        let (side_x, side_y) = match sp.dir {
            PinDir::Right => (-1, 0),
            PinDir::Left => (1, 0),
            PinDir::Up => (0, -1),
            PinDir::Down => (0, 1),
        };
        let (cx, cy) = canon(side_x, side_y);
        let edge = match (cx, cy) {
            (-1, _) => 0,
            (1, _) => 1,
            (_, 1) => 2,
            _ => 3,
        };
        let (ax, ay) = canon(sp.at.x, sp.at.y);
        // Upright edges list top first, across edges left first.
        let along = if edge < 2 { -ay } else { ax };
        edges[edge].push((i, along, name_w(sp)));
    }
    for e in &mut edges {
        e.sort_by_key(|&(i, along, _)| (along, i));
    }
    // Across edges: each pin a whole number of steps from the last, far enough for both names,
    // the run centred on the body.
    let mut across: [Vec<i32>; 2] = Default::default();
    for (k, e) in edges[2..].iter().enumerate() {
        let mut x = 0;
        for (j, &(_, _, w)) in e.iter().enumerate() {
            if j > 0 {
                let prev = e[j - 1].2;
                x += (((prev + w) * 0.5 + NAME_GAP).ceil() as i32).max(1);
            }
            across[k].push(x);
        }
        let shift = x.div_euclid(2);
        for x in &mut across[k] {
            *x -= shift;
        }
    }
    let mut hw = 2;
    for (k, e) in edges[2..].iter().enumerate() {
        for (j, &(_, _, w)) in e.iter().enumerate() {
            let x = across[k][j].abs();
            hw = hw.max(x + 1).max((x as f64 + w * 0.5 + NAME_EDGE).ceil() as i32);
        }
    }
    let rows = edges[0].len().max(edges[1].len());
    for r in 0..rows {
        let need = match (edges[0].get(r), edges[1].get(r)) {
            (Some(a), Some(b)) => a.2 + b.2 + 2.0 * NAME_SIDE + NAME_GAP,
            (Some(a), None) | (None, Some(a)) => a.2 + NAME_SIDE + NAME_EDGE,
            (None, None) => 0.0,
        };
        hw = hw.max((need * 0.5).ceil() as i32);
    }
    // Upright rows start below the top edge's name row and end above the bottom edge's.
    let band = |e: &Vec<(usize, i32, f64)>| e.iter().any(|&(_, _, w)| w > 0.0) as i32;
    let (top, bottom) = (band(&edges[2]), band(&edges[3]));
    let hh = (2i32).max((rows as i32 + top + bottom + 2) / 2);
    let mut pins = unit.pins.clone();
    let mut place = |i: usize, (x, y): (i32, i32)| {
        let sp = &mut pins[i];
        let n = sp.len / grid;
        let (px, py) = canon(x, y);
        sp.at = match sp.dir {
            PinDir::Right => SchPt::new(px - n, py),
            PinDir::Left => SchPt::new(px + n, py),
            PinDir::Up => SchPt::new(px, py - n),
            PinDir::Down => SchPt::new(px, py + n),
        };
    };
    for (r, &(i, _, _)) in edges[0].iter().enumerate() {
        place(i, (-hw, hh - 1 - top - r as i32));
    }
    for (r, &(i, _, _)) in edges[1].iter().enumerate() {
        place(i, (hw, hh - 1 - top - r as i32));
    }
    for (j, &(i, _, _)) in edges[2].iter().enumerate() {
        place(i, (across[0][j], hh));
    }
    for (j, &(i, _, _)) in edges[3].iter().enumerate() {
        place(i, (across[1][j], -hh));
    }
    let (sx, sy) = canon(hw, hh);
    Some(SymbolUnit {
        name: unit.name.clone(),
        body_style: unit.body_style,
        graphics: vec![SymGraphic::Rect {
            min: SchPt::new(-sx, -sy).to_p(),
            max: SchPt::new(sx, sy).to_p(),
            w: *w,
            fill: *fill,
        }],
        pins,
    })
}

/// A part's ports (indexed like its pins) and body box from its first symbol unit as the sheet
/// draws it at `quarter` (see `sheet_unit`). A pin the symbol does not draw gets a port at the
/// origin, on the left. A power flag uses the family glyph's bounds and connects at its foot.
pub fn symbol_geometry(part: &PartDef, quarter: bool) -> ([i32; 4], Vec<GPort>) {
    if role_of(part) == Role::Flag {
        let pin = &part.pins[0];
        let foot = GPort { name: pin.name.clone(), at: [0, 0], side: Side::Bottom, kind: port_kind(pin.kind), nc: false };
        return ([-1, 0, 1, 2], vec![foot]);
    }
    unit_geometry(part, sheet_unit(part, quarter).as_ref().or(part.symbol.units.first()))
}

fn unit_geometry(part: &PartDef, unit: Option<&SymbolUnit>) -> ([i32; 4], Vec<GPort>) {
    let grid = SchPt::GRID as f64;
    let mut lo = [f64::MAX; 2];
    let mut hi = [f64::MIN; 2];
    let mut grow = |x: f64, y: f64| {
        lo = [lo[0].min(x), lo[1].min(y)];
        hi = [hi[0].max(x), hi[1].max(y)];
    };
    let mut ports: Vec<GPort> = part
        .pins
        .iter()
        .map(|p| GPort { name: p.name.clone(), at: [0, 0], side: Side::Left, kind: port_kind(p.kind), nc: false })
        .collect();
    if let Some(unit) = unit {
        for g in &unit.graphics {
            let pts: Vec<(i32, i32)> = match g {
                SymGraphic::Poly { pts, .. } => pts.iter().map(|p| (p.x, p.y)).collect(),
                SymGraphic::Arc { a, mid, b, .. } => vec![(a.x, a.y), (mid.x, mid.y), (b.x, b.y)],
                SymGraphic::Circle { c, r, .. } => vec![(c.x - r, c.y - r), (c.x + r, c.y + r)],
                SymGraphic::Rect { min, max, .. } => vec![(min.x, min.y), (max.x, max.y)],
                SymGraphic::Text { .. } => vec![],
            };
            for (x, y) in pts {
                grow(x as f64 / grid, y as f64 / grid);
            }
        }
        for sp in &unit.pins {
            let Some(port) = ports.get_mut(sp.pin.ix()) else { continue };
            port.at = [sp.at.x, sp.at.y];
            // The direction points from the connection point towards the body.
            port.side = match sp.dir {
                PinDir::Right => Side::Left,
                PinDir::Left => Side::Right,
                PinDir::Up => Side::Bottom,
                PinDir::Down => Side::Top,
            };
            grow(sp.at.x as f64, sp.at.y as f64);
        }
    }
    if lo[0] > hi[0] {
        return ([-1, -1, 1, 1], ports);
    }
    let body = [lo[0].floor() as i32, lo[1].floor() as i32, hi[0].ceil() as i32, hi[1].ceil() as i32];
    (body, ports)
}

/// The graph of one circuit with `hints` layered over the circuit's own sheet hints (a drag's
/// preview passes its proposed hint here).
pub fn graph_of(c: &Circuit, hints: &SheetHints) -> LayoutGraph {
    graph_led(c, hints, None)
}

/// `graph_of` with the part or block a drag moves (by path) as the lead: pinned parts and blocks
/// it runs into make room for it (`LayoutGraph::lead`, `lead_block`).
pub fn graph_led(c: &Circuit, hints: &SheetHints, lead: Option<&str>) -> LayoutGraph {
    let hint = |path: &str| {
        hints.parts.iter().rev().chain(c.hints.parts.iter().rev()).find(|(p, _)| p == path).map(|(_, h)| h)
    };
    let lead_block = lead.filter(|lead| !lead.is_empty()).and_then(|lead| c.tree.iter().position(|b| b.path == lead));
    let lead = lead.and_then(|lead| c.comps.iter().position(|comp| comp.path == lead));
    let mut g = LayoutGraph { lead: lead.map(|n| n as u32), lead_block: lead_block.map(|b| b as u32), ..LayoutGraph::default() };
    for (i, comp) in c.comps.iter().enumerate() {
        let h = hint(&comp.path);
        // A hint's turn decides which fit the sheet draws; the other one waits for a reveal's turn.
        let quarter = h.is_some_and(|h| h.turn & 1 == 1);
        let (body, mut ports) = symbol_geometry(&comp.part, quarter);
        let refit = sheet_unit(&comp.part, !quarter).map(|unit| {
            let (body, ports) = unit_geometry(&comp.part, Some(&unit));
            Box::new(Refit { body, at: ports.iter().map(|p| p.at).collect() })
        });
        for pin in &comp.nc {
            if let Some(p) = ports.get_mut(pin.ix()) {
                p.nc = true;
            }
        }
        let me = CompId(i as u32);
        let near = c.intents.iter().find_map(|intent| match intent {
            Intent::Near { a: Target::Comp(a), b, .. } if *a == me => match b {
                Target::Comp(b) if *b != me => Some((b.0, None)),
                Target::Pin(b, pin) if *b != me => Some((b.0, Some(pin.0 as u32))),
                _ => None,
            },
            _ => None,
        });
        g.nodes.push(GNode {
            key: i as u32,
            role: role_of(&comp.part),
            block: comp.block.0,
            body,
            ports,
            order: i as u32,
            name: comp.refdes.clone(),
            value: comp.value.as_ref().map(|v| v.text.clone()),
            pin_at: h.and_then(|h| h.at).map(|p| [p.x, p.y]),
            turn: h.map(|h| h.turn),
            mirror: h.is_some_and(|h| h.mirror),
            free: h.is_some_and(|h| h.free && h.at.is_some()),
            near,
            quarter,
            refit,
        });
    }
    for (i, net) in c.nets.iter().enumerate() {
        let style = hints
            .nets
            .iter()
            .rev()
            .chain(c.hints.nets.iter().rev())
            .find(|(n, _)| *n == net.name)
            .map(|(_, s)| match s {
                circuit::NetStyle::Wire => NetStyle::Wire,
                circuit::NetStyle::Label => NetStyle::Label,
            });
        g.nets.push(GNet {
            key: i as u32,
            name: net.name.clone(),
            kind: match net.kind {
                circuit::NetKind::Signal => NetKind::Signal,
                circuit::NetKind::Power => NetKind::Power,
                circuit::NetKind::Ground => NetKind::Ground,
            },
            volts: net.volts.map(|v| 0.5 * (v.min + v.max)),
            pins: Vec::new(),
            bus: None,
            style,
        });
    }
    for (ci, comp) in c.comps.iter().enumerate() {
        for &(pin, net) in &comp.conns {
            if let Some(n) = g.nets.get_mut(net.ix()) {
                n.pins.push((ci as u32, pin.0 as u32));
            }
        }
    }
    for (bi, bus) in c.buses.iter().enumerate() {
        let nets: Vec<u32> = bus.members.iter().map(|(_, n)| n.0).collect();
        for &n in &nets {
            if let Some(net) = g.nets.get_mut(n as usize) {
                net.bus.get_or_insert(bi as u32);
            }
        }
        g.buses.push(GBus { name: bus.name.clone(), nets });
    }
    let mut sibling = vec![0u32; c.tree.len()];
    for (i, b) in c.tree.iter().enumerate() {
        let def = b.def.and_then(|d| c.block_defs.get(d.0 as usize));
        let order = match b.parent {
            Some(p) => {
                let o = sibling[p.ix()];
                sibling[p.ix()] += 1;
                o
            }
            None => 0,
        };
        let ports = b
            .ports
            .iter()
            .map(|(name, nets)| {
                let pdef = def.and_then(|d| d.ports.iter().find(|p| p.name == *name));
                let side = def.and_then(|d| d.sheet.sides.iter().find(|(n, _)| n == name)).map(|(_, s)| side_of(*s));
                GBlockPort {
                    name: name.clone(),
                    side,
                    kind: pdef.map(|p| block_port_kind(&p.kind)).unwrap_or(PortKind::Passive),
                    nets: nets.iter().map(|n| n.0).collect(),
                }
            })
            .collect();
        let h = hint(&b.path);
        g.blocks.push(GBlock {
            key: i as u32,
            parent: b.parent.map(|p| p.0),
            path: b.path.clone(),
            title: def.map(|d| d.name.clone()),
            flow: h.and_then(|h| h.flow).map(flow_of).or(def.map(|d| flow_of(d.sheet.flow))).unwrap_or_default(),
            ports,
            order,
            pin_at: h.and_then(|h| h.at).map(|p| [p.x, p.y]),
        });
    }
    g
}
