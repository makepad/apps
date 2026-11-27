//! The sheet as retained instance rows, upright text, and a shared picking/semantics table.
//! `item` is a SheetRows row; selection comes from the host's item-state texture
//! (`electronics/shared/item-state.md`). Geometry and motion remain indexed by graph node.

use super::glyphs::{self, Align, Family, Fill, Prim, TextKind, Xf, BUS_W, WIRE_W};
use super::liveix::{mini_nets, SceneLive};
use crate::live::flags::{anchor_pin, Anchor, PinSpot};
use crate::canvas::{
    draw::{ArcInst, BoxInst, BridgeInst, Common, LineInst, MiniInst, PinInst},
    looks::{ease_out, REVEAL_SECONDS, STATE_SECONDS},
    Camera2d, CanvasItems, CanvasNode, Look, Tokens,
};
use fab_elec_circuit::{Circuit, NetKind};
use fab_elec_core::{BlockId, CompId, CompRef, NetId, NetRef, Sel};
use fab_elec_design::Design;
use fab_elec_explore::groups::{spanning, wire_groups};
use fab_elec_explore::layered::Side;
use fab_elec_explore::legible::Legible;
use fab_elec_explore::rows::RowIndex;
use fab_elec_explore::sheet::{self, Owner, Run, SheetLayout, TextRole, LINE_GAP};
use fab_elec_parts::{PartKind, PinKind};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Text with no selectable owner; its geometry uses the shader's -1 item id.
pub const NO_ITEM: u32 = u32::MAX;

/// Text smaller than this many screen points is not drawn, nor are the leaders to it.
pub const TEXT_MIN_POINTS: f64 = 4.0;

/// Part references are enlarged to this screen size while the floor is active.
pub const TEXT_FLOOR_POINTS: f64 = 6.0;
/// Beyond this enlargement, use the ordinary text cutoff for an overview.
pub const FLOOR_MAX_SCALE: f64 = 4.0;

/// Travel arcs bulge by this fraction of their chord.
pub const BRIDGE_BEND: f32 = 0.2;

/// The screen size in points of text `size` world units tall.
pub fn text_points(size: f64, px_per_unit: f64) -> f64 {
    size * px_per_unit * 0.85
}

/// The same translation/opacity endpoints and f32 clock as the canvas shader.
#[derive(Clone, Copy, Debug)]
pub struct Flight {
    pub from: [f32; 4],
    pub to: [f32; 4],
    pub span: [f32; 2],
}

impl Flight {
    const REST: Self = Self { from: [0.0, 0.0, 0.0, 1.0], to: [0.0, 0.0, 0.0, 1.0], span: [0.0; 2] };

    pub fn pose(&self, now: f64) -> [f32; 4] {
        let t = if self.span[1] > 0.0 {
            ease_out(((now as f32 - self.span[0]) / self.span[1]) as f64) as f32
        } else { 1.0 };
        std::array::from_fn(|i| self.from[i] + (self.to[i] - self.from[i]) * t)
    }

    pub fn apply(&self, c: &mut Common) {
        c.pose_a = self.from;
        c.pose_b = self.to;
        c.span = self.span;
    }

