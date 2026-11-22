//! A part's local reveal: trace its relations, then place them in the focus's sheet frame.
//! All ordering comes from pins, nets and source order. Wires stop at component pins: a path
//! through a series part or regulator has a separate run for each electrical net.

use crate::graph::{LayoutGraph, NetKind, PortKind, Pt};
use crate::layered::Side;
use crate::molecules;
use crate::sheet::{self, Bx, PartPlace, SheetLayout};
use fab_elec_core::{BoardId, CompId, CompRef, PinIx, PinRef};
use fab_elec_design::{trace, Design};
use std::cmp::Ordering;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Rel {
    Focus,
    Power,
    Controller,
    Driver,
    Load,
    Hop,
    Satellite,
}

/// What is related to the focus, in display order.
#[derive(Clone, Debug, PartialEq)]
pub struct Relations {
    pub focus: u32,
    /// One column per supply rail, volts high first, unknown last, then net index.
    pub rails: Vec<RailCol>,
    /// (Focus port, controller node, controller port), ordered by controller port.
    pub controllers: Vec<(u32, u32, u32)>,
    /// Input ports, top first in the focus's own frame, then port index.
    pub drivers: Vec<Link>,
    /// Output ports in the same order; bidirectional and passive pins can occur in both lists.
    pub loads: Vec<Link>,
    /// Members of the focus's own molecules, without the focus itself.
    pub satellites: Vec<u32>,
    /// Foreign ends and hops omitted from the trace answers.
    pub skipped: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RailCol {
    pub rail: u32,
    pub volts: Option<f64>,
    /// Focus-side hops first, source node last. Each node occurs at most once.
    pub chain: Vec<u32>,
    /// A power output's part, or None for an external source.
    pub source: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub focus_port: u32,
    pub node: u32,
    pub port: u32,
    /// Intermediate nodes, from the focus outward.
    pub hops: Vec<u32>,
}

fn local_hops(board: BoardId, hops: &[trace::Hop], skipped: &mut usize) -> Vec<u32> {
    let mut out = Vec::new();
    for h in hops {
        if h.through.board != board || h.from.board != board || h.to.board != board {
            *skipped += 1;
        } else if !out.contains(&h.through.comp.0) {
            out.push(h.through.comp.0);
        }
    }
    out
}

fn local_link(
    focus: CompRef,
    port: u32,
    end: PinRef,
    hops: &[trace::Hop],
    skipped: &mut usize,
) -> Option<Link> {
    let mut hops = local_hops(focus.board, hops, skipped);
    if end.board != focus.board {
        *skipped += 1;
        return None;
    }
    if end.comp == focus.comp {
        return None;
    }
    hops.retain(|&n| n != focus.comp.0 && n != end.comp.0);
    Some(Link { focus_port: port, node: end.comp.0, port: u32::from(end.pin.0), hops })
}

/// Trace one unit's focus. The graph must describe that same unit, with component/pin indices
/// unchanged. Controller paths are also retained as links so their intermediate pins can wire.
pub fn relations(d: &Design, unit: usize, g: &LayoutGraph, focus: u32) -> Relations {
    let mut out = Relations {
        focus,
        rails: Vec::new(),
        controllers: Vec::new(),
        drivers: Vec::new(),
        loads: Vec::new(),
        satellites: Vec::new(),
        skipped: 0,
    };
    let Some(u) = d.units.get(unit) else { return out };
    let Some(node) = g.nodes.get(focus as usize) else { return out };
    let comp = CompRef { board: u.id, comp: CompId(focus) };
    let port_nets = g.port_nets();

    for r in trace::power_tree(d, comp) {
        let mut chain = local_hops(u.id, &r.hops, &mut out.skipped);
        let source = match r.source {
            trace::Source::Pin(p) if p.board == u.id => Some(p.comp.0),
            trace::Source::Pin(_) => {
                out.skipped += 1;
                None
            }
            trace::Source::External(n) => {
                if n.board != u.id {
                    out.skipped += 1;
                }
                None
            }
        };
        chain.retain(|&n| n != focus && Some(n) != source);
        if let Some(n) = source.filter(|&n| n != focus) {
            chain.push(n);
        }
        if !out.rails.iter().any(|c| c.rail == r.rail.net.0) {
            out.rails.push(RailCol {
                rail: r.rail.net.0,
                volts: r.volts.map(|v| 0.5 * v.min + 0.5 * v.max),
                chain,
                source,
            });
        }
    }
    out.rails.sort_by(|a, b| {
        match (a.volts, b.volts) {
            (Some(a), Some(b)) => b.total_cmp(&a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| a.rail.cmp(&b.rail))
    });

    for c in trace::controllers(d, comp) {
        if let Some(link) = local_link(comp, u32::from(c.target_pin.pin.0), c.mcu_pin, &c.hops, &mut out.skipped) {
            out.controllers.push((link.focus_port, link.node, link.port));
            match c.role {
                trace::ControlRole::Drives => out.drivers.push(link),
                trace::ControlRole::Senses => out.loads.push(link),
            }
        }
    }
    out.controllers.sort_by_key(|&(fp, n, p)| (p, n, fp));
    out.controllers.dedup();

    for (p, port) in node.ports.iter().enumerate() {
        if port.nc || !port_nets[focus as usize][p].is_some_and(|n| g.nets[n as usize].kind == NetKind::Signal) {
            continue;
        }
        let pin = PinRef { board: u.id, comp: comp.comp, pin: PinIx(p as u16) };
        if matches!(port.kind, PortKind::In | PortKind::Bidi | PortKind::Passive) {
            for t in trace::drivers(d, pin) {
                if let Some(link) = local_link(comp, p as u32, t.end, &t.hops, &mut out.skipped) {
                    out.drivers.push(link);
                }
            }
        }
        if matches!(port.kind, PortKind::Out | PortKind::WeakOut | PortKind::Bidi | PortKind::Passive) {
            for t in trace::loads(d, pin) {
                if let Some(link) = local_link(comp, p as u32, t.end, &t.hops, &mut out.skipped) {
                    out.loads.push(link);
                }
            }
        }
    }
    for links in [&mut out.drivers, &mut out.loads] {
        links.sort_by(|a, b| {
            node.ports[b.focus_port as usize].at[1]
                .cmp(&node.ports[a.focus_port as usize].at[1])
                .then_with(|| a.focus_port.cmp(&b.focus_port))
                .then_with(|| a.node.cmp(&b.node))
                .then_with(|| a.port.cmp(&b.port))
                .then_with(|| a.hops.cmp(&b.hops))
        });
        links.dedup();
    }

    let mut bound = vec![false; g.nets.len()];
    for b in &g.blocks {
        for p in &b.ports {
            for &n in &p.nets {
                bound[n as usize] = true;
            }
        }
    }
    for m in molecules::find(g, &port_nets, &bound) {
        if m.host.is_some_and(|(n, _)| n == focus) || m.parts.contains(&focus) {
            for n in m.parts {
                if n != focus && !out.satellites.contains(&n) {
                    out.satellites.push(n);
                }
            }
        }
    }
    out.satellites.sort_by_key(|&n| (g.nodes[n as usize].order, n));

    // Untyped ends: a signal pin the trace answered nothing for (passive parts, headers, a switch)
    // still shows what its net reaches. A small two-pin part in line is a series hop to what lies
    // beyond it. Input pins put their ends on the driver side, every other kind on the load side.
    let traced: Vec<u32> = out.drivers.iter().chain(&out.loads).map(|l| l.focus_port).collect();
    for (p, port) in node.ports.iter().enumerate() {
        if port.nc || traced.contains(&(p as u32)) {
            continue;
        }
        let Some(net) = port_nets[focus as usize][p].filter(|&n| g.nets[n as usize].kind == NetKind::Signal) else { continue };
        let mut links: Vec<Link> = Vec::new();
        for &(n, q) in &g.nets[net as usize].pins {
            if n == focus || out.satellites.contains(&n) {
                continue;
            }
            let other = &g.nodes[n as usize];
            let far = (other.role.is_small() && other.ports.len() == 2)
                .then(|| port_nets[n as usize][1 - q as usize])
                .flatten()
                .filter(|&f| f != net && g.nets[f as usize].kind == NetKind::Signal);
            let beyond: Vec<(u32, u32)> = far.map_or(Vec::new(), |f| {
                g.nets[f as usize].pins.iter().copied().filter(|&(m, _)| m != n && m != focus).collect()
            });
            if beyond.is_empty() {
                links.push(Link { focus_port: p as u32, node: n, port: q, hops: Vec::new() });
            } else {
                for (m, r) in beyond {
                    links.push(Link { focus_port: p as u32, node: m, port: r, hops: vec![n] });
                }
            }
        }
        // Untyped ends go to the side their pin leaves from; top and bottom pins by pin kind.
        let left = match port.side {
            Side::Left => true,
            Side::Right => false,
            _ => port.kind == PortKind::In,
        };
        if left { out.drivers.extend(links) } else { out.loads.extend(links) }
    }
    for links in [&mut out.drivers, &mut out.loads] {
        links.sort_by(|a, b| {
            node.ports[b.focus_port as usize].at[1]
                .cmp(&node.ports[a.focus_port as usize].at[1])
                .then_with(|| a.focus_port.cmp(&b.focus_port))
                .then_with(|| a.node.cmp(&b.node))
                .then_with(|| a.port.cmp(&b.port))
                .then_with(|| a.hops.cmp(&b.hops))
        });
        links.dedup();
    }
    out
}

/// Where everything sits in the reveal, in integer schematic grid steps, y up.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealLayout {
    pub focus: u32,
    /// Each related node once; poses use the node's block and are marked placed.
    pub parts: Vec<(u32, Rel, PartPlace)>,
    /// Orthogonal links and rail columns, split at hop pins when the net changes.
    pub wires: Vec<sheet::Run>,
    /// Rail tags above columns, and each part's own rail and no-connect tags as the sheet draws
    /// them (stub and turn, neighbouring pins joined).
    pub tags: Vec<sheet::Tag>,
    pub junctions: Vec<(u32, Pt)>,
    /// Everything (bodies, wires and tag extents) plus a margin of three: the card fits its content.
    pub frame: Bx,
}

// Match sheet's mirror-before-counter-clockwise-turn convention without exposing its helpers.
fn rot(p: Pt, turn: u8, mirror: bool) -> Pt {
    let p = if mirror { [-p[0], p[1]] } else { p };
    match turn & 3 {
        0 => p,
        1 => [-p[1], p[0]],
        2 => [-p[0], -p[1]],
        _ => [p[1], -p[0]],
    }
}

fn bx_rot(b: Bx, turn: u8, mirror: bool) -> Bx {
    let a = rot([b[0], b[1]], turn, mirror);
    let c = rot([b[2], b[3]], turn, mirror);
    [a[0].min(c[0]), a[1].min(c[1]), a[0].max(c[0]), a[1].max(c[1])]
}

fn side_dir(s: Side) -> Pt {
    match s {
        Side::Left => [-1, 0],
        Side::Right => [1, 0],
        Side::Top => [0, 1],
        Side::Bottom => [0, -1],
    }
}

fn rot_side(s: Side, turn: u8, mirror: bool) -> Side {
    match rot(side_dir(s), turn, mirror) {
        [x, _] if x < 0 => Side::Left,
        [x, _] if x > 0 => Side::Right,
        [_, y] if y > 0 => Side::Top,
        _ => Side::Bottom,
    }
}

fn add(a: Pt, b: Pt) -> Pt {
    [a[0] + b[0], a[1] + b[1]]
}

fn bx_add(b: Bx, p: Pt) -> Bx {
    [b[0] + p[0], b[1] + p[1], b[2] + p[0], b[3] + p[1]]
}

fn union(a: Bx, b: Bx) -> Bx {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
}

fn near(a: Bx, b: Bx, gap: i32) -> bool {
    a[0] < b[2] + gap && b[0] < a[2] + gap && a[1] < b[3] + gap && b[1] < a[3] + gap
}

/// Whether the orthogonal segment `a`-`b` passes through the open interior of `b`.
fn enters(a: Pt, b: Pt, bx: Bx) -> bool {
    a[0].min(b[0]) < bx[2] && a[0].max(b[0]) > bx[0] && a[1].min(b[1]) < bx[3] && a[1].max(b[1]) > bx[1]
}

/// Whether `p` lies on the orthogonal segment `a`-`b`, ends included.
fn on_seg(p: Pt, a: Pt, b: Pt) -> bool {
    (a[0] == p[0] && b[0] == p[0] && a[1].min(b[1]) <= p[1] && p[1] <= a[1].max(b[1]))
        || (a[1] == p[1] && b[1] == p[1] && a[0].min(b[0]) <= p[0] && p[0] <= a[0].max(b[0]))
}

/// The clear distance between two boxes (0 when they touch or overlap).
fn box_gap(a: [f64; 4], b: [f64; 4]) -> f64 {
    let dx = (a[0] - b[2]).max(b[0] - a[2]).max(0.0);
    let dy = (a[1] - b[3]).max(b[1] - a[3]).max(0.0);
    dx.hypot(dy)
}

fn junctions_of(wires: &[sheet::Run], pins: &[(u32, Pt)]) -> Vec<(u32, Pt)> {
    let mut by_net: BTreeMap<u32, Vec<&sheet::Run>> = BTreeMap::new();
    // Candidate keys give unique points in (net, x, y) order; values count pins.
    let mut candidates = BTreeMap::new();
    for run in wires {
        by_net.entry(run.net).or_default().push(run);
        for &p in &run.pts {
            candidates.entry((run.net, p)).or_insert(0usize);
        }
    }
    for &pin in pins {
        *candidates.entry(pin).or_insert(0) += 1;
    }
    // A point is a junction when the net's wiring leaves it three or more distinct ways, at
    // least two of them wires (each pin is a way). Runs of one net that overlap, such as two
    // routes leaving a pin along the same lead, are one way, not two.
    let mut junctions = Vec::new();
    let mut ways: Vec<Pt> = Vec::new();
    for ((net, p), pin_count) in candidates {
        let Some(runs) = by_net.get(&net) else { continue };
        ways.clear();
        for run in runs {
            for w in run.pts.windows(2) {
                if p != w[0] && p != w[1] && !on_seg(p, w[0], w[1]) {
                    continue;
                }
                for end in [w[0], w[1]] {
                    let way = [(end[0] - p[0]).signum(), (end[1] - p[1]).signum()];
                    if way != [0, 0] && !ways.contains(&way) {
                        ways.push(way);
                    }
                }
            }
        }
        if ways.len() >= 2 && ways.len() + pin_count >= 3 {
            junctions.push((net, p));
        }
    }
    junctions
}

struct Scene<'a> {
    g: &'a LayoutGraph,
    sheet: &'a SheetLayout,
    draw: &'a sheet::PartDrawer<'a>,
    out: RevealLayout,
    index: Vec<Option<usize>>,
    /// Per placed part (as `out.parts`): its body with its rails (and an upright satellite's label
    /// room beside it), for spacing.
    ext: Vec<Bx>,
    /// The focus's supply pins the rail columns wire.
    focus_rails: Vec<usize>,
    nets: Vec<Vec<Option<u32>>>,
    /// Boxes of the continuation flags known before routing (pins whose net has no other pin in
    /// the reveal): routes keep clear of them.
    flag_boxes: Vec<[f64; 4]>,
}

