//! Route problems (E6 1.3, C4): what an engine is asked to solve, built from a committed board,
//! its rules, its connectivity and the routing world.
//!
//! - Terminals come from connectivity clusters, so a partly routed net is continued, never
//!   restarted: each cluster of a net is one terminal. A free end of a cluster's fixed track is
//!   a route somebody started: the router continues it from its tip (`Terminal::ends`).
//! - Widths and via sizes come from the rule matrix for the net's class.
//! - Hold (E4 further change 6): in an autoroute problem `Authored`, undetached `Block` and
//!   `Derived` copper is fixed. The board's `Solved` copper may be replaced only in a re-route
//!   (`RouteProblem::reroute`): the scope's nets' and any other net's in their way, joined again
//!   before a rip is kept. Otherwise the job keeps all of the board's copper and routes the open
//!   connections from it.
//!
//! Cost: O(clusters + nets) after the connectivity the host already has.

use crate::gesture::Style;
use crate::grid::search::Pref;
use crate::progress::Progress;
use crate::request::{Base, Effort, RouteScope};
use crate::solution::RouteSolution;
use crate::world::{Fixity, RBase, RItem};
use fab_elec_board::{dangling_ends, Board, BoardOp, ConnItem, Connectivity, ItemKind, RuleMatrix, ZoneFill};
use fab_elec_circuit::{Circuit, ViaSize};
use fab_elec_core::{BBox, ClassId, CompId, ItemId, LayerId, LayerSet, NetId, Nm, P};
use std::sync::Arc;

/// The copper layers and each one's preferred direction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RStack {
    pub layers: Vec<LayerId>,
    pub prefs: Vec<Pref>,
}

impl RStack {
    /// The board's copper layers in stack order, alternating horizontal and vertical from the
    /// front (a single layer prefers nothing).
    pub fn of(board: &Board) -> RStack {
        let layers = board.stackup.copper();
        let prefs = if layers.len() < 2 {
            vec![Pref::None; layers.len()]
        } else {
            (0..layers.len()).map(|i| if i % 2 == 0 { Pref::Horizontal } else { Pref::Vertical }).collect()
        };
        RStack { layers, prefs }
    }
}

/// One terminal of a net: one connectivity cluster.
#[derive(Clone, PartialEq, Debug)]
pub struct Terminal {
    /// Index into `Connectivity::clusters`.
    pub cluster: u32,
    /// Its copper pieces (pads, tracks, vias, holes, zone islands), sorted.
    pub items: Vec<ConnItem>,
    /// The box of its copper.
    pub bbox: BBox,
    /// The free ends of its fixed tracks (`dangling_ends`), in (track, end) order.
    pub ends: Vec<End>,
}

/// A free end of a fixed track: where a route somebody started is continued.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct End {
    pub track: ItemId,
    /// 0: the path's start; 1: its end.
    pub end: u8,
    pub at: P,
    pub layer: LayerId,
}

/// The free ends of the fixed tracks among `items` (a cluster's sorted copper). O(its tracks).
fn free_ends(board: &Board, items: &[ConnItem]) -> Vec<End> {
    let mut out = Vec::new();
    let mut last = None;
    for ci in items {
        let ConnItem::Track { item, .. } = *ci else { continue };
        if last.replace(item) == Some(item) {
            continue;
        }
        let Some((ItemKind::Track, i)) = board.locate(item) else { continue };
        let t = &board.tracks[i];
        if !Fixity::holds(&t.origin, t.locked) {
            continue;
        }
        for (e, dangling) in dangling_ends(board, t).into_iter().enumerate() {
            if dangling {
                let at = if e == 0 { t.path.start } else { t.path.end() };
                out.push(End { track: item, end: e as u8, at, layer: t.layer });
            }
        }
    }
    out
}

/// A net to route.
#[derive(Clone, PartialEq, Debug)]
pub struct RNet {
    pub net: NetId,
    pub class: ClassId,
    pub terminals: Vec<Terminal>,
    /// Track width per copper layer (stack order).
    pub w: Vec<Nm>,
    pub via: ViaSize,
    pub layers: LayerSet,
    pub priority: i32,
    /// A zone of the net covers some of its terminals: routed last, by stub and via (E6 5.5).
    pub zone_served: bool,
}

impl RNet {
    /// Half the perimeter of the box of its terminals (the plan's size key).
    pub fn half_perimeter(&self) -> i64 {
        let b = self.terminals.iter().fold(BBox::EMPTY, |b, t| b.union(t.bbox));
        if b.is_empty() {
            0
        } else {
            b.width() + b.height()
        }
    }
}

/// What an engine is asked to solve.
#[derive(Clone, Debug)]
pub struct RouteProblem {
    pub board: Arc<Board>,
    pub base: Arc<RBase>,
    pub rules: Arc<RuleMatrix>,
    pub conn: Arc<Connectivity>,
    pub fills: Arc<Vec<ZoneFill>>,
    pub stack: RStack,
    /// In plan order (E6 5: constrained and short first, zone-served last, then `NetId`).
    pub nets: Vec<RNet>,
    pub style: Style,
    pub effort: Effort,
    /// Whether the board's solved, unlocked copper may be ripped and laid again (the person asked
    /// for a re-route): of the nets in scope, and of any other net in their way, which the rip-up
    /// joins again or rolls back. Off, a net's solved tree is kept and a pin added to the net
    /// joins it as it is.
    pub reroute: bool,
}