    pub fn end(&self) -> f64 {
        self.span[0] as f64 + self.span[1] as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leg { Arrive, Leave, Settle }

#[derive(Clone, Debug)]
pub struct FlyFrom {
    /// One motion per component node, independent of the item's SheetRows row.
    pub parts: Vec<Option<Flight>>,
    /// The layer clock for details.
    pub detail: Flight,
    pub leg: Leg,
    /// When the action began, on the motion clock.
    pub start: f64,
    /// A retarget replaces visible wires or arcs, so its arcs start at full alpha.
    pub continued: bool,
    /// Text alpha per component node at take-off; missing means zero.
    pub text_from: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct SceneState {
    /// SheetRows row, as used by the hover uniforms.
    pub hover: Option<u32>,
    pub assemble_at: Option<f64>,
    pub fly: Option<FlyFrom>,
}

/// Take-off and landing: `from` until `out`, down to zero over `dur`; zero until `land`, up to `to` over `dur`.
/// `dur <= 0` steps. Eased with `ease_out`, as before.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextFade { pub out: f32, pub land: f32, pub dur: f32, pub from: f32, pub to: f32 }

impl TextFade {
    pub const REST: Self = Self { out: 0.0, land: 0.0, dur: 0.0, from: 1.0, to: 1.0 };

    pub fn fade_in(land: f32, dur: f32) -> Self {
        Self { out: land, land, dur, from: 0.0, to: 1.0 }
    }

    pub fn fade_out(from: f32, out: f32, dur: f32) -> Self {
        Self { out, land: out + dur, dur, from, to: 0.0 }
    }

    pub fn out_in(from: f32, out: f32, land: f32, dur: f32) -> Self {
        Self { out, land, dur, from, to: 1.0 }
    }

    pub fn alpha(&self, now: f64) -> f32 {
        let now = now as f32;
        let ramp = |start| if self.dur > 0.0 {
            ease_out(((now - start) / self.dur) as f64) as f32
        } else if now < start { 0.0 } else { 1.0 };
        if now < self.land { self.from * (1.0 - ramp(self.out)) }
        else { self.to * ramp(self.land) }
    }

    pub fn end(&self) -> f64 {
        self.out.max(self.land) as f64 + self.dur.max(0.0) as f64
    }
}

#[derive(Clone, Debug)]
pub struct Text {
    pub text: String,
    pub at: [f64; 2],
    pub size: f64,
    pub scale: f64,
    pub hidden: bool,
    pub align: Align,
    /// reads bottom to top, along a vertical pin
    pub vertical: bool,
    pub color: [f32; 4],
    /// SheetRows row, or NO_ITEM.
    pub item: u32,
    /// Component node for motion; non-part text has no moving owner.
    pub owner: Option<u32>,
    pub is_value: bool,
    pub fade: TextFade,
}

impl Text {
    pub fn alpha(&self, now: f64) -> f32 {
        self.fade.alpha(now)
    }
}

struct ValueHit {
    bounds: [f64; 4],
    text: usize,
}

/// Text indices and original geometry for one part label, in sheet-label order.
pub struct PartTexts {
    pub label: usize,
    pub refdes: usize,
    pub value: Option<usize>,
    refdes_at: [f64; 2],
    value_at: Option<[f64; 2]>,
    value_bounds: Option<[f64; 4]>,
}

/// A placed part as a semantic lookup sees it ([`Scene::lookup_parts`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LookupPart {
    pub sel: Sel,
    pub name: String,
    pub value: Option<String>,
    /// The body as drawn, world units; None while the part is hidden.
    pub body: Option<[f64; 4]>,
}

#[derive(Clone)]
struct PartHit {
    node: u32,
    item: u32,
    body: [f64; 4],
    name: String,
    value: Option<String>,
    at: [f64; 2],
    motion: Flight,
}

#[derive(Clone)]
pub(super) struct WireHit {
    item: u32,
    /// Circuit net; bus runs and entry strokes are not probe targets.
    pub(super) net: Option<u32>,
    pub(super) a: [f64; 2],
    pub(super) b: [f64; 2],
    /// Sheet run and the segment containing this piece's start; absent for bus/entry strokes.
    pub(super) run: Option<u32>,
    pub(super) seg: u32,
}

/// Every placed pin's own line in world coordinates, including glyph and unconnected pins.
#[derive(Clone)]
pub(super) struct PinHit {
    pub(super) node: u32,
    pub(super) pin: u32,
    pub(super) outer: [f64; 2],
    pub(super) inner: [f64; 2],
    pub(super) net: Option<u32>,
    pub(super) nc: bool,
    /// The pin's net is one the document did not name, on this pin only: no wire leaves its end.
    pub(super) open_end: bool,
}

#[derive(Clone)]
pub(super) struct TagHit {
    pub(super) net: u32,
    pub(super) kind: sheet::TagKind,
    pub(super) bx: [f64; 4],
    pub(super) at: [f64; 2],
}

/// One visible pin end of a net ([`Scene::net_pins`]): where a probe's flag can anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetPin {
    /// The part node (its circuit component) and the pin's index in the part.
    pub node: u32,
    pub pin: u32,
    /// The pin's end and its direction out of the part, posed, world units.
    pub at: [f64; 2],
    pub out: [f64; 2],
    /// The part's body box, posed.
    pub body: [f64; 4],
}

/// A connected pin end in its part's resting pose.
#[derive(Clone)]
struct ConnectedPinHit {
    node: u32,
    at: [f64; 2],
    net: u32,
    pin: usize,
}

/// Picking and unfaded opacity parallel to the scene's mini instances.
#[derive(Clone)]
struct MiniHit {
    /// Graph net represented by this strip or bundle member.
    net: u32,
    /// Full strip box or member band, including the gap between drawn bundle rows.
    bounds: [f64; 4],
    /// Label opacity endpoints before a moving net is ghosted.
    opacity: [f32; 2],
}

#[derive(Clone, Default)]
pub(super) struct HitTable {
    parts: Vec<PartHit>,
    pub(super) pins: Vec<PinHit>,
    pub(super) tags: Vec<TagHit>,
    // Keep the connectivity/probe ordering independent of the complete edit-pin list.
    connected_pins: Vec<ConnectedPinHit>,
    pub(super) wires: Vec<WireHit>,
    /// One full picking band per subscribed mini instance.
    minis: Vec<MiniHit>,
    /// Title strips: (block row, strip, block path).
    frames: Vec<(u32, [f64; 4], String)>,
}

/// Static ink at frozen part and connection endpoints, below the live sheet.
pub(super) struct GhostRows {
    pub(super) boxes: Vec<BoxInst>,
    pub(super) lines: Vec<LineInst>,
    pub(super) arcs: Vec<ArcInst>,
    pub(super) pins: Vec<PinInst>,
}

/// Pure world-space data. Rows are ordered within each box, line, arc and mini layer.
#[derive(Default)]
pub struct Scene {
    pub lines: Vec<LineInst>,
    /// Pin ownership at each connection stroke's endpoints (component glyphs excluded).
    line_pins: Vec<[Option<usize>; 2]>,
    pin_nodes: Vec<Vec<u32>>,
    /// Label leaders: drawn only while their labels' text is (`text_shown`).
    pub leaders: Vec<LineInst>,
    /// In paint order: opaque frame grounds, part underlays, then frame outlines and symbol ink.
    pub boxes: Vec<BoxInst>,
    /// The underlay rows within `boxes`; the main sheet retains them separately during a drag.
    pub(super) glow_range: std::ops::Range<usize>,
    pub arcs: Vec<ArcInst>,
    pub bridges: Vec<BridgeInst>,
    /// Subscribed trace strips and bundle members, in label then member order.
    pub minis: Vec<MiniInst>,
    /// MCU signal-pin marks with static firmware texture rows.
    pub pins: Vec<PinInst>,
    pub texts: Vec<Text>,
    pub part_texts: Vec<PartTexts>,
    pub(super) hits: Arc<HitTable>,
    part_index: Vec<Option<usize>>,
    detail: Option<Flight>,
    selections: HashMap<u32, Sel>,
    items: HashMap<Sel, u32>,
    values: HashMap<u32, ValueHit>,
    bounds: [f64; 4],
    settle: f64,
}

impl Scene {
    pub fn build(design: &Design, unit: usize, sheet: &SheetLayout, rows: &RowIndex, tokens: &Tokens, state: &SceneState, live: &SceneLive) -> Scene {
        let mut scene = Self { bounds: sheet.bounds.map(f64::from), ..Self::default() };
        let Some(unit) = design.units.get(unit) else { return scene };
        let circuit = &unit.circuit;
        let mut hits = HitTable::default();
        let mut pinned_paths: HashSet<&str> = HashSet::new();
        for (path, hint) in &circuit.hints.parts {
            if hint.at.is_some() { pinned_paths.insert(path.as_str()); }
            else { pinned_paths.remove(path.as_str()); }
        }
        let board = unit.id;
        let net_row = |net: u32| live.nets.get(net as usize).copied().unwrap_or(-1.0);
        scene.part_index.resize(circuit.comps.len(), None);
        let mut pins: HashMap<[i32; 2], usize> = HashMap::new();
        scene.detail = state.fly.as_ref().map(|fly| fly.detail);
        for n in 0..circuit.comps.len() {
            scene.select(rows.comp(n as u32).unwrap_or(NO_ITEM), Sel::Comp(CompRef { board, comp: CompId(n as u32) }));
        }
        for k in 0..circuit.nets.len() {
            scene.select(rows.net(k as u32).unwrap_or(NO_ITEM), Sel::Net(NetRef { board, net: NetId(k as u32) }));
        }
        for (k, bus) in circuit.buses.iter().enumerate() {
            if let Some((_, net)) = bus.members.first() {
                scene.select(rows.bus(k as u32).unwrap_or(NO_ITEM), Sel::Net(NetRef { board, net: *net }));
            }
        }
        for b in 0..circuit.tree.len() {
            scene.select(rows.block(b as u32).unwrap_or(NO_ITEM), Sel::Block(board, BlockId(b as u32)));
        }
        let fade = state.assemble_at.map_or((0.0, 0.0), |t| ((t + 0.45) as f32, 0.2));
        let (fade, opacity) = state.fly.as_ref().map_or((fade, [0.0, 1.0]), |fly| {
            ((fly.detail.span[0], fly.detail.span[1]), [fly.detail.from[3], fly.detail.to[3]])
        });
        let detail_fade = if fade.1 <= 0.0 {
            TextFade { from: opacity[1], to: opacity[1], ..TextFade::REST }
        } else if opacity[1] == 0.0 {
            TextFade::fade_out(opacity[0], fade.0, fade.1)
        } else {
            TextFade { to: opacity[1], ..TextFade::fade_in(fade.0, fade.1) }
        };

        // Resolve flights and pin geometry before publishing any rows. Each symbol's ports
        // are consumed here once, for both connectivity and the unchanged picking tables.
        let mut flights = vec![None; circuit.comps.len()];
        let mut pin_marks = vec![0..0; circuit.comps.len()];
        // A net the document did not name, on one pin only (a placed part's open end), draws no
        // wire: that pin's end is no wire end to grab, so a click or drag there starts a wire
        // from the pin. One pass over the connections, O(pins).
        let mut pins_on = vec![0u32; circuit.nets.len()];
        for comp in &circuit.comps {
            for (_, net) in &comp.conns {
                if let Some(count) = pins_on.get_mut(net.ix()) { *count += 1; }
            }
        }
        let open_end = |net: u32| pins_on.get(net as usize) == Some(&1)
            && circuit.nets.get(net as usize).is_some_and(|net| net.scope.is_some());
        for (k, (n, p)) in sheet.parts.iter().enumerate().filter(|(_, p)| p.placed).enumerate() {
            let Some(comp) = circuit.comps.get(n) else { continue };
            flights[n] = state.fly.as_ref().and_then(|fly| fly.parts.get(n)).copied().flatten()
                .or_else(|| state.assemble_at.map(|t0| {
                    let center = sheet.frames.get(p.block as usize).filter(|f| f.placed).map_or(
                        p.at.map(f64::from),
                        |f| [(f.rect[0] as f64 + f.rect[2] as f64) * 0.5, (f.rect[1] as f64 + f.rect[3] as f64) * 0.5],
                    );
                    Flight {
                        from: [(center[0] - p.at[0] as f64) as f32, (center[1] - p.at[1] as f64) as f32, 0.0, 0.0],
                        to: [0.0, 0.0, 0.0, 1.0],
                        span: [(t0 + STATE_SECONDS + 0.01 * (k % 24) as f64) as f32, REVEAL_SECONDS as f32],
                    }
                }));
            let xf = Xf { at: p.at.map(|v| v as f32), turn: p.turn, mirror: p.mirror };
            let (_, ports) = fab_elec_explore::adapt::symbol_geometry(&comp.part, p.quarter);
            let pin_start = scene.pins.len();
            for (pin, port) in ports.iter().enumerate() {
                let at = xf.point(port.at.map(|v| v as f32));
                let net = comp.conns.iter().find(|(index, _)| index.ix() == pin).map(|(_, net)| net.0);
                let inward = match port.side {
                    Side::Left => [1, 0], Side::Right => [-1, 0],
                    Side::Top => [0, -1], Side::Bottom => [0, 1],
                };
                let inner = xf.point([(port.at[0] + inward[0]) as f32, (port.at[1] + inward[1]) as f32]);
                if let Some(&live_pin) = live.pins.get(n).and_then(|pins| pins.get(pin)).filter(|&&row| row >= 0.0) {
                    if comp.part.pins.get(pin).is_some_and(|pin| !matches!(pin.kind, PinKind::PowerIn | PinKind::PowerOut | PinKind::NoConnect)) {
                        scene.pins.push(PinInst {
                            c: common(rows.comp(n as u32).unwrap_or(NO_ITEM), tokens.ink),
                            at, dir: [at[0] - inner[0], at[1] - inner[1]], size: 0.22, live_pin,
                        });
                    }
                }
                hits.pins.push(PinHit {
                    node: n as u32, pin: pin as u32, outer: at.map(f64::from), inner: inner.map(f64::from),
                    net, nc: comp.nc.iter().any(|index| index.ix() == pin),
                    open_end: net.is_some_and(open_end),
                });
                if let Some(net) = net {
                    hits.connected_pins.push(ConnectedPinHit { node: n as u32, at: at.map(f64::from), net, pin });
                }
                let point = at.map(|v| v.round() as i32);
                let index = *pins.entry(point).or_insert_with(|| {
                    scene.pin_nodes.push(Vec::new());
                    scene.pin_nodes.len() - 1
                });
                if !scene.pin_nodes[index].contains(&(n as u32)) {
                    scene.pin_nodes[index].push(n as u32);
                }
            }
            pin_marks[n] = pin_start..scene.pins.len();
        }
        let motion = state.fly.as_ref().filter(|fly| fly.leg != Leg::Settle)
            .map(|fly| (fly.leg, fly.start, fly.continued, fly.text_from.as_slice()))
            .or_else(|| state.assemble_at.map(|start| (Leg::Arrive, start, false, &[][..])));
        // Empty tables at rest and during drag reflow: no group or spanning work there.
        let mut runs = Vec::new();
        let mut junctions = Vec::new();
        let mut tags = Vec::new();
        let mut still = Flight::REST;
        if let Some((leg, start, continued, _)) = motion {
            let at: Vec<_> = hits.connected_pins.iter().map(|pin| (pin.net, pin.at.map(|v| v.round() as i32))).collect();
            let groups = wire_groups(sheet, &at);
            let nodes: Vec<Vec<u32>> = groups.iter().map(|group| {
                let mut nodes: Vec<_> = group.pins.iter().map(|&pin| hits.connected_pins[pin as usize].node).collect();
                nodes.sort_unstable();
                nodes.dedup();
                nodes
            }).collect();
            if state.fly.as_ref().is_some_and(|fly| fly.leg != Leg::Settle) {
                join_flights(&mut flights, &nodes);
            }
            let landed = flights.iter().flatten().filter(|flight| moving(**flight))
                .map(Flight::end).reduce(f64::max).unwrap_or(start);
            if leg == Leg::Leave {
                still.to[3] = 0.0;
                still.span = [(landed - STATE_SECONDS) as f32, STATE_SECONDS as f32];
            }
            runs.resize(sheet.wires.len(), None);
            junctions.resize(sheet.junctions.len(), None);
            tags.resize(sheet.tags.len(), None);
            for (group, nodes) in groups.iter().zip(&nodes) {
                let Some(&node) = nodes.first() else { continue };
                let flight = flights[node as usize].unwrap_or(Flight::REST);
                let rigid = nodes.iter().all(|&node| same_flight(flight, flights[node as usize].unwrap_or(Flight::REST)));
                let landed = nodes.iter().filter_map(|&node| flights[node as usize]).map(|f| f.end())
                    .reduce(f64::max).unwrap_or(start);
                let route = if rigid {
                    if moving(flight) { flight } else { still }
                } else if leg == Leg::Arrive {
                    Flight { from: [0.0; 4], span: [landed as f32, STATE_SECONDS as f32], ..Flight::REST }
                } else {
                    state.fly.as_ref().map_or(Flight::REST, |fly| fly.detail)
                };
                for &run in &group.runs { runs[run as usize] = Some(route); }
                for &junction in &group.junctions { junctions[junction as usize] = Some(route); }
                for &tag in &group.tags {
                    let at = sheet.tags[tag as usize].at.map(f64::from);
                    let distance = |pin: u32| {
                        let p = hits.connected_pins[pin as usize].at;
                        (p[0] - at[0]).abs() + (p[1] - at[1]).abs()
                    };
                    tags[tag as usize] = group.pins.iter().copied().min_by(|&a, &b| {
                        distance(a).total_cmp(&distance(b)).then(a.cmp(&b))
                    }).map(|pin| hits.connected_pins[pin as usize].node);
                }
                if rigid { continue; }
                let takeoff = nodes.iter().filter_map(|&node| flights[node as usize]).filter(|flight| moving(*flight))
                    .map(|flight| flight.span[0]).reduce(f32::min).unwrap_or(start as f32);
                let points: Vec<_> = group.pins.iter().map(|&pin| hits.connected_pins[pin as usize].at).collect();
                let owners: Vec<_> = group.pins.iter().map(|&pin| hits.connected_pins[pin as usize].node).collect();
                let fade = if leg == Leg::Arrive && continued { [start as f32, 0.0] }
                    else { [if leg == Leg::Leave { start as f32 } else { takeoff }, STATE_SECONDS as f32] };
                for [i, j] in spanning(&points, &owners) {
                    let a = points[i as usize];
                    let b = points[j as usize];
                    let mut c = common(rows.net(group.net).unwrap_or(NO_ITEM), net_ink(circuit, group.net, tokens));
                    flights[owners[i as usize] as usize].unwrap_or(Flight::REST).apply(&mut c);
                    c.pose_a[3] = 1.0;
                    c.pose_b[3] = 1.0;
                    c.live_ix = [net_row(group.net), -1.0, -1.0, 0.0];
                    let flight = flights[owners[j as usize] as usize].unwrap_or(Flight::REST);
                    scene.bridges.push(BridgeInst {
                        c, ends: [a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32],
                        b_pose: [flight.from[0], flight.from[1], flight.to[0], flight.to[1]], b_span: flight.span,
                        bend: BRIDGE_BEND * if b[0] >= a[0] { 1.0 } else { -1.0 }, stroke: [WIRE_W, 0.0],
                        fade: [fade[0], fade[1], landed as f32, STATE_SECONDS as f32],
                    });
                }
            }
        }
        let text_fade = |owner: Option<u32>| {
            let (Some((leg, start, _, text_from)), Some(node)) = (motion, owner) else { return detail_fade };
            let flight = flights[node as usize].unwrap_or(Flight::REST);
            if moving(flight) {
                if leg == Leg::Arrive {
                    TextFade::out_in(text_from.get(node as usize).copied().unwrap_or(0.0),
                        flight.span[0], flight.end() as f32, STATE_SECONDS as f32)
                } else { TextFade::fade_out(1.0, start as f32, STATE_SECONDS as f32) }
            } else if leg == Leg::Leave {
                TextFade::fade_out(1.0, still.span[0], STATE_SECONDS as f32)
            } else { TextFade::REST }
        };

        let mut foreground = Vec::new();
        for (b, frame) in sheet.frames.iter().enumerate().skip(1).filter(|(_, f)| f.placed) {
            let item = rows.block(b as u32).unwrap_or(NO_ITEM);
            let rect = frame.rect.map(f64::from);
            let c = fading(state, item, tokens.block_frame, 0.0, STATE_SECONDS);
            let frame_box = BoxInst {
                c,
                rect: [
                    ((rect[0] + rect[2]) * 0.5) as f32,
                    ((rect[1] + rect[3]) * 0.5) as f32,
                    ((rect[2] - rect[0]) * 0.5) as f32,
                    ((rect[3] - rect[1]) * 0.5) as f32,
                ],
                corner: 1.0,
                stroke: [1.0, 0.0],
                fill: tokens.block_fill,
            };
            scene.boxes.push(BoxInst { stroke: [0.0; 2], ..frame_box });
            if frame.planned {
                let [l, b, r, t] = frame.rect.map(|v| v as f32);
                for seg in [[l,b,r,b], [r,b,r,t], [r,t,l,t], [l,t,l,b]] {
                    scene.lines.push(LineInst { c, seg, width: [1.0, 0.0], dash: [3.0, 2.0], ..Default::default() });
                }
            } else {
                foreground.push(BoxInst { fill: [0.0; 4], ..frame_box });
            }
            for port in &frame.ports {
                let [x, y] = port.at.map(|v| v as f32);
                foreground.push(BoxInst { c, rect: [x, y, 0.2, 0.2], corner: 0.2,
                    stroke: [1.0, 0.0], fill: if port.open { tokens.block_fill } else { c.ink } });
            }
            let path = circuit.tree.get(b).map_or(String::new(), |block| block.path.clone());
            if pinned_paths.contains(path.as_str()) {
                let mut fill = tokens.selection;
                fill[3] = 0.9;
                foreground.push(BoxInst {
                    c: Common { ink: tokens.selection, live_ix: [-1.0; 4], ..c },
                    rect: [rect[0] as f32, rect[3] as f32, 0.35, 0.35],
                    corner: 0.35,
                    stroke: [0.0, 0.0],
                    fill,
                });
            }
            hits.frames.push((item, [rect[0], (rect[3] - f64::from(frame.title)).max(rect[1]), rect[2], rect[3]], path));
        }

        let grounds = scene.boxes.len();
        let mut glows = Vec::new();
        scene.boxes.append(&mut foreground);
        for (n, p) in sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
            let Some(comp) = circuit.comps.get(n) else { continue };
            let item = rows.comp(n as u32).unwrap_or(NO_ITEM);
            let xf = Xf { at: p.at.map(|v| v as f32), turn: p.turn, mirror: p.mirror };
            let mut c = common(item, tokens.ink);
            if let Some(flight) = flights[n] {
                flight.apply(&mut c);
            }
            // Grouped flights are final now: marks carry exactly the pin stubs' common fields.
            for pin in &mut scene.pins[pin_marks[n].clone()] { pin.c = c; }
            let family = if p.glyph { family(&comp.part.kind) } else { None };
            // A box symbol is drawn in the fit its pose's geometry was laid out with.
            let fitted = fab_elec_explore::adapt::sheet_unit(&comp.part, p.quarter);
            let unit = fitted.as_ref().or(comp.part.symbol.units.first());
            let (prims, pin_texts) = if let Some(family) = family {
                (glyphs::family_prims(family), Vec::new())
            } else if let Some(unit) = unit {
                glyphs::symbol_prims(unit, |i| {
                    let pin = &comp.part.pins[i];
                    (pin.name.clone(), pin.pads.join(","))
                })
            } else {
                (Vec::new(), Vec::new())
            };
            let row = live.parts.get(n).copied().filter(|row| *row >= 0.0);
            let emit = comp.part.emitter.as_ref().map_or(0.0, |e| crate::canvas::emitter::emitter_ix(&e.light));
            // A lit LED's strokes take its light's colour by its brightness (`live_emit`).
            let ink_c = match row {
                Some(row) if emit > 0.0 => Common { live_ix: [-1.0, -1.0, row, emit], ..c },
                _ => c,
            };
            let mut bounds = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
            for prim in prims {
                let prim = xf.prim(&prim);
                if row.is_some() { extend_symbol_bounds(&mut bounds, &prim); }
                scene.prim(prim, ink_c, tokens);
            }
            if let Some(row) = row {
                let [x0, y0, x1, y1] = if bounds[0] <= bounds[2] { bounds } else { p.body.map(|v| v as f32) };
                if emit > 0.0 {
                    // An LED's light: a round bloom on the diode's body (the family glyph's origin,
                    // else the symbol's middle), fading out at twice the symbol's size from it.
                    let at = if family == Some(Family::Led) { xf.point([0.0, 0.0]) } else { [(x0+x1)*0.5, (y0+y1)*0.5] };
                    let r = ((x1-x0).max(y1-y0)*0.5).max(0.5);
                    glows.push(BoxInst::glow(c, [at[0], at[1], r, r], 3.0*r, row, emit));
                } else {
                    // One sheet unit is the pin connection grid step (SchPt::GRID).
                    glows.push(BoxInst::glow(c, [(x0+x1)*0.5, (y0+y1)*0.5, (x1-x0)*0.5, (y1-y0)*0.5], 0.5, row, emit));
                }
            }
            if pinned_paths.contains(comp.path.as_str()) {
                let mut fill = tokens.selection;
                fill[3] = 0.9;
                // The mark rides the part's row, including its flight, fades and drag copy.
                scene.boxes.push(BoxInst {
                    c: Common { ink: tokens.selection, live_ix: [-1.0; 4], ..c },
                    rect: [p.body[0] as f32, p.body[3] as f32, 0.35, 0.35],
                    corner: 0.35,
                    stroke: [0.0, 0.0],
                    fill,
                });
            }
            let body = unit.and_then(glyphs::symbol_body);
            for pin in glyphs::pose_pin_texts(&pin_texts, body, &xf) {
                scene.texts.push(Text {
                    text: pin.text,
                    at: pin.at.map(f64::from),
                    size: pin.size as f64,
                    scale: 1.0,
                    hidden: false,
                    align: pin.align,
                    vertical: false,
                    color: match pin.kind { TextKind::PinNumber => tokens.ink_dim, TextKind::PinName => tokens.ink },
                    item,
                    owner: Some(n as u32),
                    is_value: false,
                    fade: text_fade(Some(n as u32)),
                });
            }
            scene.part_index[n] = Some(hits.parts.len());
            hits.parts.push(PartHit {
                node: n as u32,
                item,
                body: p.body.map(f64::from),
                name: comp.refdes.clone(),
                value: comp.value.as_ref().map(|v| v.text.clone()),
                at: p.at.map(f64::from),
                motion: Flight { from: c.pose_a, to: c.pose_b, span: c.span },
            });
        }