impl Scene<'_> {
    /// A part's body with the rails it will draw, at the origin.
    fn extent(&self, n: u32, turn: u8, mirror: bool) -> Bx {
        let skip: &[usize] = if n == self.out.focus { &self.focus_rails } else { &[] };
        self.draw.rails(n, turn, mirror, skip).extent
    }

    fn ext_of(&self, n: u32) -> Bx {
        self.ext[self.index[n as usize].expect("related part is placed")]
    }

    fn pose(&self, n: u32) -> PartPlace {
        self.out.parts[self.index[n as usize].expect("related part is placed")].2
    }

    fn pin(&self, n: u32, p: u32) -> Pt {
        let pose = self.pose(n);
        add(pose.at, rot(self.g.nodes[n as usize].ports[p as usize].at, pose.turn, pose.mirror))
    }

    fn dir(&self, n: u32, p: u32) -> Pt {
        let pose = self.pose(n);
        side_dir(rot_side(self.g.nodes[n as usize].ports[p as usize].side, pose.turn, pose.mirror))
    }

    fn part(&self, n: u32, at: Pt, turn: u8, mirror: bool) -> PartPlace {
        PartPlace {
            at,
            turn,
            mirror,
            glyph: self.sheet.parts[n as usize].glyph,
            body: bx_add(bx_rot(self.g.nodes[n as usize].body, turn, mirror), at),
            block: self.g.nodes[n as usize].block,
            placed: true,
            quarter: self.g.nodes[n as usize].quarter,
            turn_step: self.sheet.parts[n as usize].turn_step,
        }
    }

    fn put(&mut self, n: u32, rel: Rel, pose: PartPlace) {
        if self.index[n as usize].is_none() {
            self.index[n as usize] = Some(self.out.parts.len());
            self.ext.push(bx_add(self.extent(n, pose.turn, pose.mirror), pose.at));
            self.out.parts.push((n, rel, pose));
        }
    }

    fn column(&mut self, link: &Link, rel: Rel, edge: i32, left: bool, bottom: &mut Option<i32>) -> bool {
        if self.index[link.node as usize].is_some() {
            return false;
        }
        let g = self.g;
        let node = &g.nodes[link.node as usize];
        // The linked pin faces the focus, so its run never crosses the part to reach it.
        let mirror = node.ports[link.port as usize].side == if left { Side::Left } else { Side::Right };
        let local = self.extent(link.node, 0, mirror);
        let x = edge - local[if left { 2 } else { 0 }];
        let mut y = self.pin(self.out.focus, link.focus_port)[1] - node.ports[link.port as usize].at[1];
        if let Some(b) = *bottom {
            y = y.min(b - 2 - local[3]);
        }
        // Satellites are fixed. If one reaches into a column, also clear it with the rails.
        loop {
            let e = bx_add(local, [x, y]);
            let below = self.ext.iter().filter(|&&q| near(e, q, 2)).map(|q| q[1] - 2 - local[3]).min();
            let Some(next) = below else { break };
            y = next;
        }
        let pose = self.part(link.node, [x, y], 0, mirror);
        *bottom = Some(local[1] + y);
        self.put(link.node, rel, pose);
        true
    }

    fn shift_column(&mut self, nodes: &[u32], dx: i32) {
        for &n in nodes {
            let i = self.index[n as usize].expect("column part is placed");
            let p = &mut self.out.parts[i].2;
            p.at[0] += dx;
            p.body = bx_add(p.body, [dx, 0]);
            self.ext[i] = bx_add(self.ext[i], [dx, 0]);
        }
    }

    fn hops(&mut self, link: &Link, left: &[u32], right: &[u32]) {
        if link.hops.iter().all(|&n| self.index[n as usize].is_some()) {
            return;
        }
        let start = self.pin(self.out.focus, link.focus_port);
        let end = self.pose(link.node);
        let on_left = end.at[0] < self.pose(self.out.focus).at[0];
        let dir = if on_left { -1 } else { 1 };
        let column = if on_left { left } else { right };
        let movable = column.contains(&link.node);
        // Hops stand in line with their end, so parallel paths fan out as rows, not a staircase.
        let row = if movable { self.pin(link.node, link.port)[1] } else { start[1] };
        let first_net = self.nets[self.out.focus as usize][link.focus_port as usize];
        // Use the end's near edge: a pin on its far side must not put a hop inside it.
        let mut end_x = self.ext_of(link.node)[if on_left { 2 } else { 0 }];
        loop {
            let mut pending: Vec<(u32, PartPlace, Bx)> = Vec::new();
            let mut clash = false;
            for (i, &n) in link.hops.iter().enumerate() {
                if self.index[n as usize].is_some() || pending.iter().any(|&(p, _, _)| p == n) {
                    continue;
                }
                let node = &self.g.nodes[n as usize];
                let level = node.ports.iter().all(|p| matches!(p.side, Side::Left | Side::Right));
                let mut turn = if level { 0 } else { 1 };
                // The first hop of a level two-pin part takes the path on its focus-facing end.
                if i == 0 && level && node.ports.len() == 2 {
                    if let Some(entry) = first_net.and_then(|net| self.entry(n, net)) {
                        let facing = (node.ports[entry as usize].at[0] < 0) == (dir > 0);
                        if !facing {
                            turn = 2;
                        }
                    }
                }
                // Hops sit just before their end, six steps each, leaving the lanes by the focus free.
                let x = end_x - dir * (6 * (link.hops.len() - i) as i32 - 2);
                let pose = self.part(n, [x, row], turn, false);
                let e = bx_add(self.extent(n, turn, false), pose.at);
                clash |= self.ext.iter().any(|&q| near(e, q, 1)) || pending.iter().any(|&(_, _, q)| near(e, q, 1));
                pending.push((n, pose, e));
            }
            if !clash {
                for (n, pose, _) in pending {
                    self.put(n, Rel::Hop, pose);
                }
                break;
            }
            // Large symbols or another path's hops may need more than the nominal gap.
            // Fixed molecule members retain their sheet pose; only new hops move in that case.
            end_x += 6 * dir;
            if movable {
                self.shift_column(column, 6 * dir);
            }
        }
    }

    fn wire(&mut self, net: u32, mut pts: Vec<Pt>) {
        pts.dedup();
        if pts.len() >= 2 {
            let run = sheet::Run { net, pts };
            if !self.out.wires.contains(&run) {
                self.out.wires.push(run);
            }
        }
    }

    fn route(&mut self, from: (u32, u32), to: Pt, net: u32, column: Option<i32>) {
        let at = self.pin(from.0, from.1);
        let dir = self.dir(from.0, from.1);
        let lead = add(at, dir);
        if column.is_some() && dir[0] != 0 {
            // A side supply pin rises outside its body before turning toward the next part.
            self.wire(net, vec![at, lead, [lead[0], to[1]], to]);
            return;
        }
        let x = column.unwrap_or_else(|| {
            // The middle column, or the nearest one between the leads that keeps off the flags.
            let mid = lead[0] + (to[0] - lead[0]) / 2;
            let (lo, hi) = (lead[0].min(to[0]), lead[0].max(to[0]));
            (0..=hi - lo).flat_map(|k| [mid - k, mid + k])
                .filter(|x| (lo..=hi).contains(x))
                .find(|&x| self.clear_of_flags(&[at, lead, [x, lead[1]], [x, to[1]], to]))
                .unwrap_or(mid)
        });
        self.wire(net, vec![at, lead, [x, lead[1]], [x, to[1]], to]);
    }

    /// Whether a path keeps off every continuation flag known before routing.
    fn clear_of_flags(&self, pts: &[Pt]) -> bool {
        pts.windows(2).all(|w| self.flag_boxes.iter().all(|&b| sheet::segment_box_distance(w[0], w[1], b) >= 0.1))
    }

    /// A route whose vertical runs at `x`: the link's own lane.
    fn route_x(&mut self, from: (u32, u32), to: Pt, net: u32, x: i32) {
        let at = self.pin(from.0, from.1);
        let lead = add(at, self.dir(from.0, from.1));
        self.wire(net, vec![at, lead, [x, lead[1]], [x, to[1]], to]);
    }

    /// Whether a lane run from `from` across to `x`, along it to `to`'s height and on to `to`
    /// keeps off every placed part but those in `skip`: no body (with its rails) entered, no
    /// pin touched.
    fn lane_clear(&self, skip: [u32; 2], from: Pt, x: i32, to: Pt) -> bool {
        let segs = [(from, [x, from[1]]), ([x, from[1]], [x, to[1]]), ([x, to[1]], to)];
        self.clear_of_flags(&[from, [x, from[1]], [x, to[1]], to]) && self.out.parts.iter().zip(&self.ext).all(|(&(n, _, pose), &e)| {
            skip.contains(&n) || segs.iter().all(|&(a, b)| {
                !enters(a, b, e) && !self.g.nodes[n as usize].ports.iter()
                    .any(|port| on_seg(add(pose.at, rot(port.at, pose.turn, pose.mirror)), a, b))
            })
        })
    }

    /// The point a link's first run reaches: its first hop's entry, else its end.
    fn first_target(&self, link: &Link) -> Option<(u32, Pt)> {
        match link.hops.first() {
            Some(&h) => {
                let net = self.nets[self.out.focus as usize][link.focus_port as usize]?;
                Some((h, self.pin(h, self.entry(h, net)?)))
            }
            None => Some((link.node, self.pin(link.node, link.port))),
        }
    }

    fn entry(&self, n: u32, net: u32) -> Option<u32> {
        self.nets[n as usize].iter().position(|&v| v == Some(net)).map(|p| p as u32)
    }

    fn exit(&self, n: u32, entry: u32, next: u32, port: Option<u32>) -> Option<u32> {
        self.nets[n as usize].iter().enumerate().find_map(|(p, &net)| {
            let net = net?;
            if p as u32 == entry || self.g.nets[net as usize].kind == NetKind::Ground {
                return None;
            }
            let matches = match port {
                Some(q) => self.nets[next as usize][q as usize] == Some(net),
                None => self.nets[next as usize].contains(&Some(net)),
            };
            matches.then_some(p as u32)
        })
    }

    fn link_wire(&mut self, link: &Link, lane: Option<i32>) {
        let mut from = (self.out.focus, link.focus_port);
        for (i, &n) in link.hops.iter().enumerate() {
            let Some(net) = self.nets[from.0 as usize][from.1 as usize] else { return };
            let Some(entry) = self.entry(n, net) else { return };
            match lane.filter(|_| i == 0) {
                Some(x) => self.route_x(from, self.pin(n, entry), net, x),
                None => self.route(from, self.pin(n, entry), net, None),
            }
            let (next, port) = match link.hops.get(i + 1) {
                Some(&next) => (next, None),
                None => (link.node, Some(link.port)),
            };
            let Some(exit) = self.exit(n, entry, next, port) else { return };
            from = (n, exit);
        }
        let Some(net) = self.nets[from.0 as usize][from.1 as usize] else { return };
        if self.nets[link.node as usize][link.port as usize] == Some(net) {
            match lane.filter(|_| link.hops.is_empty()) {
                Some(x) => self.route_x(from, self.pin(link.node, link.port), net, x),
                None => self.route(from, self.pin(link.node, link.port), net, None),
            }
        }
    }

    fn rail_wire(&mut self, rail: &RailCol, pin: u32, x: i32) -> (u32, u32) {
        let mut from = (self.out.focus, pin);
        for (i, &n) in rail.chain.iter().enumerate() {
            if n == from.0 {
                continue;
            }
            let Some(net) = self.nets[from.0 as usize][from.1 as usize] else { break };
            let Some(entry) = self.entry(n, net) else { break };
            let to = self.pin(n, entry);
            let (out, into) = (self.dir(from.0, from.1), self.dir(n, entry));
            if out[1] > 0 && into[1] < 0 {
                // An up pin to the down pin above: rise at the source's lead and cross on the
                // target's lead row, below its pins, so the run never touches its other pins.
                // The crossing row steps further from the target while a continuation flag by
                // its pins is in the way, never past the source's lead.
                let at = self.pin(from.0, from.1);
                let (lead, into_lead) = (add(at, out), add(to, into));
                let row = (0..=(into_lead[1] - lead[1]).max(0))
                    .map(|k| into_lead[1] - k)
                    .find(|&y| self.clear_of_flags(&[lead, [lead[0], y], [to[0], y], to]))
                    .unwrap_or(into_lead[1]);
                self.wire(net, vec![at, lead, [lead[0], row], [to[0], row], to]);
            } else {
                self.route(from, to, net, Some(x));
            }
            from = (n, entry);
            if let Some(&next) = rail.chain.get(i + 1) {
                let Some(exit) = self.exit(n, entry, next, None) else { break };
                from = (n, exit);
            }
        }
        from
    }
}