impl RouteProblem {
    /// May an engine replace this item? Any copper the job added itself (its ids start at the
    /// board's `next_item`): a pour repair of a net outside the problem is the job's own work
    /// too. In a re-route, also the board's solved, unlocked copper of any net (E4 change 6).
    pub fn owned(&self, it: &RItem) -> bool {
        it.fixity == Fixity::Solved && !it.locked
            && (it.item.is_some_and(|id| id.0 >= self.board.next_item)
                || self.reroute)
    }
}

/// The nets `scope` names that still have more than one cluster, sorted.
pub fn scope_nets(board: &Board, conn: &Connectivity, scope: &RouteScope) -> Vec<NetId> {
    let mut nets: Vec<NetId> = conn
        .unrouted
        .iter()
        .copied()
        .filter(|n| match scope {
            RouteScope::All => true,
            RouteScope::Nets(v) => v.contains(n),
            RouteScope::Class(c) => board.nets.get(n.ix()).is_some_and(|x| x.class == *c),
            RouteScope::Group(_) => false,
            RouteScope::Region(r) => conn.clusters.iter()
                .filter(|c| c.nets.contains(n))
                .any(|c| cluster_bbox(board, &c.items).intersects(r)),
            // by connection, not by net: the worker refuses the scope until its problem form lands
            RouteScope::Parts(_) => false,
        })
        .collect();
    nets.sort();
    nets.dedup();
    nets
}

fn cluster_bbox(board: &Board, items: &[ConnItem]) -> BBox {
    let mut b = BBox::EMPTY;
    let mut ids: Vec<_> = items.iter().map(|c| c.item()).collect();
    ids.dedup();
    for id in ids {
        if let Some(it) = board.item(id) {
            b = b.union(it.bbox(board));
        }
    }
    b
}

/// The problem of routing `nets` on the committed state `base` with the world `world`, keeping
/// the board's copper (set `reroute` for a re-route).
pub fn problem(base: &Base, world: Arc<RBase>, nets: &[NetId], style: Style, effort: Effort) -> RouteProblem {
    let board = &base.board;
    let rules = &base.rules;
    let stack = RStack::of(board);
    let mut out: Vec<RNet> = nets.iter().filter_map(|&net| rnet(board, rules, &base.conn, &stack, net)).collect();
    out.sort_by_key(|n| (n.zone_served, -n.priority, n.half_perimeter(), n.net));
    RouteProblem {
        board: board.clone(),
        base: world,
        rules: rules.clone(),
        conn: base.conn.clone(),
        fills: base.fills.clone(),
        stack,
        nets: out,
        style,
        effort,
        reroute: false,
    }
}

/// The track width nets of `class` route with: the class's width, never below its minimum.
pub(crate) fn class_track_width(rules: &RuleMatrix, class: ClassId) -> Nm {
    rules.class(class).track.unwrap_or(0).max(rules.track_min(class))
}

/// One net from the board and connectivity in use, including any copper added by routing.
/// Returns `None` when fewer than two terminals need joining.
pub fn rnet(board: &Board, rules: &RuleMatrix, conn: &Connectivity, stack: &RStack, net: NetId) -> Option<RNet> {
    let class = board.nets.get(net.ix()).map(|n| n.class).unwrap_or_default();
    let terminals: Vec<Terminal> = conn
        .clusters
        .iter()
        .enumerate()
        .filter(|(_, c)| c.nets.contains(&net))
        .map(|(i, c)| Terminal { cluster: i as u32, items: c.items.clone(), bbox: cluster_bbox(board, &c.items), ends: free_ends(board, &c.items) })
        .collect();
    if terminals.len() < 2 {
        return None;
    }
    let w = class_track_width(rules, class);
    Some(RNet {
        net,
        class,
        terminals,
        w: vec![w; stack.layers.len()],
        via: rules.class(class).via.unwrap_or_else(|| rules.via(class)),
        layers: board.copper_set(),
        priority: 0,
        zone_served: board.zones.iter().any(|z| z.net == Some(net)),
    })
}

/// A search engine: anytime, always legal (E6 0.6). `progress.offer` at every transaction.
pub trait Autorouter {
    fn name(&self) -> &str;
    fn route(&mut self, p: &RouteProblem, progress: &Progress) -> RouteSolution;
}

/// Makes any placement legal with minimal movement (E1 4.8).
pub trait Legaliser {
    fn legalise(&mut self, board: &Board, circuit: &Circuit, scope: &[CompId]) -> Vec<BoardOp>;
}

/// The point of a terminal piece a route attaches to when nothing nearer is wanted: a pad's or
/// via's centre (tracks and zones have none of their own).
pub fn anchor_point(board: &Board, c: ConnItem) -> Option<P> {
    match c {
        ConnItem::Pad { placed, pad } => board.placed_item(placed)?.pad_at(pad),
        ConnItem::Via(v) => board.vias.iter().find(|x| x.id == v).map(|x| x.at),
        ConnItem::Hole(h) => board.holes.iter().find(|x| x.id == h).map(|x| x.at),
        ConnItem::Track { .. } | ConnItem::Zone { .. } => None,
    }
}