        scene.glow_range = grounds..grounds+glows.len();
        scene.boxes.splice(grounds..grounds, glows);

        for (i, run) in sheet.wires.iter().enumerate() {
            let item = rows.net(run.net).unwrap_or(NO_ITEM);
            let mut c = fading(state, item, net_ink(circuit, run.net, tokens), 0.35, REVEAL_SECONDS);
            if let Some(flight) = runs.get(i).copied().flatten() { flight.apply(&mut c); }
            c.live_ix = [net_row(run.net), -1.0, -1.0, 0.0];
            if let Some(pieces) = live.runs.get(i).filter(|pieces| !pieces.is_empty()) {
                let mut along = 0.0f64;
                let mut seg = 0;
                for &(a, b, row) in pieces {
                    // Pieces follow the run. At a shared vertex, the next segment owns the
                    // start; one live piece can also cover several collinear sheet segments.
                    while seg + 2 < run.pts.len() {
                        let [p, q] = [run.pts[seg], run.pts[seg + 1]];
                        let bx = [p[0].min(q[0]), p[1].min(q[1]), p[0].max(q[0]), p[1].max(q[1])];
                        if a != q && contains(bx.map(f64::from), a.map(f64::from), 0.0) { break; }
                        seg += 1;
                    }
                    c.live_ix[1] = row;
                    let a = a.map(f64::from);
                    let b = b.map(f64::from);
                    scene.lines.push(LineInst {
                        c, seg: [a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32],
                        width: [WIRE_W, 0.0], along0: along as f32, ..LineInst::default()
                    });
                    hits.wires.push(WireHit { item, net: Some(run.net), a, b, run: Some(i as u32), seg: seg as u32 });
                    along += (b[0] - a[0]).hypot(b[1] - a[1]);
                }
            } else {
                let first = hits.wires.len();
                scene.run(&mut hits, run, c, WIRE_W, false, Some(run.net));
                for hit in &mut hits.wires[first..] { hit.run = Some(i as u32); }
            }
        }
        for run in &sheet.buses {
            let c = fading(state, rows.bus(run.net).unwrap_or(NO_ITEM), tokens.wire_bus, 0.35, REVEAL_SECONDS);
            scene.run(&mut hits, run, c, BUS_W, true, None);
        }
        for (j, entry) in sheet.entries.iter().enumerate() {
            let item = rows.net(entry.net).unwrap_or(NO_ITEM);
            let a = entry.at.map(|v| v as f32);
            let b = entry.to.map(|v| v as f32);
            let mut c = fading(state, item, tokens.wire_signal, 0.35, REVEAL_SECONDS);
            c.live_ix = [net_row(entry.net), live.entries.get(j).copied().unwrap_or(-1.0), -1.0, 0.0];
            scene.prim(
                Prim::Line { a, b, w: WIRE_W, butt: false, pressed: None },
                c, tokens,
            );
            hits.wires.push(WireHit { item, net: None, a: entry.at.map(f64::from), b: entry.to.map(f64::from), run: None, seg: 0 });
        }
        for (j, &(net, at)) in sheet.junctions.iter().enumerate() {
            let mut c = fading(state, rows.net(net).unwrap_or(NO_ITEM), net_ink(circuit, net, tokens), 0.35, REVEAL_SECONDS);
            if let Some(flight) = junctions.get(j).copied().flatten() { flight.apply(&mut c); }
            c.live_ix = [net_row(net), -1.0, -1.0, 0.0];
            scene.prim(
                glyphs::junction(at.map(|v| v as f32)),
                c, tokens,
            );
        }
        for (j, tag) in sheet.tags.iter().enumerate() {
            if let Some(net) = tag.net.filter(|_| tag.kind != sheet::TagKind::NoConnect) {
                hits.tags.push(TagHit { net, kind: tag.kind, bx: sheet::tag_item_box(tag), at: tag.at.map(f64::from) });
            }
            let (kind, ink) = match tag.kind {
                sheet::TagKind::Power => (glyphs::TagKind::Power, tokens.wire_power),
                sheet::TagKind::Ground => (glyphs::TagKind::Ground, tokens.wire_ground),
                sheet::TagKind::Label => (glyphs::TagKind::Label { text_w: sheet::text_w(&tag.text, sheet::TEXT_NET) as f32 }, tokens.ink_dim),
                sheet::TagKind::NoConnect => (glyphs::TagKind::NoConnect, tokens.ink_dim),
            };
            let item = tag.net.and_then(|net| rows.net(net)).unwrap_or(NO_ITEM);
            let mut c = fading(state, item, ink, 0.35, REVEAL_SECONDS);
            let owner = tags.get(j).copied().flatten();
            if let Some(node) = owner {
                let flight = flights[node as usize].unwrap_or(Flight::REST);
                (if moving(flight) { flight } else { still }).apply(&mut c);
            }
            if tag.kind != sheet::TagKind::NoConnect {
                c.live_ix = [tag.net.map_or(-1.0, net_row), -1.0, -1.0, 0.0];
            }
            let (prims, text) = glyphs::tag_prims(kind, tag.at.map(|v| v as f32), tag.dir, tag.band, tag.side);
            for prim in prims {
                scene.prim(prim, c, tokens);
            }
            if let Some((at, align)) = text {
                // A name flag's name is the information itself: full ink, whatever its box's ink.
                let color = if tag.kind == sheet::TagKind::Label { tokens.ink } else { ink };
                scene.texts.push(Text { text: tag.text.clone(), at: at.map(f64::from), size: sheet::TEXT_NET,
                    scale: 1.0, hidden: false,
                    align, vertical: tag.kind == sheet::TagKind::Label && tag.dir[1] != 0,
                    color, item, owner, is_value: false, fade: text_fade(owner) });
            }
        }
        for (label_index, label) in sheet.labels.iter().enumerate() {
            if matches!(label.owner, Owner::Trace(_) | Owner::Bundle(_)) {
                for (i, net) in mini_nets(circuit, label.owner).enumerate() {
                    let slot = live.mini.get(net as usize).copied().unwrap_or(-1.0);
                    if slot < 0.0 { continue; }
                    let mut bounds = label.bx;
                    let bundle = matches!(label.owner, Owner::Bundle(_));
                    if bundle {
                        // World y points up: member zero is the top band, below the padding.
                        bounds[3] -= 0.2 + i as f64 * sheet::BUNDLE_ROW;
                        bounds[1] = bounds[3] - sheet::BUNDLE_ROW;
                    }
                    let mut c = common(owner_item(Owner::Net(net), rows), tokens.wire_signal);
                    c.live_ix[0] = net_row(net);
                    c.pose_a[3] = opacity[0];
                    c.pose_b[3] = opacity[1];
                    c.span = [fade.0, fade.1];
                    scene.minis.push(MiniInst {
                        c,
                        rect: [((bounds[0] + bounds[2]) * 0.5) as f32,
                            ((bounds[1] + bounds[3]) * 0.5) as f32,
                            ((bounds[2] - bounds[0]) * 0.5) as f32,
                            if bundle { (0.4 * sheet::BUNDLE_ROW) as f32 } else { ((bounds[3] - bounds[1]) * 0.5) as f32 }],
                        slot, stroke: [1.25, 0.0],
                        fill: [tokens.ink[0], tokens.ink[1], tokens.ink[2], 0.06],
                    });
                    hits.minis.push(MiniHit { net, bounds, opacity });
                }
                continue;
            }
            let item = owner_item(label.owner, rows);
            let owner = match label.owner { Owner::Part(node) => Some(node), _ => None };
            if matches!(label.lines.as_slice(), [(name, TextRole::Refdes)] if name.starts_with('#')) {
                continue;
            }
            let refdes = scene.texts.len();
            let mut heights = 0.0;
            for (i, (text, role)) in label.lines.iter().enumerate() {
                let (height, color) = match role {
                    TextRole::Refdes => (sheet::TEXT_REFDES, tokens.ink),
                    TextRole::Value => (sheet::TEXT_VALUE, tokens.ink_dim),
                    TextRole::Net => (sheet::TEXT_NET, tokens.wire_signal),
                    TextRole::Title => (sheet::TEXT_TITLE, tokens.ink_dim),
                };
                let y = label.bx[3] - heights - i as f64 * LINE_GAP * height - height * 0.5;
                scene.texts.push(Text {
                    text: text.clone(), at: [label.bx[0], y], size: height,
                    scale: 1.0, hidden: *role == TextRole::Refdes && text.starts_with('#'),
                    align: Align::Left, vertical: false, color, item, owner, fade: text_fade(owner),
                    is_value: matches!((label.owner, role), (Owner::Part(_), TextRole::Value)),
                });
                if let (Owner::Part(node), TextRole::Value) = (label.owner, role) {
                    scene.values.insert(node, ValueHit {
                        bounds: [label.bx[0], y - height * 0.5, label.bx[0] + sheet::text_w(text, height), y + height * 0.5],
                        text: scene.texts.len() - 1,
                    });
                }
                heights += height;
            }
            if let Some(node) = owner {
                let value = scene.values.get(&node);
                scene.part_texts.push(PartTexts {
                    label: label_index, refdes, value: value.map(|hit| hit.text),
                    refdes_at: scene.texts[refdes].at,
                    value_at: value.map(|hit| scene.texts[hit.text].at),
                    value_bounds: value.map(|hit| hit.bounds),
                });
            }
            if let Some([a, b]) = label.leader {
                scene.leaders.push(LineInst {
                    c: fading(state, item, tokens.ink_dim, 0.35, REVEAL_SECONDS),
                    seg: [a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32],
                    width: [1.0, 0.0],
                    ..LineInst::default()
                });
            }
        }
        scene.line_pins = scene.lines.iter().map(|line| {
            if line.c.item >= 0.0 && scene.part_node(line.c.item as u32).is_some() {
                return [None; 2];
            }
            [[line.seg[0], line.seg[1]], [line.seg[2], line.seg[3]]].map(|at| {
                let grid = at.map(|v| v.round() as i32);
                (at == grid.map(|v| v as f32)).then(|| pins.get(&grid).copied()).flatten()
            })
        }).collect();
        scene.settle = scene.lines.iter().chain(scene.leaders.iter()).map(|i| i.c.span)
            .chain(scene.boxes.iter().map(|i| i.c.span))
            .chain(scene.arcs.iter().map(|i| i.c.span))
            .chain(scene.bridges.iter().map(|i| i.c.span))
            .chain(scene.minis.iter().map(|i| i.c.span))
            .map(|s| s[0] as f64 + s[1] as f64)
            .chain(scene.bridges.iter().map(|i| i.fade[2] as f64 + i.fade[3] as f64))
            .chain(scene.texts.iter().map(|t| t.fade.end()))
            .fold(0.0, f64::max);
        // One shared table per scene; a press only clones its Arc.
        scene.hits = Arc::new(hits);
        scene
    }