/// Arrange relations around the focus at the origin, retaining its sheet turn and mirror.
/// `sheet` and `rel` must use the indices of `g`. A box symbol the reveal turns an odd number of
/// quarters from its fit takes its other fit (`adapt::sheet_unit`), so its pin names still fit: the
/// reveal is laid out again with those fits (turns come from connectivity, so they stay).
pub fn reveal(g: &LayoutGraph, sheet: &SheetLayout, rel: &Relations) -> RevealLayout {
    let first = reveal_in(g, sheet, rel);
    let turned: Vec<u32> = first.parts.iter()
        .filter(|(n, kind, p)| {
            let node = &g.nodes[*n as usize];
            !matches!(kind, Rel::Focus | Rel::Satellite) && node.refit.is_some() && (p.turn & 1 == 1) != node.quarter
        })
        .map(|(n, _, _)| *n)
        .collect();
    if turned.is_empty() {
        return first;
    }
    let mut fitted = g.clone();
    for n in turned {
        fitted.nodes[n as usize].flip_fit();
    }
    reveal_in(&fitted, sheet, rel)
}

fn reveal_in(g: &LayoutGraph, sheet: &SheetLayout, rel: &Relations) -> RevealLayout {
    let opts = sheet::Opts::default();
    let draw = sheet::PartDrawer::new(g, &opts);
    let nets = g.port_nets();
    let focus_rails = g.nodes[rel.focus as usize].ports.iter().enumerate()
        .filter(|(p, port)| port.kind == PortKind::PowerIn && rel.rails.iter().any(|r| nets[rel.focus as usize][*p] == Some(r.rail)))
        .map(|(p, _)| p).collect();
    let mut s = Scene {
        g,
        sheet,
        draw: &draw,
        out: RevealLayout {
            focus: rel.focus, parts: Vec::new(), wires: Vec::new(), tags: Vec::new(), junctions: Vec::new(),
            frame: [0; 4],
        },
        index: vec![None; g.nodes.len()],
        ext: Vec::new(),
        focus_rails,
        nets,
        flag_boxes: Vec::new(),
    };
    let focus = sheet.parts[rel.focus as usize];
    s.put(rel.focus, Rel::Focus, s.part(rel.focus, [0, 0], focus.turn, focus.mirror));
    for &n in &rel.satellites {
        let p = sheet.parts[n as usize];
        let at = [p.at[0] - focus.at[0], p.at[1] - focus.at[1]];
        s.put(n, Rel::Satellite, s.part(n, at, p.turn, p.mirror));
        // A small upright part's label goes beside it, as on the sheet (its first spot): its extent
        // keeps that room, so lanes and columns leave it free and the label needs no leader.
        let node = &g.nodes[n as usize];
        if node.role.is_small() && p.turn & 1 == 1 {
            let gap = crate::labels::LabelOpts::default().gap;
            let label = node.value.as_deref().map_or(0.0, |v| sheet::text_w(v, sheet::TEXT_VALUE))
                .max(sheet::text_w(&node.name, sheet::TEXT_REFDES));
            let i = s.index[n as usize].expect("satellite is placed");
            let body = s.out.parts[i].2.body;
            // One more step: a lane on the extent's edge keeps the label's gap from it too.
            s.ext[i][2] = s.ext[i][2].max((body[2] as f64 + 2.0 * gap + label).ceil() as i32 + 1);
        }
    }
    // The molecule keeps its sheet wires (a pull-down's stub to its pin): every sheet run
    // whose two ends are pins of the focus or its satellites.
    let members: Vec<u32> = std::iter::once(rel.focus).chain(rel.satellites.iter().copied()).collect();
    let member_pin = |at: Pt| members.iter().any(|&n| {
        let p = sheet.parts[n as usize];
        g.nodes[n as usize].ports.iter().any(|port| add(p.at, rot(port.at, p.turn, p.mirror)) == at)
    });
    for run in &sheet.wires {
        if let (Some(&a), Some(&b)) = (run.pts.first(), run.pts.last()) {
            if member_pin(a) && member_pin(b) {
                s.wire(run.net, run.pts.iter().map(|p| [p[0] - focus.at[0], p[1] - focus.at[1]]).collect());
            }
        }
    }
    let body = s.pose(rel.focus).body;
    let fext = s.ext_of(rel.focus);
    let controller_links: Vec<Link> = rel.controllers.iter().map(|&(fp, n, p)| {
        rel.drivers.iter().chain(&rel.loads)
            .find(|l| (l.focus_port, l.node, l.port) == (fp, n, p)).cloned()
            .unwrap_or(Link { focus_port: fp, node: n, port: p, hops: Vec::new() })
    }).collect();
    // Room for the hops in front of each column and one lane per link by the focus.
    let extra = |links: &[&Link]| {
        links.iter().map(|l| l.hops.len() as i32 * 6).max().unwrap_or(0) + links.len() as i32
    };
    // The columns stand clear of the focus and its satellites (with their label room), so the
    // lanes between them never run through a satellite's label.
    let held = rel.satellites.iter().map(|&n| s.ext_of(n)).fold(fext, union);
    let left_edge = held[0] - 8 - extra(&controller_links.iter().chain(&rel.drivers).collect::<Vec<_>>());
    let right_edge = held[2] + 8 + extra(&rel.loads.iter().collect::<Vec<_>>());
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut bottom = None;
    for (links, kind) in [(&controller_links, Rel::Controller), (&rel.drivers, Rel::Driver)] {
        for link in links {
            if s.column(link, kind.clone(), left_edge, true, &mut bottom) {
                left.push(link.node);
            }
        }
    }
    bottom = None;
    for link in &rel.loads {
        if s.column(link, Rel::Load, right_edge, false, &mut bottom) {
            right.push(link.node);
        }
    }
    let mut links = controller_links;
    for link in rel.drivers.iter().chain(&rel.loads) {
        if !links.contains(link) {
            links.push(link.clone());
        }
    }
    for link in &links {
        s.hops(link, &left, &right);
    }

    let mut rail_tops = Vec::new();
    for (i, rail) in rel.rails.iter().enumerate() {
        let x = 4 * i as i32 - 2 * (rel.rails.len() as i32 - 1);
        let mut y = fext[3] + 4;
        // Each chain part turns its entry (the net from below) down, so its runs never pass behind it.
        let mut net_in = Some(rail.rail);
        for (ci, &n) in rail.chain.iter().enumerate() {
            let entry = net_in.and_then(|net| s.entry(n, net));
            let turn = entry.map_or(0, |e| {
                let side = g.nodes[n as usize].ports[e as usize].side;
                (0..4u8).find(|&t| rot_side(side, t, false) == Side::Bottom).unwrap_or(0)
            });
            net_in = entry
                .and_then(|e| rail.chain.get(ci + 1).and_then(|&next| s.exit(n, e, next, None)))
                .and_then(|x| s.nets[n as usize][x as usize]);
            if s.index[n as usize].is_none() {
                let local = s.extent(n, turn, false);
                let mut at = [x, y - local[1]];
                loop {
                    let b = bx_add(local, at);
                    let above = s.ext.iter().filter(|&&q| near(b, q, 3)).map(|q| q[3] + 3 - local[1]).max();
                    let Some(next) = above else { break };
                    at[1] = next;
                }
                s.put(n, Rel::Power, s.part(n, at, turn, false));
            }
            y = y.max(s.ext_of(n)[3] + 3);
        }
        rail_tops.push([x, y]);
    }
    // Route only after every column has its final position. Each link whose first run changes
    // height gets its own lane by the focus: downward runs with the lowest pin turning first,
    // then upward runs with the highest first, so parallel runs never share or cross a vertical.
    // A signal pin whose net leaves the reveal and meets no other part in it takes a
    // continuation flag; the routes below keep clear of those flags.
    let outside: Vec<bool> = g.nets.iter()
        .map(|net| net.pins.iter().any(|&(n, _)| s.index[n as usize].is_none()))
        .collect();
    let mut flag_boxes = Vec::new();
    for &(n, _, _) in &s.out.parts {
        for (p, &net) in s.nets[n as usize].iter().enumerate() {
            let Some(net) = net else { continue };
            let gnet = &g.nets[net as usize];
            if g.nodes[n as usize].ports[p].nc || gnet.kind != NetKind::Signal || !outside[net as usize]
                || gnet.pins.iter().any(|&(m, _)| m != n && s.index[m as usize].is_some())
            {
                continue;
            }
            flag_boxes.push(sheet::tag_item_box(&sheet::Tag {
                net: Some(net), at: s.pin(n, p as u32), dir: s.dir(n, p as u32), kind: sheet::TagKind::Label,
                text: sheet::short_name(&gnet.name), band: None, side: 0,
            }));
        }
    }
    s.flag_boxes = flag_boxes;
    let focus_x = s.pose(rel.focus).at[0];
    let mut lanes: Vec<Option<i32>> = vec![None; links.len()];
    for right_side in [false, true] {
        let mut down: Vec<(i32, usize)> = Vec::new();
        let mut up: Vec<(i32, usize)> = Vec::new();
        for (i, link) in links.iter().enumerate() {
            let Some((target_node, target)) = s.first_target(link) else { continue };
            if (s.pose(target_node).at[0] > focus_x) != right_side {
                continue;
            }
            let pin = s.pin(rel.focus, link.focus_port);
            if target[1] < pin[1] {
                down.push((pin[1], i));
            } else if target[1] > pin[1] {
                up.push((pin[1], i));
            }
        }
        down.sort();
        up.sort_by(|a, b| b.cmp(a));
        let dir = if right_side { 1 } else { -1 };
        // A lane keeps off the bodies and pins of fixed parts (a satellite by the focus): it
        // moves further out, never past its target; when nothing is clear the nominal lane
        // stands. Later lanes stay outside it.
        let mut next = 1;
        for &(_, i) in down.iter().chain(&up) {
            let fp = links[i].focus_port;
            let pin = s.pin(rel.focus, fp);
            let lead = pin[0] + s.dir(rel.focus, fp)[0];
            let Some((target_node, target)) = s.first_target(&links[i]) else { continue };
            let step = (next..)
                .take_while(|&k| (lead + dir * k - target[0]) * dir < 0)
                .find(|&k| s.lane_clear([rel.focus, target_node], [lead, pin[1]], lead + dir * k, target))
                .unwrap_or(next);
            lanes[i] = Some(lead + dir * step);
            next = step + 1;
        }
    }
    for (link, lane) in links.iter().zip(lanes) {
        s.link_wire(link, lane);
    }
    for (rail, &at) in rel.rails.iter().zip(&rail_tops) {
        let pins: Vec<u32> = g.nodes[rel.focus as usize].ports.iter().enumerate()
            .filter(|(p, port)| port.kind == PortKind::PowerIn && s.nets[rel.focus as usize][*p] == Some(rail.rail))
            .map(|(p, _)| p as u32).collect();
        let mut tag_net = rail.rail;
        let mut sourced = false;
        for p in pins {
            let end = s.rail_wire(rail, p, at[0]);
            // A chain that reaches its source part ends there: the part is the rail's origin.
            if rail.source.is_some_and(|src| src == end.0 && src != rel.focus) {
                sourced = true;
                continue;
            }
            if let Some(net) = s.nets[end.0 as usize][end.1 as usize] {
                tag_net = net;
                s.route(end, at, net, Some(at[0]));
            }
        }
        if sourced {
            continue;
        }
        s.out.tags.push(sheet::Tag {
            net: Some(tag_net), at, dir: [0, 1], kind: sheet::TagKind::Power,
            text: sheet::short_name(&g.nets[tag_net as usize].name),
            band: None,
            side: 0,
        });
    }
    // Each part's own rails and no-connect pins as the sheet draws them; pins the reveal wired
    // keep their wires.
    let skips: Vec<(u32, PartPlace, Vec<usize>)> = s.out.parts.iter().map(|&(n, _, pose)| {
        let skip = (0..g.nodes[n as usize].ports.len()).filter(|&p| {
            let at = s.pin(n, p as u32);
            s.out.wires.iter().any(|r| r.pts.first() == Some(&at) || r.pts.last() == Some(&at))
        }).collect();
        (n, pose, skip)
    }).collect();
    for (n, pose, skip) in skips {
        let r = s.draw.rails(n, pose.turn, pose.mirror, &skip);
        for run in r.wires {
            s.wire(run.net, run.pts.iter().map(|&p| add(p, pose.at)).collect());
        }
        for mut t in r.tags {
            t.at = add(t.at, pose.at);
            s.out.tags.push(t);
        }
    }
    // A bare signal pin whose net leaves the reveal continues elsewhere by name.
    let mut flags = Vec::new();
    for &(n, _, _) in &s.out.parts {
        for (p, &net) in s.nets[n as usize].iter().enumerate() {
            let Some(net) = net else { continue };
            if g.nodes[n as usize].ports[p].nc || g.nets[net as usize].kind != NetKind::Signal || !outside[net as usize] {
                continue;
            }
            let at = s.pin(n, p as u32);
            if s.out.wires.iter().any(|run| run.pts.first() == Some(&at) || run.pts.last() == Some(&at)
                || run.pts.windows(2).any(|w| on_seg(at, w[0], w[1])))
                || s.out.tags.iter().any(|tag| tag.at == at)
            {
                continue;
            }
            flags.push((net, at, s.dir(n, p as u32)));
        }
    }
    // A flag that would meet another tag, a body or another net's wire steps out along its pin,
    // on a stub of its net, until it is clear (neighbouring pins' flags stagger). The search ends
    // where the stub would cross something, since stepping further cannot clear it; the flag then
    // takes the clearest reach it found.
    let bodies: Vec<[f64; 4]> = s.out.parts.iter().map(|(_, _, p)| p.body.map(f64::from)).collect();
    for (net, pin, dir) in flags {
        let pin_f = pin.map(f64::from);
        let others: Vec<[f64; 4]> = bodies.iter().copied()
            .filter(|b| !(b[0] <= pin_f[0] && pin_f[0] <= b[2] && b[1] <= pin_f[1] && pin_f[1] <= b[3]))
            .chain(s.out.tags.iter().map(sheet::tag_item_box))
            .chain(s.out.wires.iter().filter(|run| run.net != net).flat_map(|run| run.pts.windows(2).map(|w| {
                [w[0][0].min(w[1][0]), w[0][1].min(w[1][1]), w[0][0].max(w[1][0]), w[0][1].max(w[1][1])].map(f64::from)
            })))
            .collect();
        let mut tag = sheet::Tag {
            net: Some(net), at: pin, dir, kind: sheet::TagKind::Label,
            text: sheet::short_name(&g.nets[net as usize].name), band: None, side: 0,
        };
        let (mut best, mut best_gap, mut reach) = (0, f64::NEG_INFINITY, 0);
        loop {
            tag.at = add(pin, [dir[0] * reach, dir[1] * reach]);
            if reach > 0 && (dir == [0, 0] || others.iter().any(|&o| sheet::segment_box_distance(pin, tag.at, o) < 0.1)) {
                break;
            }
            let b = sheet::tag_item_box(&tag);
            let gap = others.iter().map(|&o| box_gap(b, o)).fold(f64::INFINITY, f64::min);
            if gap > best_gap {
                (best, best_gap) = (reach, gap);
            }
            if gap >= 0.1 {
                break;
            }
            reach += 2;
        }
        let reach = best;
        tag.at = add(pin, [dir[0] * reach, dir[1] * reach]);
        if reach > 0 {
            s.wire(net, vec![pin, tag.at]);
        }
        s.out.tags.push(tag);
    }
    let bodies: Vec<_> = s.out.parts.iter().map(|(_, _, p)| p.body).collect();
    sheet::settle_tag_names(&mut s.out.tags, &s.out.wires, &bodies, &[]);
    let mut pins = Vec::new();
    for &(n, _, _) in &s.out.parts {
        for (p, &net) in s.nets[n as usize].iter().enumerate() {
            if let Some(net) = net {
                pins.push((net, s.pin(n, p as u32)));
            }
        }
    }
    s.out.junctions = junctions_of(&s.out.wires, &pins);
    let mut bounds = body;
    for (_, _, p) in &s.out.parts {
        bounds = union(bounds, p.body);
    }
    for run in &s.out.wires {
        for &p in &run.pts {
            bounds = union(bounds, [p[0], p[1], p[0], p[1]]);
        }
    }
    for t in &s.out.tags {
        bounds = union(bounds, sheet::tag_box(t));
    }
    s.out.frame = [bounds[0] - 3, bounds[1] - 3, bounds[2] + 3, bounds[3] + 3];
    s.out
}