    /// Apply enlarged part labels and their picking bounds; scale one restores the layout.
    pub fn apply_legible(&mut self, legible: &[Legible], scale: f64) {
        for entry in legible {
            let Ok(index) = self.part_texts.binary_search_by_key(&entry.label, |part| part.label) else { continue };
            let part = &self.part_texts[index];
            let refdes = &mut self.texts[part.refdes];
            let at = if scale == 1.0 { Some(part.refdes_at) }
                else { entry.refdes.map(|b| [b[0], (b[1] + b[3]) * 0.5]) };
            refdes.scale = scale;
            // A reference with no free spot beside its own part is not drawn at this scale.
            refdes.hidden = at.is_none() || refdes.text.starts_with('#');
            if let Some(at) = at { refdes.at = at; }
            let Some(value_index) = part.value else { continue };
            let value = &mut self.texts[value_index];
            let bounds = if scale == 1.0 { part.value_bounds } else { entry.value };
            value.scale = scale;
            value.hidden = bounds.is_none();
            if let Some(bounds) = bounds {
                value.at = if scale == 1.0 { part.value_at.expect("a value has its original position") }
                    else { [bounds[0], (bounds[1] + bounds[3]) * 0.5] };
                if let Some(node) = value.owner {
                    self.values.insert(node, ValueHit { bounds, text: value_index });
                }
            } else if let Some(node) = value.owner {
                self.values.remove(&node);
            }
        }
    }

    fn select(&mut self, item: u32, sel: Sel) {
        if item == NO_ITEM { return; }
        // A bus's selection is its first member; prefer that net's own id for hover.
        self.items.entry(sel.clone()).or_insert(item);
        self.selections.insert(item, sel);
    }

    /// Selection of a SheetRows row, not a component node.
    pub fn sel_of(&self, item: u32) -> Option<Sel> {
        self.selections.get(&item).cloned()
    }

    /// SheetRows row of a selection (the first member net's own row for a bus selection).
    pub fn item_of(&self, sel: &Sel) -> Option<u32> {
        self.items.get(sel).copied()
    }

    /// Component node of an item row, for geometry and value lookups.
    pub fn part_node(&self, item: u32) -> Option<u32> {
        match self.selections.get(&item)? {
            Sel::Comp(comp) => Some(comp.comp.0),
            _ => None,
        }
    }

    /// Block index of an item row, for frame lookups.
    pub fn block_node(&self, item: u32) -> Option<u32> {
        if item == NO_ITEM { return None; }
        match self.selections.get(&item)? {
            Sel::Block(_, block) => Some(block.0),
            _ => None,
        }
    }

    /// Item row of a placed component node.
    pub fn part_item(&self, node: u32) -> Option<u32> {
        self.part(node).map(|part| part.item).filter(|&item| item != NO_ITEM)
    }

    pub fn value_box(&self, node: u32) -> Option<[f64; 4]> {
        self.values.get(&node).map(|value| value.bounds)
    }

    /// Component node under a value label.
    pub fn value_at(&self, at: [f64; 2], radius: f64) -> Option<u32> {
        self.value_at_time(at, radius, f64::INFINITY)
    }

    /// Component node under a value label at its current flight position.
    pub fn value_at_time(&self, at: [f64; 2], radius: f64, now: f64) -> Option<u32> {
        let radius = radius.max(0.0);
        self.hits.parts.iter().rev().find_map(|part| {
            self.value_text(part.node).filter(|text| text.alpha(now) > 0.0)?;
            self.value_box_at(part.node, now).filter(|&bounds| contains(bounds, at, radius)).map(|_| part.node)
        })
    }

    pub fn value_box_at(&self, node: u32, now: f64) -> Option<[f64; 4]> {
        self.value_box(node).map(|bounds| shifted(bounds, self.offset(node, now)))
    }

    pub fn offset(&self, node: u32, now: f64) -> [f64; 2] {
        self.part(node).map_or([0.0; 2], |p| {
            let pose = p.motion.pose(now);
            [pose[0] as f64, pose[1] as f64]
        })
    }

    pub fn position(&self, node: u32, now: f64) -> Option<[f64; 2]> {
        let p = self.part(node)?;
        let pose = p.motion.pose(now);
        Some([p.at[0] + pose[0] as f64, p.at[1] + pose[1] as f64])
    }

    pub fn has_part(&self, node: u32) -> bool {
        self.part(node).is_some()
    }

    fn part(&self, node: u32) -> Option<&PartHit> {
        self.hits.parts.get((*self.part_index.get(node as usize)?)?)
    }

    pub fn value_text(&self, node: u32) -> Option<&Text> {
        self.texts.get(self.values.get(&node)?.text)
    }

    /// Refresh the model text during a scrub without republishing the sheet's geometry.
    /// The new layout is published when the gesture ends, keeping its value chip in place.
    pub fn update_values(&mut self, design: &Design, unit: usize) {
        let Some(unit) = design.units.get(unit) else { return };
        for (&node, hit) in &mut self.values {
            let value = unit.circuit.comps.get(node as usize).and_then(|comp| comp.value.as_ref());
            if let Some(text) = self.texts.get_mut(hit.text) {
                let value = value.map_or("", |value| value.text.as_str());
                if text.text != value {
                    text.text.clear();
                    text.text.push_str(value);
                    hit.bounds[2] = hit.bounds[0] + sheet::text_w(value, text.size * text.scale);
                }
            }
        }
        for part in &mut Arc::make_mut(&mut self.hits).parts {
            let value = unit.circuit.comps.get(part.node as usize).and_then(|comp| comp.value.as_ref());
            if part.value.as_deref() != value.map(|value| value.text.as_str()) {
                part.value = value.map(|value| value.text.clone());
            }
        }
    }