/// A reveal in sheet coordinates, retaining the graph's component indices.
#[derive(Clone, Debug, PartialEq)]
pub struct RevealSheet {
    pub focus: u32,
    pub sheet: SheetLayout,
    pub rels: Vec<(u32, Rel)>,
    /// The card: the layout's frame, grown to hold the labels.
    pub frame: Bx,
}

pub fn reveal_sheet(d: &Design, unit: usize, g: &LayoutGraph, sheet: &SheetLayout, focus: u32) -> RevealSheet {
    use crate::labels::{self, LabelOpts, LabelReq, Obstacle, Spot};
    use sheet::{Label, Owner, TextRole, LINE_GAP, TEXT_REFDES, TEXT_VALUE};

    let rel = relations(d, unit, g, focus);
    let layout = reveal(g, sheet, &rel);
    let origin = sheet.parts[focus as usize].at;
    let mut out = RevealSheet {
        focus,
        sheet: SheetLayout { parts: vec![PartPlace::default(); g.nodes.len()], ..SheetLayout::default() },
        rels: Vec::new(),
        frame: bx_add(layout.frame, origin),
    };
    for (n, rel, mut p) in layout.parts {
        p.at = add(p.at, origin);
        p.body = bx_add(p.body, origin);
        out.sheet.parts[n as usize] = p;
        out.rels.push((n, rel));
    }
    out.sheet.wires = layout.wires;
    for run in &mut out.sheet.wires {
        for p in &mut run.pts {
            *p = add(*p, origin);
        }
    }
    out.sheet.tags = layout.tags;
    for tag in &mut out.sheet.tags {
        tag.at = add(tag.at, origin);
    }
    out.sheet.junctions = layout.junctions.into_iter().map(|(net, p)| (net, add(p, origin))).collect();
    let mut obstacles = Vec::new();
    let mut reqs = Vec::new();
    let mut texts = Vec::new();
    for (n, p) in out.sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
        let node = &g.nodes[n];
        obstacles.push(Obstacle::Box { b: p.body.map(f64::from), owner: n as u32 });
        let mut lines = vec![(node.name.clone(), TextRole::Refdes)];
        if let Some(value) = &node.value {
            lines.push((value.clone(), TextRole::Value));
        }
        let w = lines.iter().map(|(text, role)| sheet::text_w(text,
            if *role == TextRole::Refdes { TEXT_REFDES } else { TEXT_VALUE })).fold(0.0, f64::max);
        let h = if lines.len() > 1 { TEXT_REFDES + TEXT_VALUE + LINE_GAP * TEXT_VALUE } else { TEXT_REFDES };
        let spots = if node.role.is_small() && p.turn & 1 == 1 {
            vec![Spot::Right, Spot::Left, Spot::RightTop, Spot::LeftTop, Spot::Above, Spot::Below]
        } else if node.role.is_small() {
            vec![Spot::Above, Spot::Below, Spot::AboveLeft, Spot::BelowLeft, Spot::Right, Spot::Left]
        } else {
            vec![Spot::AboveLeft, Spot::Above, Spot::BelowLeft, Spot::RightTop, Spot::LeftTop]
        };
        reqs.push(LabelReq {
            anchor: p.body.map(f64::from), size: [w, h], spots,
            priority: if node.role.is_small() { 1 } else { 2 }, owner: n as u32, optional: false,
        });
        texts.push((Owner::Part(n as u32), lines));
    }
    for tag in &out.sheet.tags {
        obstacles.push(Obstacle::Box { b: sheet::tag_item_box(tag), owner: u32::MAX });
    }
    for run in &out.sheet.wires {
        for pair in run.pts.windows(2) {
            // Owned by its net, as on the sheet: a net's strip may sit beside its own wire.
            obstacles.push(Obstacle::Seg {
                a: pair[0].map(f64::from), b: pair[1].map(f64::from), half: 0.15, owner: sheet::NET_OWNER + run.net,
            });
        }
    }
    // One display per net: a stack replaces its members' strips only when the whole bus
    // is on the card. If it finds no room, its members keep their traces on hover.
    let mut bundled = Vec::new();
    for (b, bus) in g.buses.iter().enumerate() {
        let members = &bus.nets;
        if members.iter().filter(|&&net| g.nets[net as usize].kind == NetKind::Signal).count() < 2
            || !members.iter().all(|&net| out.sheet.wires.iter().any(|run| run.net == net))
        {
            continue;
        }
        // Anchored on the bus's top line where it fans out of the part most of its runs end on
        // (its driver on this card): the topmost member run leaving that part, along its first
        // straight stretch. An anchor blocks every other request, so the whole fan would push
        // the lamps' and caps' labels away.
        let runs: Vec<&sheet::Run> = out.sheet.wires.iter().filter(|run| members.contains(&run.net)).collect();
        let on = |body: Bx, p: Pt| body[0] <= p[0] && p[0] <= body[2] && body[1] <= p[1] && p[1] <= body[3];
        let leaving = |body: Bx| -> Vec<Vec<Pt>> {
            runs.iter().filter(|run| run.pts.len() >= 2).filter_map(|run| {
                if on(body, run.pts[0]) {
                    Some(run.pts.clone())
                } else {
                    on(body, run.pts[run.pts.len() - 1]).then(|| run.pts.iter().rev().copied().collect())
                }
            }).collect()
        };
        let hub = out.sheet.parts.iter().filter(|p| p.placed).map(|p| p.body)
            .max_by(|a, b| leaving(*a).len().cmp(&leaving(*b).len()).then(Ordering::Greater))
            .filter(|&body| leaving(body).len() >= 2);
        let fold = |bx: [f64; 4], p: &Pt| {
            let p = p.map(f64::from);
            [bx[0].min(p[0]), bx[1].min(p[1]), bx[2].max(p[0]), bx[3].max(p[1])]
        };
        let empty = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let anchor = match hub {
            Some(body) => {
                let runs = leaving(body);
                let top = runs.iter().max_by(|a, b| a[0][1].cmp(&b[0][1]).then(Ordering::Greater))
                    .expect("a hub has member runs leaving it");
                // A horizontal stretch keeps its y, a vertical one its x.
                let axis = usize::from(top[1][1] == top[0][1]);
                // Grown past the wire's half width (0.15), so a spot beside the line clears it.
                let line = top.iter().take_while(|p| p[axis] == top[0][axis]).fold(empty, fold);
                [line[0] - 0.2, line[1] - 0.2, line[2] + 0.2, line[3] + 0.2]
            }
            None => runs.iter().flat_map(|run| run.pts.iter()).fold(empty, fold),
        };
        let b = b as u32;
        bundled.push(b);
        reqs.push(LabelReq {
            anchor,
            size: [sheet::TRACE_W, sheet::BUNDLE_ROW * members.len() as f64 + 0.4],
            spots: vec![Spot::Above, Spot::AboveRight, Spot::AboveLeft, Spot::Below, Spot::BelowRight, Spot::BelowLeft,
                Spot::Right, Spot::Left],
            priority: -1,
            owner: sheet::BUNDLE_OWNER + b,
            optional: true,
        });
        texts.push((Owner::Bundle(b), Vec::new()));
    }
    // A live strip beside each remaining signal net, as on the sheet (`sheet::strip_request`).
    let mut seen: Vec<u32> = bundled.iter().flat_map(|&b| g.buses[b as usize].nets.iter().copied()).collect();
    for run in &out.sheet.wires {
        let net = run.net;
        if seen.contains(&net) || g.nets[net as usize].kind != NetKind::Signal {
            continue;
        }
        seen.push(net);
        let pieces = out.sheet.wires.iter().filter(|wire| wire.net == net)
            .flat_map(|wire| wire.pts.windows(2).map(|pair| (pair[0], pair[1])));
        if let Some(req) = sheet::strip_request(sheet::NET_OWNER + net, pieces) {
            reqs.push(req);
            texts.push((Owner::Trace(net), Vec::new()));
        }
    }
    let placed = labels::place_labels(&reqs, &obstacles, &LabelOpts::default());
    let mut card: Vec<Label> = Vec::with_capacity(texts.len());
    for (i, (owner, lines)) in texts.into_iter().enumerate() {
        // A strip sits beside its wire or not at all: one that had to slide away is left out.
        if placed.left_out[i] || (matches!(owner, Owner::Trace(_) | Owner::Bundle(_)) && placed.leader[i].is_some()) {
            continue;
        }
        card.push(Label { owner, lines, bx: placed.boxes[i], leader: placed.leader[i] });
    }
    // The sheet's array rule, over the nets the card draws.
    let carried: Vec<u32> = card.iter().filter_map(|label| match label.owner {
        Owner::Bundle(b) => Some(b),
        _ => None,
    }).flat_map(|b| g.buses[b as usize].nets.iter().copied()).collect();
    sheet::whole_arrays(g, &mut card, |net| seen.contains(&net), &carried);
    for label in card {
        let bx = label.bx;
        out.frame = union(out.frame, [bx[0].floor() as i32 - 1, bx[1].floor() as i32 - 1,
            bx[2].ceil() as i32 + 1, bx[3].ceil() as i32 + 1]);
        out.sheet.labels.push(label);
    }
    out.sheet.bounds = out.frame;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::SheetHints;

    #[test]
    fn reveal_sheet_keeps_the_mcu_at_home_and_labels_clear() {
        let d = fab_elec_fixtures::ledctl();
        let circuit = &d.units[0].circuit;
        let focus = circuit.comps.iter().find(|comp| comp.part.kind == fab_elec_parts::PartKind::Mcu)
            .expect("fixture MCU chip").id.0;
        let hints = SheetHints::default();
        let g = crate::adapt::graph_of(circuit, &hints);
        let sheet = crate::layout(&d, 0, &hints, &crate::Opts::default());
        let result = reveal_sheet(&d, 0, &g, &sheet, focus);
        assert_eq!(result.sheet.parts[focus as usize].at, sheet.parts[focus as usize].at, "focus stays on the map");
        for (n, p) in result.sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
            assert!(contains(result.frame, p.body), "frame contains revealed body {n}");
        }
        let overlaps = sheet::overlaps(&result.sheet);
        assert!(overlaps.is_empty(), "reveal bodies, labels and tags are clear: {overlaps:?}");
        let strips: Vec<_> = result.sheet.labels.iter()
            .filter(|label| matches!(label.owner, sheet::Owner::Trace(_))).collect();
        let bundles: Vec<_> = result.sheet.labels.iter().filter_map(|label| match label.owner {
            sheet::Owner::Bundle(b) => Some((b, label)),
            _ => None,
        }).collect();
        let lamps: Vec<_> = bundles.iter().filter(|(b, _)| {
            let names: Vec<&str> = g.buses[*b as usize].nets.iter().map(|&net| g.nets[net as usize].name.as_str()).collect();
            names.len() == 8 && (0..8).all(|i| names.contains(&format!("LED{i}").as_str()))
        }).collect();
        assert_eq!(lamps.len(), 1, "reveal bundle: the lamps' bus (LED0..LED7) rides one stacked strip");
        let bundle_names: Vec<&str> = bundles.iter().map(|(b, _)| g.buses[*b as usize].name.as_str()).collect();
        for &(b, bundle) in &bundles {
            let members = &g.buses[b as usize].nets;
            assert!((bundle.bx[2] - bundle.bx[0] - sheet::TRACE_W).abs() < 1e-9,
                "reveal bundle: a stack has the strip width");
            assert!((bundle.bx[3] - bundle.bx[1] - (members.len() as f64 * sheet::BUNDLE_ROW + 0.4)).abs() < 1e-9,
                "reveal bundle: a stack has one row per member and padding");
            assert!(bundle.leader.is_none(), "reveal bundle: a stack has no leader");
            assert!(!strips.iter().any(|label| matches!(label.owner, sheet::Owner::Trace(net) if members.contains(&net))),
                "reveal bundle: its members carry no single strips");
        }
        let names: Vec<&str> = strips.iter().filter_map(|label| match label.owner {
            sheet::Owner::Trace(net) => Some(g.nets[net as usize].name.as_str()),
            _ => None,
        }).collect();
        let displayed = strips.len() + bundles.iter().map(|(b, _)| g.buses[*b as usize].nets.len()).sum::<usize>();
        assert!(displayed >= 4, "reveal strips: at least four ({names:?}, {bundle_names:?})");
        let leds = (0..8).filter(|i| names.contains(&format!("LED{i}").as_str())).count();
        assert!(leds == 0 || leds == 8, "reveal strips: an array's members are all-or-none ({names:?})");
        let frame = result.frame.map(f64::from);
        assert!(bundles.iter().all(|(_, bundle)| frame[0] <= bundle.bx[0] && frame[1] <= bundle.bx[1]
            && frame[2] >= bundle.bx[2] && frame[3] >= bundle.bx[3]),
            "reveal bundle: inside the card frame");
        assert!(strips.iter().all(|label| {
            frame[0] <= label.bx[0] && frame[1] <= label.bx[1]
                && frame[2] >= label.bx[2] && frame[3] >= label.bx[3]
        }), "reveal strips: inside the card frame");
        for label in &result.sheet.labels {
            let Some([a, b]) = label.leader else { continue };
            for (n, p) in result.sheet.parts.iter().enumerate().filter(|(_, p)| p.placed) {
                if label.owner != sheet::Owner::Part(n as u32) {
                    assert!(!leader_enters(a, b, p.body), "{:?}'s leader stays out of body {n}", label.owner);
                }
            }
            for other in result.sheet.labels.iter().filter(|o| o.owner != label.owner) {
                assert!(!leader_enters(a, b, other.bx), "{:?}'s leader stays out of {:?}'s label", label.owner, other.owner);
                if let Some([c, d]) = other.leader {
                    assert!(!leader_crosses(a, b, c, d), "{:?}'s leader crosses no other leader ({:?})", label.owner, other.owner);
                }
            }
        }
        let satellite = |n: u32| result.rels.iter().any(|(m, rel)| *m == n && matches!(rel, Rel::Satellite));
        let decouplers: Vec<_> = result.sheet.labels.iter().filter(|label| matches!(label.owner,
            sheet::Owner::Part(n) if satellite(n) && g.nodes[n as usize].role.is_small() && result.sheet.parts[n as usize].turn & 1 == 1)).collect();
        assert!(!decouplers.is_empty(), "the mcu reveal has upright small satellites (its decoupling caps)");
        for label in decouplers {
            assert!(label.leader.is_none(), "{:?}: an upright satellite's label sits beside it, with no leader", label.owner);
        }
        assert_eq!(result, reveal_sheet(&d, 0, &g, &sheet, focus), "reveal sheet is deterministic");
    }

    /// Whether a leader's open segment passes through a box's interior.
    fn leader_enters<T: Into<f64> + Copy>(a: [f64; 2], b: [f64; 2], bx: [T; 4]) -> bool {
        let bx = bx.map(Into::into);
        (1..64).any(|i| {
            let t = f64::from(i) / 64.0;
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            p[0] > bx[0] && p[0] < bx[2] && p[1] > bx[1] && p[1] < bx[3]
        })
    }

    /// Whether two leaders properly cross.
    fn leader_crosses(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
        let cross = |o: [f64; 2], p: [f64; 2], q: [f64; 2]| (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0]);
        let opposite = |x: f64, y: f64| (x > 0.0 && y < 0.0) || (x < 0.0 && y > 0.0);
        opposite(cross(c, d, a), cross(c, d, b)) && opposite(cross(a, b, c), cross(a, b, d))
    }

    /// The graph with each part's geometry in the fit the reveal drew it in.
    fn drawn(g: &LayoutGraph, r: &RevealLayout) -> LayoutGraph {
        let mut g = g.clone();
        for &(n, _, p) in &r.parts {
            if g.nodes[n as usize].quarter != p.quarter {
                assert!(g.nodes[n as usize].flip_fit(), "a part drawn in another fit has one: {n}");
            }
        }
        g
    }

    fn check_junctions(g: &LayoutGraph, r: &RevealLayout) {
        let nets = g.port_nets();
        let mut pins = Vec::new();
        for &(n, _, pose) in &r.parts {
            for (q, port) in g.nodes[n as usize].ports.iter().enumerate() {
                if let Some(net) = nets[n as usize][q] {
                    pins.push((net, add(pose.at, rot(port.at, pose.turn, pose.mirror))));
                }
            }
        }
        let mut candidates = pins.clone();
        // Each net's wiring as a set of unit grid edges, so runs that overlap count once.
        let mut edges: Vec<(u32, Pt, Pt)> = Vec::new();
        for run in &r.wires {
            for &p in &run.pts {
                candidates.push((run.net, p));
            }
            for w in run.pts.windows(2) {
                let step = [(w[1][0] - w[0][0]).signum(), (w[1][1] - w[0][1]).signum()];
                assert!(step[0] == 0 || step[1] == 0, "junction: net {} runs orthogonally at {:?}", run.net, w);
                let mut at = w[0];
                while at != w[1] {
                    let next = add(at, step);
                    edges.push((run.net, at.min(next), at.max(next)));
                    at = next;
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        edges.sort_unstable();
        edges.dedup();
        let is_junction = |net: u32, p: Pt| {
            let wire_ways = edges.iter().filter(|&&(n, a, b)| n == net && (a == p || b == p)).count();
            let pin_count = pins.iter().filter(|&&(n, at)| n == net && at == p).count();
            wire_ways >= 2 && wire_ways + pin_count >= 3
        };
        for &(net, p) in &candidates {
            if is_junction(net, p) {
                assert!(r.junctions.contains(&(net, p)),
                    "junction: a meeting of 3+ connections of net {net} at {p:?} has a dot");
            }
        }
        for (i, &(net, p)) in r.junctions.iter().enumerate() {
            assert!(candidates.binary_search(&(net, p)).is_ok() && is_junction(net, p),
                "junction: the dot of net {net} at {p:?} marks 3+ connections");
            assert!(i == 0 || r.junctions[i - 1] < (net, p),
                "junction: dots are unique and sorted at net {net} at {p:?}");
        }
    }

    fn check_continuations(g: &LayoutGraph, r: &RevealLayout) {
        let nets = g.port_nets();
        let mut placed = vec![false; g.nodes.len()];
        for &(n, _, _) in &r.parts {
            placed[n as usize] = true;
        }
        let outside: Vec<bool> = g.nets.iter()
            .map(|net| net.pins.iter().any(|&(n, _)| !placed[n as usize]))
            .collect();
        for &(n, _, pose) in &r.parts {
            for (q, port) in g.nodes[n as usize].ports.iter().enumerate() {
                let Some(net) = nets[n as usize][q] else { continue };
                if port.nc || g.nets[net as usize].kind != NetKind::Signal || !outside[net as usize] {
                    continue;
                }
                let at = add(pose.at, rot(port.at, pose.turn, pose.mirror));
                assert!(r.wires.iter().any(|run| run.pts.first() == Some(&at) || run.pts.last() == Some(&at)
                    || run.pts.windows(2).any(|w| on_seg(at, w[0], w[1])))
                    || r.tags.iter().any(|tag| tag.at == at),
                    "continuation: pin {q} of part {n} on net {net} leaves the reveal and ends in a wire or a tag");
            }
        }
    }

    fn check_bodies(g: &LayoutGraph, r: &RevealLayout) {
        check_junctions(g, r);
        check_continuations(g, r);
        for (n, kind, p) in &r.parts {
            if g.nodes[*n as usize].refit.is_some() && !matches!(kind, Rel::Focus | Rel::Satellite) {
                assert_eq!(p.quarter, p.turn & 1 == 1, "a box symbol the reveal turns is drawn in the fit for its turn: {n}");
            }
        }
        for (i, (n, _, p)) in r.parts.iter().enumerate() {
            assert!(p.placed && p.block == g.nodes[*n as usize].block, "placed part retains its block: {n}");
            assert_eq!(p.body, bx_add(bx_rot(g.nodes[*n as usize].body, p.turn, p.mirror), p.at), "posed body: {n}");
            for (other, _, q) in &r.parts[i + 1..] {
                assert_ne!(n, other, "each related node is placed once");
                assert!(!near(p.body, q.body, 0), "bodies do not overlap: {n} and {other}");
            }
            assert!(contains(r.frame, p.body), "frame contains body: {n}");
        }
        let mut content = r.parts.iter().map(|(_, _, p)| p.body).fold(r.parts[0].2.body, union);
        for run in &r.wires {
            for &p in &run.pts {
                content = union(content, [p[0], p[1], p[0], p[1]]);
            }
        }
        for tag in &r.tags {
            content = union(content, sheet::tag_box(tag));
        }
        assert_eq!(r.frame, [content[0] - 3, content[1] - 3, content[2] + 3, content[3] + 3], "card fits its content: no empty room");
        // Supply names keep off every wire; net flags keep off other nets' wires.
        for tag in r.tags.iter().filter(|t| matches!(t.kind, sheet::TagKind::Power | sheet::TagKind::Label) && !t.text.is_empty()) {
            let bx = sheet::tag_item_box(tag);
            for run in r.wires.iter().filter(|run| tag.kind == sheet::TagKind::Power || tag.net != Some(run.net)) {
                for w in run.pts.windows(2).filter(|w| w[0] != tag.at && w[1] != tag.at) {
                    assert!(sheet::segment_box_distance(w[0], w[1], bx) >= 0.05,
                        "tag name: the {} tag at {:?} keeps off the wire of net {}", tag.text, tag.at, run.net);
                }
            }
        }
        // Runs keep off bodies and bodies keep off runs: no run enters a body, and a run
        // touches only pins of its own net.
        let nets = g.port_nets();
        for run in &r.wires {
            for w in run.pts.windows(2) {
                for (n, _, p) in &r.parts {
                    assert!(!enters(w[0], w[1], p.body), "a run of net {} ({:?} to {:?}) stays out of body {n} {:?}", run.net, w[0], w[1], p.body);
                    for (q, port) in g.nodes[*n as usize].ports.iter().enumerate() {
                        let at = add(p.at, rot(port.at, p.turn, p.mirror));
                        assert!(!on_seg(at, w[0], w[1]) || nets[*n as usize][q] == Some(run.net),
                            "a run of net {} ({:?} to {:?}) touches no foreign pin ({n} pin {q})", run.net, w[0], w[1]);
                    }
                }
            }
        }
    }

    fn contains(a: Bx, b: Bx) -> bool {
        a[0] <= b[0] && a[1] <= b[1] && a[2] >= b[2] && a[3] >= b[3]
    }

    #[test]
    fn reveal_of_the_mcu_holds_its_relations() {
        let d = fab_elec_fixtures::ledctl();
        let c = &d.units[0].circuit;
        let hints = SheetHints::default();
        let g = crate::adapt::graph_of(c, &hints);
        let sheet = crate::layout(&d, 0, &hints, &crate::Opts::default());
        let focus = c.comp("mcu.u").expect("fixture MCU").id.0;
        let rel = relations(&d, 0, &g, focus);
        let r = reveal(&g, &sheet, &rel);
        let g = drawn(&g, &r);
        let reg = c.comps.iter().find(|p| p.part.kind == fab_elec_parts::PartKind::Regulator).expect("fixture regulator").id.0;
        assert!(r.parts.iter().any(|&(n, _, p)| n == reg && p.quarter && p.turn & 1 == 1),
            "the power chain turns its regulator, which takes its quarter fit");
        let v33 = c.net("3V3").expect("fixture 3V3").id.0;
        assert!(!rel.rails.is_empty() && rel.rails.iter().any(|r| r.rail == v33), "MCU takes the 3V3 rail");
        let dec: Vec<_> = c.comps.iter().filter(|p| p.path.starts_with("mcu.dec[")).collect();
        assert!(!dec.is_empty(), "fixture has decoupling parts");
        for p in dec {
            assert!(rel.satellites.contains(&p.id.0), "decoupler is a satellite: {}", p.path);
        }
        for &n in &rel.satellites {
            let pose = r.parts.iter().find(|(p, _, _)| *p == n).expect("satellite is placed").2;
            let original = sheet.parts[n as usize];
            let origin = sheet.parts[focus as usize].at;
            assert_eq!(pose.at, [original.at[0] - origin[0], original.at[1] - origin[1]], "satellite sheet offset: {n}");
            assert_eq!((pose.turn, pose.mirror), (original.turn, original.mirror), "satellite sheet orientation: {n}");
        }
        check_bodies(&g, &r);
        let nrst = g.nets.iter().position(|net| net.name.ends_with("NRST")).expect("fixture NRST net") as u32;
        assert!(r.junctions.iter().any(|&(net, _)| net == nrst), "junction: the NRST branch has its dot");
        let cc1 = g.nets.iter().position(|net| net.name.ends_with("CC1")).expect("fixture CC1 net") as u32;
        assert!(r.tags.iter().any(|tag| tag.net == Some(cc1) && tag.kind == sheet::TagKind::Label),
            "continuation: the connector's CC1 shows where its net goes");
        assert!(!r.wires.is_empty(), "supply relation has wires");
        let nets = g.port_nets();
        let boot = c.comp("mcu.boot_r").expect("fixture BOOT0 pull-down").id.0;
        let pose = r.parts.iter().find(|(n, _, _)| *n == boot).expect("pull-down is placed").2;
        let pin = add(pose.at, rot(g.nodes[boot as usize].ports[0].at, pose.turn, pose.mirror));
        assert!(r.wires.iter().any(|w| Some(w.net) == nets[boot as usize][0]
            && (w.pts.first() == Some(&pin) || w.pts.last() == Some(&pin))), "the BOOT0 pull-down keeps its sheet wire to the MCU");
        for run in &r.wires {
            let on_pin = |at: Pt| r.parts.iter().any(|(n, _, pose)| {
                g.nodes[*n as usize].ports.iter().enumerate().any(|(p, port)| {
                    nets[*n as usize][p] == Some(run.net)
                        && add(pose.at, rot(port.at, pose.turn, pose.mirror)) == at
                })
            });
            assert!(on_pin(run.pts[0]), "wire starts on a port of net {}", run.net);
            let end = *run.pts.last().expect("wire has an endpoint");
            // Rail risers end at tags; every inter-part segment ends on its own net's pin.
            assert!(on_pin(end) || r.tags.iter().any(|t| t.net == Some(run.net) && t.at == end), "wire ends on its net's port or rail tag: {}", run.net);
            assert!(run.pts.windows(2).all(|p| p[0] != p[1] && (p[0][0] == p[1][0] || p[0][1] == p[1][1])), "wire is orthogonal without duplicate points: {}", run.net);
        }
        assert_eq!(rel, relations(&d, 0, &g, focus), "relations are deterministic");
        assert_eq!(r, reveal(&g, &sheet, &rel), "reveal is deterministic");
    }

    #[test]
    fn reveal_of_a_lamp_reaches_its_driver() {
        let d = fab_elec_fixtures::ledctl();
        let c = &d.units[0].circuit;
        let hints = SheetHints::default();
        let g = crate::adapt::graph_of(c, &hints);
        let sheet = crate::layout(&d, 0, &hints, &crate::Opts::default());
        let focus = c.comp("lamps[3].d").expect("fixture lamp").id.0;
        let mcu = c.comp("mcu.u").expect("fixture MCU").id.0;
        let rel = relations(&d, 0, &g, focus);
        let r = reveal(&g, &sheet, &rel);
        let g = drawn(&g, &r);
        assert!(!rel.drivers.is_empty() || !rel.controllers.is_empty(), "lamp has a driver or controller");
        assert!(rel.controllers.iter().any(|&(_, n, _)| n == mcu)
            || rel.drivers.iter().any(|l| l.node == mcu || l.hops.contains(&mcu)), "lamp's relations reach the MCU");
        assert!(r.parts.iter().any(|(n, kind, _)| *n == mcu && matches!(kind, Rel::Controller | Rel::Driver)), "MCU is placed as the lamp's controller or driver");
        check_bodies(&g, &r);
        let mcu_pose = r.parts.iter().find(|(n, _, _)| *n == mcu).expect("MCU is placed").2;
        let nets = g.port_nets();
        assert!(r.wires.iter().any(|run| {
            g.nodes[mcu as usize].ports.iter().enumerate().any(|(p, port)| {
                nets[mcu as usize][p] == Some(run.net)
                    && run.pts.last() == Some(&add(mcu_pose.at, rot(port.at, mcu_pose.turn, mcu_pose.mirror)))
            })
        }), "lamp's driver wire reaches an MCU port on the correct net");
        let pose = r.parts.iter().find(|(n, _, _)| *n == focus).expect("focus is placed").2;
        assert_eq!(pose.at, [0, 0], "focus is at the reveal origin");
        assert_eq!((pose.turn, pose.mirror), (sheet.parts[focus as usize].turn, sheet.parts[focus as usize].mirror), "focus retains its sheet orientation");
    }

}