    /// Copy moved parts and their attached strokes from the retained committed scene.
    pub(super) fn ghost_rows(&self, moved: &[bool]) -> GhostRows {
        let belongs = |item: f32| item >= 0.0 && self.part_node(item as u32)
            .is_some_and(|node| moved.get(node as usize) == Some(&true));
        let common = |c: Common| Common {
            look: Look::Ghost.code(),
            pose_a: [0.0, 0.0, 0.0, 1.0], pose_b: [0.0, 0.0, 0.0, 1.0], span: [0.0; 2], ..c
        };
        GhostRows {
            boxes: self.boxes.iter().filter(|row| belongs(row.c.item))
                .map(|row| BoxInst { c: common(row.c), ..*row }).collect(),
            lines: self.lines.iter().zip(&self.line_pins).filter(|(row, ends)| belongs(row.c.item)
                || ends.iter().any(|pin| pin.is_some_and(|pin| self.pin_nodes[pin].iter()
                    .any(|&node| moved.get(node as usize) == Some(&true)))))
                .map(|(row, _)| LineInst { c: common(row.c), follow: [0.0; 2], ..*row }).collect(),
            arcs: self.arcs.iter().filter(|row| belongs(row.c.item))
                .map(|row| ArcInst { c: common(row.c), ..*row }).collect(),
            pins: self.pins.iter().filter(|row| belongs(row.c.item))
                .map(|row| PinInst { c: common(row.c), ..*row }).collect(),
        }
    }

    /// Recomputed only at gesture boundaries or when a layout publishes new routes.
    pub(super) fn follow(&mut self, members: &[bool]) {
        for (line, ends) in self.lines.iter_mut().zip(&self.line_pins) {
            line.follow = ends.map(|pin| {
                if pin.is_some_and(|pin| self.pin_nodes[pin].iter()
                    .any(|&node| members.get(node as usize) == Some(&true))) { 1.0 } else { 0.0 }
            });
        }
        // Like the moving parts' source texts, their nets' strips become ghosts until
        // the next layout or gesture boundary. Never rewrite instances on a clock tick.
        for (mini, hit) in self.minis.iter_mut().zip(&self.hits.minis) {
            let ghost = if self.hits.connected_pins.iter().any(|pin| pin.net == hit.net
                && members.get(pin.node as usize) == Some(&true)) { 0.33 } else { 1.0 };
            mini.c.pose_a[3] = hit.opacity[0] * ghost;
            mini.c.pose_b[3] = hit.opacity[1] * ghost;
        }
    }

    pub(super) fn body_at(&self, at: [f64; 2], radius: f64, now: f64) -> Option<u32> {
        self.hits.parts.iter().rev().find_map(|p| self.body_contains(p.node, at, radius, now).then_some(p.node))
    }

    pub(super) fn body_contains(&self, node: u32, at: [f64; 2], radius: f64, now: f64) -> bool {
        self.part(node).is_some_and(|p| p.motion.pose(now)[3] > 0.0
            && contains(shifted(p.body, self.offset(node, now)), at, radius))
    }

    /// What a probe's tag must not sit on, as world boxes `[x0, y0, x1, y1]` at time `now`, into
    /// `out`: every shown part's body, every pin's line, every wire piece, net tag and block title
    /// strip, and every shown text (its width by [`sheet::text_w`]). O(parts + pins + wires + texts).
    pub fn obstacles(&self, now: f64, out: &mut Vec<[f64; 4]>) {
        const LINE: f64 = 0.1;
        let seg = |a: [f64; 2], b: [f64; 2]| [a[0].min(b[0]) - LINE, a[1].min(b[1]) - LINE, a[0].max(b[0]) + LINE, a[1].max(b[1]) + LINE];
        let shown = |node: u32| self.part(node).map(|p| p.motion.pose(now)).filter(|pose| pose[3] > 0.0).map(|pose| [pose[0] as f64, pose[1] as f64]);
        out.clear();
        for part in &self.hits.parts {
            if let Some(d) = shown(part.node) { out.push(shifted(part.body, d)); }
        }
        for pin in &self.hits.pins {
            let Some(d) = shown(pin.node) else { continue };
            out.push(seg([pin.outer[0] + d[0], pin.outer[1] + d[1]], [pin.inner[0] + d[0], pin.inner[1] + d[1]]));
        }
        out.extend(self.hits.wires.iter().map(|wire| seg(wire.a, wire.b)));
        out.extend(self.hits.tags.iter().map(|tag| tag.bx));
        out.extend(self.hits.frames.iter().map(|(_, strip, _)| *strip));
        for text in self.texts.iter().filter(|text| !text.hidden && text.alpha(now) > 0.0) {
            let h = text.size * text.scale;
            let w = sheet::text_w(&text.text, h);
            let f = match text.align { Align::Left => 0.0, Align::Right => 1.0, Align::Center => 0.5 };
            let d = text.owner.map_or([0.0; 2], |node| self.offset(node, now));
            let at = [text.at[0] + d[0], text.at[1] + d[1]];
            out.push(if text.vertical {
                [at[0] - h * 0.5, at[1] - f * w, at[0] + h * 0.5, at[1] + (1.0 - f) * w]
            } else {
                [at[0] - f * w, at[1] - h * 0.5, at[0] + (1.0 - f) * w, at[1] + h * 0.5]
            });
        }
    }

    /// Every part's node and body `[x0, y0, x1, y1]` as laid out (no motion). O(parts).
    pub(super) fn part_bodies(&self) -> impl Iterator<Item = (u32, [f64; 4])> + '_ {
        self.hits.parts.iter().map(|part| (part.node, part.body))
    }

    pub fn part_box(&self, node: u32) -> Option<[f64; 4]> {
        self.part(node).map(|p| p.body)
    }

    pub fn bounds(&self) -> [f64; 4] {
        self.bounds
    }

    pub fn settle_at(&self) -> f64 {
        self.settle
    }

    fn prim(&mut self, prim: Prim, c: Common, tokens: &Tokens) {
        let fill = |f| match f { Fill::None => [0.0; 4], Fill::Body => tokens.part_fill, Fill::Ink => c.ink };
        match prim {
            Prim::Line { a, b, w, butt, pressed } => self.lines.push(LineInst {
                c, seg: [a[0], a[1], b[0], b[1]], width: [w, 0.0],
                dash: [0.0, 0.0], cap: if butt { 1.0 } else { 0.0 }, along0: 0.0, follow: [0.0; 2],
                press: pressed.map_or([0.0; 4], |[pa, pb]| [pa[0] - a[0], pa[1] - a[1], pb[0] - b[0], pb[1] - b[1]]),
            }),
            Prim::Box { c: center, half, corner, w, fill: f } => self.boxes.push(BoxInst {
                c, rect: [center[0], center[1], half[0], half[1]], corner, stroke: [w, 0.0], fill: fill(f),
            }),
            Prim::Arc { c: center, r, start, sweep, w, fill: f } => self.arcs.push(ArcInst {
                c, circle: [center[0], center[1], r, 0.0], sweep: [start, sweep],
                along0: 0.0, stroke: [w, 0.0], fill: fill(f),
            }),
        }
    }

    fn run(&mut self, hits: &mut HitTable, run: &Run, c: Common, width: f32, butt: bool, net: Option<u32>) {
        let mut along = 0.0f64;
        for (seg, pair) in run.pts.windows(2).enumerate() {
            let a = pair[0].map(f64::from);
            let b = pair[1].map(f64::from);
            self.lines.push(LineInst {
                c, seg: [a[0] as f32, a[1] as f32, b[0] as f32, b[1] as f32],
                width: [width, 0.0], dash: [0.0, 0.0], cap: if butt { 1.0 } else { 0.0 }, along0: along as f32, follow: [0.0; 2],
                press: [0.0; 4],
            });
            hits.wires.push(WireHit { item: if c.item < 0.0 { NO_ITEM } else { c.item as u32 }, net, a, b, run: None, seg: seg as u32 });
            along += (b[0] - a[0]).hypot(b[1] - a[1]);
        }
    }
}

impl Scene {
    /// A visible strip's exact band, without radius expansion into neighbouring rows.
    fn mini_hit(&self, at: [f64; 2], now: f64) -> Option<(u32, u32)> {
        self.minis.iter().zip(&self.hits.minis).rev().find_map(|(mini, hit)| {
            let pose = Flight { from: mini.c.pose_a, to: mini.c.pose_b, span: mini.c.span }.pose(now);
            (pose[3] > 0.0 && contains(shifted(hit.bounds, [pose[0] as f64, pose[1] as f64]), at, 0.0))
                .then_some((hit.net, if mini.c.item < 0.0 { NO_ITEM } else { mini.c.item as u32 }))
        })
    }

    /// The visible pin ends of circuit net `net` at `now`, appended to `out`: where a probe's flag
    /// can anchor. O(pins on the sheet).
    pub fn net_pins(&self, net: u32, now: f64, out: &mut Vec<NetPin>) {
        for pin in &self.hits.pins {
            if pin.net != Some(net) { continue; }
            let Some(part) = self.part(pin.node) else { continue };
            let pose = part.motion.pose(now);
            if pose[3] <= 0.0 { continue; }
            let d = [pose[0] as f64, pose[1] as f64];
            out.push(NetPin {
                node: pin.node, pin: pin.pin,
                at: [pin.outer[0] + d[0], pin.outer[1] + d[1]],
                out: [pin.outer[0] - pin.inner[0], pin.outer[1] - pin.inner[1]],
                body: shifted(part.body, d),
            });
        }
    }

    /// The longest wire piece of circuit net `net` while wires show: where a probe's flag anchors
    /// on a net with no visible pin. O(wire pieces).
    pub fn net_wire(&self, net: u32, now: f64) -> Option<([f64; 2], [f64; 2])> {
        if !self.detail.is_none_or(|flight| flight.pose(now)[3] > 0.0) { return None; }
        self.hits.wires.iter().filter(|wire| wire.net == Some(net))
            .max_by(|a, b| {
                let len = |w: &WireHit| (w.b[0] - w.a[0]).hypot(w.b[1] - w.a[1]);
                len(a).total_cmp(&len(b))
            })
            .map(|wire| (wire.a, wire.b))
    }

    /// Where the flag of a probe of circuit net `net` placed or labelled `label` anchors on this sheet: the
    /// pin [`anchor_pin`] picks among the net's visible pins, else the net's longest wire; None for
    /// a net not drawn here. `pins` is scratch. O(pins on the sheet).
    pub fn probe_anchor(&self, circuit: &Circuit, net: u32, label: &str, now: f64, pins: &mut Vec<NetPin>) -> Option<Anchor> {
        pins.clear();
        self.net_pins(net, now, pins);
        let spots: Vec<PinSpot> = pins.iter().map(|pin| match circuit.comps.get(pin.node as usize) {
            Some(comp) => PinSpot {
                refdes: comp.refdes.as_str(),
                number: comp.part.pins.get(pin.pin as usize).and_then(|def| def.pads.first()).map_or("", String::as_str),
                part_pins: comp.part.pins.len(),
            },
            None => PinSpot { refdes: "", number: "", part_pins: usize::MAX },
        }).collect();
        match anchor_pin(label, &spots).and_then(|k| pins.get(k)) {
            Some(pin) => Some(Anchor::pin(pin.at, pin.out, Some(pin.body))),
            None => self.net_wire(net, now).map(|(a, b)| Anchor::wire(a, b)),
        }
    }

    /// Visible strip, then nearest visible pin end or net wire, without allocating.
    /// Returns the circuit net, optional pin index and its part node (MAX for a wire/strip).
    pub fn probe_hit(&self, at: [f64; 2], radius: f64, now: f64) -> Option<(u32, Option<usize>, u32)> {
        if let Some((net, _)) = self.mini_hit(at, now) { return Some((net, None, u32::MAX)); }
        let radius = radius.max(0.0);
        let mut nearest = radius;
        let mut hit = None;
        for pin in &self.hits.connected_pins {
            let Some(part) = self.part(pin.node) else { continue };
            let pose = part.motion.pose(now);
            if pose[3] <= 0.0 { continue; }
            let distance = (at[0] - pin.at[0] - pose[0] as f64).hypot(at[1] - pin.at[1] - pose[1] as f64);
            if distance <= radius && (hit.is_none() || distance < nearest) {
                nearest = distance;
                hit = Some((pin.net, Some(pin.pin), pin.node));
            }
        }
        if hit.is_some() { return hit; }
        let wires_visible = self.detail.is_none_or(|flight| flight.pose(now)[3] > 0.0);
        if !wires_visible { return None; }
        for wire in &self.hits.wires {
            let Some(net) = wire.net else { continue };
            let d = [wire.b[0] - wire.a[0], wire.b[1] - wire.a[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            let t = if len2 > 0.0 {
                (((at[0] - wire.a[0]) * d[0] + (at[1] - wire.a[1]) * d[1]) / len2).clamp(0.0, 1.0)
            } else { 0.0 };
            let distance = (at[0] - wire.a[0] - t * d[0]).hypot(at[1] - wire.a[1] - t * d[1]);
            if distance <= radius && (hit.is_none() || distance < nearest) {
                nearest = distance;
                hit = Some((net, None, u32::MAX));
            }
        }
        hit
    }

    pub fn hit_into(&self, at: [f64; 2], radius: f64, now: f64, hits: &mut Vec<Sel>) {
        let mut push = |item| {
            if let Some(sel) = self.sel_of(item) {
                if !hits.contains(&sel) {
                    hits.push(sel);
                }
            }
        };
        if let Some((_, item)) = self.mini_hit(at, now) { push(item); }
        let radius = radius.max(0.0);
        for part in self.hits.parts.iter().rev() {
            if self.value_text(part.node).is_some_and(|text| text.alpha(now) > 0.0)
                && self.value_box_at(part.node, now).is_some_and(|b| contains(b, at, radius))
            {
                push(part.item);
            }
        }
        for part in self.hits.parts.iter().rev() {
            let pose = part.motion.pose(now);
            if pose[3] > 0.0 && contains(shifted(part.body, [pose[0] as f64, pose[1] as f64]), at, radius) {
                push(part.item);
            }
        }
        let wires_visible = self.detail.is_none_or(|flight| flight.pose(now)[3] > 0.0);
        for wire in self.hits.wires.iter().rev().filter(|_| wires_visible) {
            let d = [wire.b[0] - wire.a[0], wire.b[1] - wire.a[1]];
            let len2 = d[0] * d[0] + d[1] * d[1];
            let t = if len2 > 0.0 {
                (((at[0] - wire.a[0]) * d[0] + (at[1] - wire.a[1]) * d[1]) / len2).clamp(0.0, 1.0)
            } else { 0.0 };
            if (at[0] - wire.a[0] - t * d[0]).hypot(at[1] - wire.a[1] - t * d[1]) <= radius {
                push(wire.item);
            }
        }
        for &(item, band, _) in self.hits.frames.iter().rev() {
            if contains(band, at, 0.0) {
                push(item);
            }
        }
    }

    /// Every part this scene places, for a semantic lookup (a part `omit` also places is left to
    /// it): its selection, name, value, and its body as drawn at `now` (None while hidden); then,
    /// unless a reveal covers this scene (`omit`), every block's title strip by its path (the
    /// strip is where a block is pressed and dragged). O(parts + blocks), once per query; nothing
    /// runs per frame.
    pub fn lookup_parts(&self, now: f64, omit: Option<&Scene>, out: &mut Vec<LookupPart>) {
        for p in &self.hits.parts {
            if omit.is_some_and(|scene| scene.part_node(p.item).is_some_and(|node| scene.has_part(node))) {
                continue;
            }
            let Some(sel) = self.sel_of(p.item) else { continue };
            let pose = p.motion.pose(now);
            let body = (pose[3] > 0.0).then(|| shifted(p.body, [pose[0] as f64, pose[1] as f64]));
            out.push(LookupPart { sel, name: p.name.clone(), value: p.value.clone(), body });
        }
        if omit.is_some() { return; }
        for (item, band, path) in &self.hits.frames {
            let Some(sel) = self.sel_of(*item) else { continue };
            out.push(LookupPart { sel, name: path.clone(), value: None, body: Some(*band) });
        }
    }

    pub fn nodes_into(&self, cam: &Camera2d, now: f64, omit: Option<&Scene>, out: &mut Vec<CanvasNode<Sel>>) {
        let view = cam.visible();
        for p in &self.hits.parts {
            if omit.is_some_and(|scene| scene.part_node(p.item).is_some_and(|node| scene.has_part(node))) {
                continue;
            }
            let pose = p.motion.pose(now);
            let body = shifted(p.body, [pose[0] as f64, pose[1] as f64]);
            if pose[3] <= 0.0 || body[0] > view[2] || body[2] < view[0] || body[1] > view[3] || body[3] < view[1] {
                continue;
            }
            if let Some(sel) = self.sel_of(p.item) {
                out.push(CanvasNode {
                    sel, rect: cam.rect_to_screen(body), role: "part",
                    name: p.name.clone(), value: p.value.clone(),
                });
            }
        }
    }
}

impl CanvasItems for Scene {
    type Key = Sel;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Sel> {
        let mut hits = Vec::new();
        self.hit_into(at, radius, f64::INFINITY, &mut hits);
        hits
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Sel>> {
        let mut nodes = Vec::new();
        self.nodes_into(cam, f64::INFINITY, None, &mut nodes);
        nodes
    }
}

fn shifted(b: [f64; 4], offset: [f64; 2]) -> [f64; 4] {
    [b[0] + offset[0], b[1] + offset[1], b[2] + offset[0], b[3] + offset[1]]
}

pub(super) fn pinned(circuit: &Circuit, path: &str) -> bool {
    circuit.hints.parts.iter().rev().find(|(p, _)| p == path)
        .is_some_and(|(_, hint)| hint.at.is_some())
}

/// Include the posed symbol ink in its world-space bounds. An arc includes only
/// the axis extrema it sweeps through, so a shallow arc stays close to the part.
fn extend_symbol_bounds(bounds: &mut [f32; 4], prim: &Prim) {
    use std::f32::consts::{FRAC_PI_2, TAU};
    let mut include = |[x, y]: [f32; 2]| {
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    };
    match *prim {
        Prim::Line { a, b, pressed, .. } => {
            include(a);
            include(b);
            if let Some([a, b]) = pressed { include(a); include(b); }
        }
        Prim::Box { c, half, .. } => {
            include([c[0]-half[0], c[1]-half[1]]);
            include([c[0]+half[0], c[1]+half[1]]);
        }
        Prim::Arc { c, r, start, sweep, .. } => {
            if sweep.abs() >= TAU {
                include([c[0]-r, c[1]-r]);
                include([c[0]+r, c[1]+r]);
            } else {
                for angle in [start, start+sweep] {
                    include([c[0]+r*angle.cos(), c[1]+r*angle.sin()]);
                }
                for (i, [x, y]) in [[r, 0.0], [0.0, r], [-r, 0.0], [0.0, -r]].into_iter().enumerate() {
                    let angle = i as f32 * FRAC_PI_2;
                    if ((angle-start)*sweep.signum()).rem_euclid(TAU) <= sweep.abs() {
                        include([c[0]+x, c[1]+y]);
                    }
                }
            }
        }
    }
}

fn common(item: u32, ink: [f32; 4]) -> Common {
    Common {
        item: if item == NO_ITEM { -1.0 } else { item as f32 },
        ..Common::at([0.0, 0.0, 0.0], ink)
    }
}

fn moving(flight: Flight) -> bool {
    (0..2).any(|i| (flight.to[i] - flight.from[i]).abs() > 1e-3)
}

fn same_flight(a: Flight, b: Flight) -> bool {
    a.from.iter().chain(&a.to).chain(&a.span).zip(b.from.iter().chain(&b.to).chain(&b.span))
        .all(|(a, b)| (a - b).abs() <= 1e-4)
}

/// Connected parts with equal displacement share take-off, so their rails remain rigid.
fn join_flights(flights: &mut [Option<Flight>], groups: &[Vec<u32>]) {
    fn root(parent: &mut [usize], mut n: usize) -> usize {
        while parent[n] != n {
            parent[n] = parent[parent[n]];
            n = parent[n];
        }
        n
    }
    let mut parent: Vec<_> = (0..flights.len()).collect();
    for nodes in groups {
        for (i, &a) in nodes.iter().enumerate() {
            let fa = flights[a as usize].unwrap_or(Flight::REST);
            for &b in &nodes[i + 1..] {
                let fb = flights[b as usize].unwrap_or(Flight::REST);
                if (0..2).all(|i| ((fa.to[i] - fa.from[i]) - (fb.to[i] - fb.from[i])).abs() <= 1e-3) {
                    let a = root(&mut parent, a as usize);
                    let b = root(&mut parent, b as usize);
                    parent[b] = a;
                }
            }
        }
    }
    let mut starts = vec![f32::INFINITY; flights.len()];
    for (n, flight) in flights.iter().enumerate() {
        let group = root(&mut parent, n);
        starts[group] = starts[group].min(flight.unwrap_or(Flight::REST).span[0]);
    }
    for (n, flight) in flights.iter_mut().enumerate() {
        if let Some(flight) = flight { flight.span[0] = starts[root(&mut parent, n)]; }
    }
}

fn fading(state: &SceneState, item: u32, ink: [f32; 4], delay: f64, duration: f64) -> Common {
    let mut c = common(item, ink);
    if let Some(fly) = &state.fly {
        fly.detail.apply(&mut c);
    } else if let Some(t0) = state.assemble_at {
        c.pose_a[3] = 0.0;
        c.span = [(t0 + delay) as f32, duration as f32];
    }
    c
}

fn owner_item(owner: Owner, rows: &RowIndex) -> u32 {
    match owner {
        Owner::Part(n) => rows.comp(n),
        Owner::Net(n) => rows.net(n),
        Owner::Trace(_) | Owner::Bundle(_) => None,
        Owner::Bus(n) => rows.bus(n),
        Owner::Block(n) => rows.block(n),
    }.unwrap_or(NO_ITEM)
}

fn net_ink(circuit: &Circuit, net: u32, tokens: &Tokens) -> [f32; 4] {
    match circuit.nets[net as usize].kind {
        NetKind::Signal => tokens.wire_signal,
        NetKind::Power => tokens.wire_power,
        NetKind::Ground => tokens.wire_ground,
    }
}

/// The drawn glyph of a part kind; other kinds draw their own symbol.
fn family(kind: &PartKind) -> Option<Family> {
    Some(match kind {
        PartKind::Resistor | PartKind::Fuse => Family::Resistor,
        PartKind::Capacitor => Family::Capacitor,
        PartKind::Inductor => Family::Inductor,
        PartKind::Led => Family::Led,
        PartKind::Diode => Family::Diode,
        PartKind::Switch => Family::Switch,
        PartKind::Crystal => Family::Crystal,
        PartKind::PowerFlag => Family::Flag,
        _ => return None,
    })
}

fn contains(b: [f64; 4], at: [f64; 2], radius: f64) -> bool {
    at[0] >= b[0] - radius && at[0] <= b[2] + radius && at[1] >= b[1] - radius && at[1] <= b[3] + radius
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::{SheetHint, SheetHints};
    use fab_elec_core::SchPt;
    use fab_elec_explore::{layout, rows::SheetRows, Opts};

    #[test]
    fn vertical_name_flags_read_along_their_pins() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let mut sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        if !sheet.tags.iter().any(|tag| tag.kind == sheet::TagKind::Label && tag.dir[1] != 0) {
            let mcu = circuit.comp("mcu.u").expect("vertical text: the fixture has an MCU");
            let part = &sheet.parts[mcu.id.ix()];
            let at = fab_elec_explore::to_local(&sheet, part.block, part.at)
                .expect("vertical text: the MCU has a block-local origin");
            let hints = SheetHints {
                parts: vec![(mcu.path.clone(), SheetHint {
                    at: Some(SchPt::new(at[0], at[1])), turn: (part.turn + 1) % 4, ..SheetHint::default()
                })],
                ..SheetHints::default()
            };
            sheet = layout(&design, 0, &hints, &Opts::default());
        }
        let rows = SheetRows::new().sync(circuit);
        let scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &SceneLive::default());
        let mut name_flags = vec![false; scene.texts.len()];
        let mut vertical = 0;
        for tag in sheet.tags.iter().filter(|tag| tag.kind == sheet::TagKind::Label) {
            let kind = glyphs::TagKind::Label { text_w: sheet::text_w(&tag.text, sheet::TEXT_NET) as f32 };
            let (_, anchor) = glyphs::tag_prims(kind, tag.at.map(|v| v as f32), tag.dir, tag.band, tag.side);
            let (at, _) = anchor.expect("vertical text: a name flag has a text anchor");
            let i = scene.texts.iter().position(|text| text.text == tag.text && text.at == at.map(f64::from))
                .expect("vertical text: every name flag has text at its glyph anchor");
            assert_eq!(scene.texts[i].vertical, tag.dir[1] != 0,
                "vertical text: every name flag on an up or down pin reads along it and no other text does");
            name_flags[i] = true;
            if tag.dir[1] != 0 { vertical += 1; }
        }
        for (text, is_flag) in scene.texts.iter().zip(name_flags) {
            if !is_flag {
                assert!(!text.vertical, "vertical text: every non-tag text stays horizontal");
            }
        }
        assert!(vertical > 0, "vertical text: the fixture has a vertical name flag");
    }

    #[test]
    fn pinned_parts_carry_one_mark_each() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let tokens = Tokens::for_theme(true);
        let state = SceneState::default();
        let live = SceneLive::default();
        let nodes = ["mcu.u", "lamps[0].r"].map(|path| circuit.comp(path)
            .expect("pin mark: the fixture has the MCU and a resistor").id.0);
        let capacitor = circuit.comp("mcu.rst_c").expect("pin mark: the fixture has a capacitor").id.0;
        let mut hints: Vec<_> = nodes.iter().map(|&node| {
            let part = &sheet.parts[node as usize];
            assert!(part.placed, "pin mark: the hinted parts are placed");
            let at = fab_elec_explore::to_local(&sheet, part.block, part.at)
                .expect("pin mark: positioned hints use the part's block-local origin");
            (circuit.comps[node as usize].path.clone(), SheetHint {
                at: Some(SchPt::new(at[0], at[1])), turn: part.turn, ..SheetHint::default()
            })
        }).collect();
        hints.push((circuit.comps[capacitor as usize].path.clone(), SheetHint::default()));
        let mut hinted = design.clone();
        std::sync::Arc::make_mut(&mut hinted.units[0].circuit).hints.parts = hints;
        let pinned_sheet = layout(&hinted, 0, &SheetHints::default(), &Opts::default());
        let scene = Scene::build(&hinted, 0, &pinned_sheet, &rows, tokens, &state, &live);
        let mut fill = tokens.selection;
        fill[3] = 0.9;
        let is_mark = |b: &BoxInst| b.fill == fill && b.rect[2..] == [0.35, 0.35];
        let marks: Vec<_> = scene.boxes.iter().filter(|b| is_mark(b)).collect();
        assert_eq!(marks.len(), 2, "pin mark: exactly one mark per positioned hint");
        let circuit = &hinted.units[0].circuit;
        for node in nodes {
            let body = pinned_sheet.parts[node as usize].body;
            let item = rows.comp(node).expect("pin mark: every pinned part has a row") as f32;
            assert!(pinned(circuit, &circuit.comps[node as usize].path),
                "pin mark: positioned hints report pinned");
            assert_eq!(marks.iter().filter(|b| b.c.item == item
                && b.rect == [body[0] as f32, body[3] as f32, 0.35, 0.35]).count(), 1,
                "pin mark: each mark sits on its part's body corner and rides the part's row");
        }
        let capacitor_item = rows.comp(capacitor).expect("pin mark: the capacitor has a row") as f32;
        assert!(!pinned(circuit, &circuit.comps[capacitor as usize].path)
            && marks.iter().all(|b| b.c.item != capacitor_item),
            "pin mark: a hint without a position is no pin");
        let unhinted = Scene::build(&design, 0, &sheet, &rows, tokens, &state, &live);
        assert!(!unhinted.boxes.iter().any(is_mark), "pin mark: an unhinted design has none");
    }

    #[test]
    fn pinned_blocks_carry_one_mark_each() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let tokens = Tokens::for_theme(true);
        let state = SceneState::default();
        let live = SceneLive::default();
        let [mcu, reg] = ["mcu", "reg"].map(|path| circuit.tree.iter().find(|block| block.path == path)
            .expect("block pin mark: the fixture has MCU and regulator blocks").id.0);
        let origin = fab_elec_explore::block_origin(&sheet, mcu)
            .expect("block pin mark: the MCU block is placed");
        let at = fab_elec_explore::to_local(&sheet, 0, origin)
            .expect("block pin mark: the positioned hint uses the root's local origin");
        let mut hinted = design.clone();
        std::sync::Arc::make_mut(&mut hinted.units[0].circuit).hints.parts = vec![
            (circuit.tree[mcu as usize].path.clone(), SheetHint {
                at: Some(SchPt::new(at[0], at[1])), ..SheetHint::default()
            }),
            (circuit.tree[reg as usize].path.clone(), SheetHint::default()),
        ];
        let pinned_sheet = layout(&hinted, 0, &SheetHints::default(), &Opts::default());
        let scene = Scene::build(&hinted, 0, &pinned_sheet, &rows, tokens, &state, &live);
        let mut fill = tokens.selection;
        fill[3] = 0.9;
        let is_mark = |b: &BoxInst| b.fill == fill && b.rect[2..] == [0.35, 0.35];
        let marks: Vec<_> = scene.boxes.iter()
            .filter(|b| is_mark(b) && scene.block_node(b.c.item as u32).is_some()).collect();
        assert_eq!(marks.len(), 1, "block pin mark: exactly one mark per positioned block hint");
        let rect = pinned_sheet.frames[mcu as usize].rect;
        let item = rows.block(mcu).expect("block pin mark: the MCU block has a row") as f32;
        assert_eq!(marks.iter().filter(|b| b.c.item == item
            && b.rect == [rect[0] as f32, rect[3] as f32, 0.35, 0.35]).count(), 1,
            "block pin mark: the mark sits on the frame's top-left corner and rides the frame's row");
        let reg_item = rows.block(reg).expect("block pin mark: the regulator block has a row") as f32;
        let circuit = &hinted.units[0].circuit;
        assert!(!pinned(circuit, &circuit.tree[reg as usize].path)
            && marks.iter().all(|b| b.c.item != reg_item),
            "block pin mark: a hint without a position is no pin");
    }

    #[test]
    fn transitions_join_moving_parts_with_arcs_on_ledctl() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let tokens = Tokens::for_theme(true);
        let live = SceneLive::default();
        let scene = Scene::build(&design, 0, &sheet, &rows, tokens,
            &SceneState { assemble_at: Some(10.0), ..SceneState::default() }, &live);
        assert!(!scene.bridges.is_empty(), "transitions: assembly joins moving parts with arcs");
        let mut actual = HashMap::<u32, usize>::new();
        for bridge in &scene.bridges {
            for at in [[bridge.ends[0], bridge.ends[1]], [bridge.ends[2], bridge.ends[3]]] {
                assert!(scene.hits.connected_pins.iter().any(|pin| {
                    (pin.at[0] - at[0] as f64).hypot(pin.at[1] - at[1] as f64) <= 1e-4
                }), "transitions: each arc end is a placed pin at rest");
            }
            *actual.entry(bridge.c.item as u32).or_default() += 1;
            assert!(scene.settle_at() >= bridge.fade[2] as f64 + bridge.fade[3] as f64,
                "transitions: settle covers every arc fade");
        }
        let pins: Vec<_> = scene.hits.connected_pins.iter().map(|pin| (pin.net, pin.at.map(|v| v.round() as i32))).collect();
        let groups = wire_groups(&sheet, &pins);
        let mut expected = HashMap::<u32, usize>::new();
        for group in &groups {
            let mut nodes: Vec<_> = group.pins.iter().map(|&pin| scene.hits.connected_pins[pin as usize].node).collect();
            nodes.sort_unstable();
            nodes.dedup();
            let Some(&first) = nodes.first() else { continue };
            let flight = scene.part(first).expect("transitions: grouped pins belong to placed parts").motion;
            let rigid = nodes.iter().all(|&node| same_flight(flight,
                scene.part(node).expect("transitions: each grouped node has a flight").motion));
            if !rigid {
                *expected.entry(rows.net(group.net).expect("transitions: every group has a net row")).or_default() += nodes.len() - 1;
            }
        }
        assert_eq!(actual, expected, "transitions: each net has one spanning tree per stretching group");
        for text in &scene.texts {
            assert!(scene.settle_at() >= text.fade.end(), "transitions: settle covers every text fade");
        }
        let rest = Scene::build(&design, 0, &sheet, &rows, tokens, &SceneState::default(), &live);
        assert!(rest.bridges.is_empty(), "transitions: rest has no travel arcs");
        for text in &rest.texts {
            assert_eq!(text.fade, TextFade::REST, "transitions: resting text has a constant full-opacity fade");
            for now in [-1.0, 0.0, 10.0, f64::INFINITY] {
                assert_eq!(text.alpha(now), 1.0, "transitions: resting text is fully visible at any time");
            }
        }
        let mut parts = vec![None; sheet.parts.len()];
        for (k, (n, _)) in sheet.parts.iter().enumerate().filter(|(_, p)| p.placed).enumerate() {
            parts[n] = Some(Flight {
                from: [0.0, 0.0, 0.0, 1.0], to: [5.0, 0.0, 0.0, 1.0],
                span: [(10.0 + 0.01 * k as f64) as f32, REVEAL_SECONDS as f32],
            });
        }
        let leaving = Scene::build(&design, 0, &sheet, &rows, tokens, &SceneState {
            fly: Some(FlyFrom {
                parts, detail: Flight { to: [0.0; 4], span: [10.0, STATE_SECONDS as f32], ..Flight::REST },
                leg: Leg::Leave, start: 10.0, continued: false, text_from: Vec::new(),
            }), ..SceneState::default()
        }, &live);
        assert!(leaving.bridges.is_empty(), "transitions: equal displacements make wired molecules rigid");
        let mut wired = 0;
        for group in &groups {
            let mut nodes: Vec<_> = group.pins.iter().map(|&pin| scene.hits.connected_pins[pin as usize].node).collect();
            nodes.sort_unstable();
            nodes.dedup();
            let [first, rest @ ..] = nodes.as_slice() else { continue };
            if rest.is_empty() { continue; }
            wired += 1;
            let a = leaving.part(*first).expect("transitions: a wired part is placed");
            let now = a.motion.span[0] as f64 + a.motion.span[1] as f64 * 0.5;
            let at = leaving.position(a.node, now).expect("transitions: a leaving part has a position");
            for &node in rest {
                let b = leaving.part(node).expect("transitions: every wired part is placed");
                let bt = leaving.position(node, now).expect("transitions: every leaving part has a position");
                assert!((0..2).all(|i| (at[i] - bt[i] - (a.at[i] - b.at[i])).abs() <= 1e-4),
                    "transitions: wired parts moving alike share a span and keep their resting separation");
            }
        }
        assert!(wired > 0, "transitions: the fixture has groups wiring two or more parts");
    }

    /// The pane publishes one node; a lookup answers parts on request. One setup checks each
    /// property: every part is found by its refdes exactly once, with a centre that presses it
    /// (or no bounds and OFFSCREEN); a key resolves to the same part; identical queries give
    /// identical results; structural filters materialise nothing; out of view means OFFSCREEN.
    #[test]
    fn lookup_answers_each_part_by_name_with_a_pressable_centre() {
        use crate::canvas::pane::{part_key, HIT_RADIUS};
        use crate::explorer::reveal_view::ExplorerItems;
        use makepad_widgets_core::makepad_platform::a11y_tree::{query::Query, States};
        use makepad_widgets_core::{dvec2, Rect};
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &SceneLive::default());
        let view = Rect { pos: dvec2(0.0, 0.0), size: dvec2(1600.0, 900.0) };
        let b = sheet.bounds.map(f64::from);
        // The whole sheet in a 1600 x 900 view.
        let mut cam = Camera2d::fit(view, 1.0, b, 24.0);
        let items = ExplorerItems { sheet: &scene, reveal: None, cam: &cam, now: f64::INFINITY };
        let query = |text: &str| Query::parse(text).expect("lookup: a valid query");
        let parts = items.lookup_parts();
        assert!(parts.len() > 20, "lookup: the fixture places its parts ({})", parts.len());
        let mut pressable = 0;
        for part in &parts {
            let found = items.lookup(Some(&query(&format!("name={}", part.name))), None);
            assert_eq!(found.len(), 1, "lookup: {} is found by its refdes exactly once", part.name);
            let (key, node) = &found[0];
            assert_eq!(*key, part_key(&part.sel), "lookup: the answer keeps the part's stable key");
            assert_eq!(items.lookup(None, Some(*key)), found, "lookup: a key resolves to the same answer");
            if node.states.contains(States::OFFSCREEN) {
                assert!(node.bounds.is_empty(), "lookup: an offscreen answer has no bounds ({})", part.name);
                continue;
            }
            pressable += 1;
            let (x, y) = node.bounds.center();
            let at = cam.to_world(dvec2(f64::from(x), f64::from(y)));
            assert_eq!(items.hit(at, HIT_RADIUS / cam.px_per_unit).first(), Some(&part.sel),
                "lookup: the centre of {}'s bounds presses it", part.name);
        }
        assert!(pressable > 0, "lookup: parts in plain sight have bounds");
        let all = query("name~=");
        assert_eq!(items.lookup(Some(&all), None), items.lookup(Some(&all), None), "lookup: identical queries, identical answers");
        let names: Vec<_> = items.lookup(Some(&query("name~=r1")), None).into_iter().map(|(_, n)| n.name.to_string()).collect();
        assert!(names.windows(2).all(|w| w[0].len() < w[1].len() || (w[0].len() == w[1].len() && w[0] <= w[1])),
            "lookup: refdes come in natural order ({names:?})");
        assert!(items.lookup(Some(&query("role=button")), None).is_empty(), "lookup: structural filters alone materialise nothing");
        cam.center = [b[2] + 1000.0, b[3] + 1000.0];
        let away = ExplorerItems { sheet: &scene, reveal: None, cam: &cam, now: f64::INFINITY };
        let found = away.lookup(Some(&query("name=U2")), None);
        assert!(found.len() == 1 && found[0].1.states.contains(States::OFFSCREEN) && found[0].1.bounds.is_empty(),
            "lookup: a part out of view answers OFFSCREEN without bounds");
    }

    #[test]
    fn probe_hits_ledctl_wire_pin_and_misses_outside() {
        let design = fab_elec_fixtures::ledctl();
        let circuit = &design.units[0].circuit;
        let sheet = layout(&design, 0, &SheetHints::default(), &Opts::default());
        let rows = SheetRows::new().sync(circuit);
        let graph = fab_elec_explore::adapt::graph_of(circuit, &SheetHints::default());
        let strips: Vec<_> = sheet.labels.iter().filter_map(|label| match label.owner {
            Owner::Trace(net) => Some((label, vec![net])),
            Owner::Bundle(bus) => Some((label, graph.buses[bus as usize].nets.clone())),
            _ => None,
        }).collect();
        let mut live = SceneLive {
            nets: (0..circuit.nets.len()).map(|net| (net + 100) as f32).collect(),
            mini: vec![-1.0; circuit.nets.len()],
            ..SceneLive::default()
        };
        for (_, nets) in &strips {
            for &net in nets { live.mini[net as usize] = (net + 1000) as f32; }
        }
        let mut scene = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &live);
        assert_eq!(scene.minis.len(), strips.iter().map(|(_, nets)| nets.len()).sum::<usize>(),
            "mini traces: one instance per Trace label and per Bundle member");
        assert!(strips.iter().any(|(label, _)| matches!(label.owner, Owner::Trace(_))),
            "mini traces: the fixture exercises a standalone strip");
        let mut first = 0;
        let mut lamps = None;
        for (label, nets) in &strips {
            let instances = &scene.minis[first..first + nets.len()];
            for (i, (&net, mini)) in nets.iter().zip(instances).enumerate() {
                assert_eq!(mini.c.item, rows.net(net).expect("mini traces: every net has an item row") as f32,
                    "mini traces: each strip selects its own net item");
                assert_eq!(mini.c.live_ix[0], live.nets[net as usize], "mini traces: each strip carries its net's live row");
                assert_eq!(mini.slot, live.mini[net as usize], "mini traces: each strip carries its assigned slot");
                let at = [mini.rect[0] as f64, mini.rect[1] as f64];
                assert!(contains(label.bx, at, 0.0), "mini traces: every row centre is inside its label box");
                let sel = Sel::Net(NetRef { board: design.units[0].id, net: NetId(net) });
                let mut hits = Vec::new();
                scene.hit_into(at, 1.0, 0.0, &mut hits);
                assert_eq!(hits.first(), Some(&sel), "mini traces: strip centre hits its own net despite neighbouring row radius");
                assert_eq!(scene.probe_hit(at, 1.0, 0.0), Some((net, None, u32::MAX)),
                    "mini traces: probing a strip returns its own net without a pin");
                match label.owner {
                    Owner::Trace(_) => {
                        let at = [(label.bx[0] + label.bx[2]) * 0.5, (label.bx[1] + label.bx[3]) * 0.5];
                        assert_eq!(mini.rect, [at[0] as f32, at[1] as f32,
                            ((label.bx[2] - label.bx[0]) * 0.5) as f32, ((label.bx[3] - label.bx[1]) * 0.5) as f32],
                            "mini traces: a standalone strip occupies its whole label box");
                        assert_eq!(scene.hit(at, 0.0).first(), Some(&sel), "mini traces: a Trace label box hits its net");
                    }
                    Owner::Bundle(_) => {
                        let top = label.bx[3] - 0.2 - i as f64 * sheet::BUNDLE_ROW;
                        assert!((at[1] - (top - 0.5 * sheet::BUNDLE_ROW)).abs() < 1e-4,
                            "mini traces: bundle member order descends from the top padding");
                        assert_eq!(mini.rect[3], (0.4 * sheet::BUNDLE_ROW) as f32,
                            "mini traces: bundle rows leave a gap between their fills");
                        assert!(at[1] - mini.rect[3] as f64 > label.bx[1]
                            && at[1] + (mini.rect[3] as f64) < label.bx[3],
                            "mini traces: bundle rows stay wholly inside the stack box");
                    }
                    _ => unreachable!("mini traces: only strip labels were collected"),
                }
            }
            if matches!(label.owner, Owner::Bundle(bus) if graph.buses[bus as usize].name == "LED") {
                assert_eq!(nets.len(), 8, "mini traces: the lamps bundle has eight member rows");
                assert!(instances.windows(2).all(|pair| pair[0].rect[1] > pair[1].rect[1]),
                    "mini traces: lamps row centres strictly descend along world y in member order");
                lamps = Some((first, nets[3]));
            }
            first += nets.len();
        }
        let (lamps_first, missing) = lamps.expect("mini traces: the fixture places the lamps bundle");
        live.mini[missing as usize] = -1.0;
        let partial = Scene::build(&design, 0, &sheet, &rows, Tokens::for_theme(true), &SceneState::default(), &live);
        let kept: Vec<_> = scene.minis.iter().filter(|mini| mini.slot != (missing + 1000) as f32).copied().collect();
        assert_eq!(partial.minis, kept, "mini traces: an unsubscribed member leaves its row empty without shifting other rows");
        let empty = scene.minis[lamps_first + 3].rect;
        assert!(partial.mini_hit([empty[0] as f64, empty[1] as f64], 0.0).is_none(),
            "mini traces: an unsubscribed member's empty band has no strip hit");

        let mut moving = vec![false; circuit.comps.len()];
        let lamp = circuit.comp("lamps[3].d").expect("mini traces: the fixture has a moving lamp").id.0 as usize;
        moving[lamp] = true;
        scene.follow(&moving);
        assert_eq!(scene.minis[lamps_first + 3].c.pose_b[3], 0.33,
            "mini traces: a moving part's net strip fades with its source text");
        scene.follow(&[]);
        assert_eq!(scene.minis[lamps_first + 3].c.pose_b[3], 1.0,
            "mini traces: ending a drag restores strip opacity without cumulative fading");
        let net = circuit.nets.iter().position(|net| net.name.ends_with("NRST"))
            .expect("probe hit: wire fixture has an NRST net") as u32;
        let at = sheet.wires.iter().filter(|run| run.net == net).flat_map(|run| run.pts.windows(2))
            .filter(|pair| pair[0] != pair[1]).find_map(|pair| {
                let at = [(pair[0][0] as f64 + pair[1][0] as f64) * 0.5,
                    (pair[0][1] as f64 + pair[1][1] as f64) * 0.5];
                scene.hits.connected_pins.iter().all(|pin| (at[0] - pin.at[0]).hypot(at[1] - pin.at[1]) > 0.1).then_some(at)
            }).expect("probe hit: wire fixture has an NRST segment midpoint clear of pins");
        assert_eq!(scene.probe_hit(at, 0.1, 0.0), Some((net, None, u32::MAX)), "probe hit: wire");

        let node = circuit.comps.iter().position(|comp| comp.refdes == "U2")
            .expect("probe hit: pin fixture has U2") as u32;
        let pin = scene.hits.connected_pins.iter().find(|pin| pin.node == node && circuit.comps[node as usize].part.pins[pin.pin].name == "NRST")
            .expect("probe hit: pin fixture has U2 NRST");
        assert_eq!(scene.probe_hit(pin.at, 0.1, 0.0), Some((net, Some(pin.pin), node)), "probe hit: pin");
        assert_eq!(crate::live::probe::probe_label(&circuit.comps[node as usize], pin.pin).as_deref(), Some("U2.3\nNRST"),
            "probe label: a named MCU pin is <refdes>.<number>, its name on a second line");
        let r1 = circuit.comps.iter().find(|comp| comp.refdes == "R1").expect("probe label: fixture has R1");
        assert_eq!(crate::live::probe::probe_label(r1, 0).as_deref(), Some("R1.1"), "probe label: a passive's pin is <refdes>.<number> alone");

        let outside = [sheet.bounds[2] as f64 + 1000.0, sheet.bounds[3] as f64 + 1000.0];
        assert_eq!(scene.probe_hit(outside, 0.1, 0.0), None, "probe hit: miss");
    }
}
