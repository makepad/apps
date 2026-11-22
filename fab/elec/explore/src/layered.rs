//! Deterministic, port-aware layered placement for circuit anchors.
//!
//! The pipeline uses weighted Eades-Lin-Smyth cycle breaking, longest-path
//! layering and up to eight span-compaction passes, dummy vertices for long
//! edges, twelve alternating port-barycentre sweeps followed by adjacent
//! exchanges that lower the port-level crossing count of the drawn edges
//! (`LEdge::drawn`), and eight alternating priority/median coordinate passes.
//! Original edge directions and port attachments are retained in the result;
//! feedback edges only change the direction used internally. No hash-table
//! iteration or random choices occur.
//!
//! For V original nodes, P ports, E original edges, N nodes after dummy
//! insertion, M resulting segments, and L columns, storage is
//! O(V + P + E + N + M + L). Input validation costs O(V + P + E).
//! Cycle breaking costs O(V^2 + E log(E + 1)); layering costs
//! O((V + E) log(V + 1)) plus eight linear compaction passes. Each crossing
//! sweep costs O(N log(N + 1) + M + L + sum(channel_edges^2)); each exchange
//! pass costs the sum of neighbour degree products, and every pass but the last
//! lowers the count, so at most the swept count of passes run; each coordinate
//! pass costs O(N log(N + 1) + M + L + sum(layer_nodes^2)). Dummy expansion
//! therefore makes cost depend on the actual layer spans, including pinned gaps.
//!
//! Sizes, port offsets, and spacing must be nonnegative, endpoints must exist,
//! and the resulting coordinates must fit i32. Invalid input panics. Pins take
//! precedence over flow constraints: incompatible pins can leave an edge within
//! one layer or pointing backward. Self-loops are never reversed and do not
//! influence placement. Unpinned isolated nodes occupy layer zero.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BTreeSet};

/// The side of a node's box a port sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    /// The left edge of the box.
    Left,
    /// The right edge of the box.
    Right,
    /// The top edge of the box.
    Top,
    /// The bottom edge of the box.
    Bottom,
}

/// A port: its side and its offset along that side in grid units, measured from the box's top edge for
/// Left/Right ports and from its left edge for Top/Bottom ports (0..=size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LPort {
    /// The box edge containing this port.
    pub side: Side,
    /// Distance from the top or left end of that edge, in grid units.
    pub offset: i32,
}

/// A node: a box of `size` (w, h) grid units with ports.
#[derive(Clone, Debug, PartialEq)]
pub struct LNode {
    /// Box width and height in grid units.
    pub size: [i32; 2],
    /// Ports indexed by edge endpoints.
    pub ports: Vec<LPort>,
    /// Source order: the tie-break everywhere, smaller first.
    pub order: u32,
    /// A layer this node must take (a connector pinned to the left edge is `Some(0)`), else free.
    pub layer: Option<u32>,
    /// Wanted top-left position in grid units, x right and y down; else free.
    pub at: Option<[i32; 2]>,
}

/// A directed relation driver -> load between two ports: (node index, port index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LEdge {
    /// Driver node and port indices.
    pub from: (u32, u32),
    /// Load node and port indices.
    pub to: (u32, u32),
    /// Importance used for cycle breaking and layer span minimisation.
    pub weight: u32,
    /// Drawn as a wire. An edge that is not (supply flow between blocks, say) still takes part in
    /// layering and makes room, but never counts as a crossing, and pulls a node in the sweeps only
    /// when the node has no drawn segment toward the layer it is ordered against.
    pub drawn: bool,
}

/// The boxes and directed port relations to lay out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LGraph {
    /// Nodes in index order.
    pub nodes: Vec<LNode>,
    /// Edges in index order; parallel edges remain distinct relations.
    pub edges: Vec<LEdge>,
    /// The pinned node a drag moves: pinned columns it runs into make room for it (those left
    /// of it move left, those right of it move right). Without one, later columns move right.
    pub lead: Option<u32>,
    /// Ends of a net that all face one way (all on right ports or all on left ports), on two or
    /// more nodes: no edge joins them, so nothing else pulls them together. Each only orders the
    /// layers: a turn in the layer beyond its ends, joined to each end through the layers between,
    /// takes part in crossing reduction and is gone before coordinates. Without that layer (ends
    /// in the last layer facing right, or the first facing left) it is left out.
    pub turns: Vec<Vec<(u32, u32)>>,
}

/// Spacing in grid units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LOpts {
    /// The smallest horizontal gap between two layers' boxes.
    pub layer_gap: i32,
    /// Extra horizontal gap per distinct edge crossing a channel (room for its vertical wire segment).
    pub per_edge: i32,
    /// The smallest vertical gap between two boxes in one layer.
    pub node_gap: i32,
}

impl Default for LOpts {
    fn default() -> Self {
        Self {
            layer_gap: 6,
            per_edge: 1,
            node_gap: 4,
        }
    }
}

/// The result. Coordinates: grid units, x right, y DOWN, the top-left corner of each node's box; the
/// whole drawing's top-left at (0, 0) unless positions are pinned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placed {
    /// Layer index of each original node.
    pub layer: Vec<u32>,
    /// Original nodes in each layer, top to bottom; empty pinned gaps are retained.
    pub layers: Vec<Vec<u32>>,
    /// Top-left position of each original node.
    pub pos: Vec<[i32; 2]>,
    /// Per edge: reversed to break a cycle (a feedback edge).
    pub reversed: Vec<bool>,
    /// Per edge: the points (x, y) where a long edge (spanning more than one layer) passes through each
    /// intermediate layer, in order from source to target (the dummy nodes' positions); empty for short edges.
    pub through: Vec<Vec<[i32; 2]>>,
    /// Per layer: the x of its left edge and its width (the widest box).
    pub columns: Vec<[i32; 2]>,
    /// Crossings of drawn edges counted at port granularity between adjacent layers (dummies
    /// included).
    pub crossings: u32,
    /// Per original node: in a pinned column that gave way (to the lead, or to the column before
    /// it), so it is not where its pins or the free drawing put it.
    pub yielded: Vec<bool>,
}

/// Where port `port` of node `node` is in `placed` (x, y), grid units, y down.
pub fn port_at(g: &LGraph, placed: &Placed, node: u32, port: u32) -> [i32; 2] {
    let node_index = node as usize;
    let node = &g.nodes[node_index];
    let port = node.ports[port as usize];
    let offset = port_offset(node.size, port);
    let pos = placed.pos[node_index];
    [
        grid(i64::from(pos[0]) + i64::from(offset[0])),
        grid(i64::from(pos[1]) + i64::from(offset[1])),
    ]
}

/// Lays the graph out in layers. Pure and deterministic.
pub fn place(g: &LGraph, o: &LOpts) -> Placed {
    validate(g, o);
    if g.nodes.is_empty() {
        return Placed::default();
    }
    let reversed = break_cycles(g);
    let directed = direct_edges(g, &reversed);
    let layer = assign_layers(g, &directed);
    let mut expanded = expand_edges(g, &directed, &layer, &reversed);
    let mut crossings = reduce_crossings(&mut expanded);
    if drop_turns(&mut expanded) {
        crossings = count_crossings(&expanded, &node_ranks(&expanded));
    }
    // Pins are positions in the free drawing's frame: the same graph laid out without them,
    // normalised to the origin. A pinned layout keeps that drawing, so a pin moves its node
    // against the free nodes, which only make room.
    let mut free = None;
    if g.nodes.iter().any(|node| node.at.is_some()) {
        // Compare wanted y with physical positions, not dimensionless crossing ranks.
        let (baseline, _, origin, _) = coordinates(&expanded, o, None, None);
        for (work, node) in expanded.nodes.iter_mut().zip(&g.nodes) {
            work.at = node.at;
        }
        order_pins(&mut expanded, &baseline, o);
        crossings = count_crossings(&expanded, &node_ranks(&expanded));
        free = Some((origin, baseline));
    }
    let lead = g.lead.filter(|&n| g.nodes.get(n as usize).is_some_and(|node| node.at.is_some()))
        .map(|n| expanded.nodes[n as usize].layer);
    let (positions, columns, _, moved) = coordinates(&expanded, o, free.as_ref().map(|(origin, baseline)| (*origin, baseline.as_slice())), lead);
    let yielded = expanded.nodes[..g.nodes.len()].iter().map(|node| moved[node.layer]).collect();
    Placed {
        layer,
        layers: expanded
            .layers
            .iter()
            .map(|nodes| {
                nodes
                    .iter()
                    .copied()
                    .filter(|&node| node < g.nodes.len())
                    .map(|node| node as u32)
                    .collect()
            })
            .collect(),
        pos: positions[..g.nodes.len()].to_vec(),
        reversed,
        through: expanded
            .through
            .iter()
            .map(|nodes| nodes.iter().map(|&node| positions[node]).collect())
            .collect(),
        columns,
        crossings,
        yielded,
    }
}

fn grid(value: i64) -> i32 {
    i32::try_from(value).expect("layout coordinates must fit i32")
}

fn port_offset(size: [i32; 2], port: LPort) -> [i32; 2] {
    match port.side {
        Side::Left => [0, port.offset],
        Side::Right => [size[0], port.offset],
        Side::Top => [port.offset, 0],
        Side::Bottom => [port.offset, size[1]],
    }
}

fn validate(g: &LGraph, o: &LOpts) {
    assert!(
        o.layer_gap >= 0 && o.per_edge >= 0 && o.node_gap >= 0,
        "layout spacing must be nonnegative"
    );
    assert!(g.nodes.len() <= u32::MAX as usize, "node indices must fit u32");
    for node in &g.nodes {
        assert!(node.size[0] >= 0 && node.size[1] >= 0, "box sizes must be nonnegative");
        for port in &node.ports {
            let extent = match port.side {
                Side::Left | Side::Right => node.size[1],
                Side::Top | Side::Bottom => node.size[0],
            };
            assert!(
                (0..=extent).contains(&port.offset),
                "port offsets must lie on their box edge"
            );
        }
    }
    for edge in &g.edges {
        for (node, port) in [edge.from, edge.to] {
            assert!((node as usize) < g.nodes.len(), "edge node indices must exist");
            assert!(
                (port as usize) < g.nodes[node as usize].ports.len(),
                "edge port indices must exist"
            );
        }
    }
    for ends in &g.turns {
        assert!(ends.len() >= 2, "a turn joins two or more ends");
        for &(node, port) in ends {
            assert!((node as usize) < g.nodes.len(), "turn node indices must exist");
            assert!((port as usize) < g.nodes[node as usize].ports.len(), "turn port indices must exist");
        }
        let side = |&(node, port): &(u32, u32)| g.nodes[node as usize].ports[port as usize].side;
        assert!(
            matches!(side(&ends[0]), Side::Left | Side::Right) && ends.iter().all(|end| side(end) == side(&ends[0])),
            "a turn's ends all face left or all face right"
        );
    }
}

#[derive(Clone, Default)]
struct Balance {
    incoming: usize,
    outgoing: usize,
    in_weight: i128,
    out_weight: i128,
    removed: bool,
}

fn remove_cycle_node(
    node: usize,
    balance: &mut [Balance],
    incoming: &[Vec<(usize, i128)>],
    outgoing: &[Vec<(usize, i128)>],
) {
    balance[node].removed = true;
    for &(other, weight) in &incoming[node] {
        if !balance[other].removed {
            balance[other].outgoing -= 1;
            balance[other].out_weight -= weight;
        }
    }
    for &(other, weight) in &outgoing[node] {
        if !balance[other].removed {
            balance[other].incoming -= 1;
            balance[other].in_weight -= weight;
        }
    }
}

fn break_cycles(g: &LGraph) -> Vec<bool> {
    // Aggregate parallel relations, including zero-weight structural edges.
    let mut weights = BTreeMap::<(usize, usize), i128>::new();
    for edge in &g.edges {
        let from = edge.from.0 as usize;
        let to = edge.to.0 as usize;
        if from != to {
            *weights.entry((from, to)).or_default() += i128::from(edge.weight);
        }
    }
    let count = g.nodes.len();
    let mut incoming = vec![Vec::new(); count];
    let mut outgoing = vec![Vec::new(); count];
    let mut balance = vec![Balance::default(); count];
    for ((from, to), weight) in weights {
        incoming[to].push((from, weight));
        outgoing[from].push((to, weight));
        balance[from].outgoing += 1;
        balance[from].out_weight += weight;
        balance[to].incoming += 1;
        balance[to].in_weight += weight;
    }
    let mut ordered: Vec<_> = (0..count).collect();
    ordered.sort_by_key(|&node| (g.nodes[node].order, node));
    let mut front = Vec::with_capacity(count);
    let mut back = Vec::new();
    for &node in &ordered {
        if g.nodes[node].layer == Some(0) {
            front.push(node);
            remove_cycle_node(node, &mut balance, &incoming, &outgoing);
        }
    }
    while front.len() + back.len() < count {
        if let Some(&node) = ordered
            .iter()
            .find(|&&node| !balance[node].removed && balance[node].outgoing == 0)
        {
            back.push(node);
            remove_cycle_node(node, &mut balance, &incoming, &outgoing);
            continue;
        }
        if let Some(&node) = ordered
            .iter()
            .find(|&&node| !balance[node].removed && balance[node].incoming == 0)
        {
            front.push(node);
            remove_cycle_node(node, &mut balance, &incoming, &outgoing);
            continue;
        }
        let node = ordered
            .iter()
            .copied()
            .filter(|&node| !balance[node].removed)
            .min_by_key(|&node| {
                (
                    Reverse(balance[node].out_weight - balance[node].in_weight),
                    g.nodes[node].order,
                    node,
                )
            })
            .expect("an unfinished cycle ordering has a remaining node");
        front.push(node);
        remove_cycle_node(node, &mut balance, &incoming, &outgoing);
    }
    front.extend(back.into_iter().rev());
    let mut rank = vec![0; count];
    for (position, node) in front.into_iter().enumerate() {
        rank[node] = position;
    }
    g.edges
        .iter()
        .map(|edge| rank[edge.from.0 as usize] > rank[edge.to.0 as usize])
        .collect()
}

type End = (usize, usize);

struct DirectedEdge {
    from: End,
    to: End,
    weight: i128,
}

struct Directed {
    edges: Vec<DirectedEdge>,
    incoming: Vec<Vec<usize>>,
    outgoing: Vec<Vec<usize>>,
}

fn direct_edges(g: &LGraph, reversed: &[bool]) -> Directed {
    let mut result = Directed {
        edges: Vec::with_capacity(g.edges.len()),
        incoming: vec![Vec::new(); g.nodes.len()],
        outgoing: vec![Vec::new(); g.nodes.len()],
    };
    for (index, edge) in g.edges.iter().enumerate() {
        let (from, to) = if reversed[index] {
            (edge.to, edge.from)
        } else {
            (edge.from, edge.to)
        };
        let from = (from.0 as usize, from.1 as usize);
        let to = (to.0 as usize, to.1 as usize);
        result.edges.push(DirectedEdge {
            from,
            to,
            weight: i128::from(edge.weight),
        });
        if from.0 != to.0 {
            result.outgoing[from.0].push(index);
            result.incoming[to.0].push(index);
        }
    }
    result
}

fn topological_order(g: &LGraph, directed: &Directed, backward: bool) -> Vec<usize> {
    let (dependencies, followers) = if backward {
        (&directed.outgoing, &directed.incoming)
    } else {
        (&directed.incoming, &directed.outgoing)
    };
    let mut remaining: Vec<_> = dependencies.iter().map(Vec::len).collect();
    let mut ready = BTreeSet::new();
    for (node, &degree) in remaining.iter().enumerate() {
        if degree == 0 {
            ready.insert((g.nodes[node].order, node));
        }
    }
    let mut order = Vec::with_capacity(g.nodes.len());
    while let Some(&(source_order, node)) = ready.iter().next() {
        ready.remove(&(source_order, node));
        order.push(node);
        for &index in &followers[node] {
            let edge = &directed.edges[index];
            let other = if backward { edge.from.0 } else { edge.to.0 };
            remaining[other] -= 1;
            if remaining[other] == 0 {
                ready.insert((g.nodes[other].order, other));
            }
        }
    }
    assert_eq!(order.len(), g.nodes.len(), "cycle breaking must produce a DAG");
    order
}

fn predecessor_floor(node: usize, directed: &Directed, layer: &[u32]) -> u32 {
    directed.incoming[node]
        .iter()
        .map(|&index| {
            layer[directed.edges[index].from.0]
                .checked_add(1)
                .expect("layer indices must fit u32")
        })
        .max()
        .unwrap_or(0)
}

fn assign_layers(g: &LGraph, directed: &Directed) -> Vec<u32> {
    let mut layer = vec![0; g.nodes.len()];
    for node in topological_order(g, directed, false) {
        layer[node] = match g.nodes[node].layer {
            Some(pinned) => pinned,
            None => predecessor_floor(node, directed, &layer),
        };
    }
    let reverse_order = topological_order(g, directed, true);
    for _ in 0..8 {
        let mut changed = false;
        for &node in &reverse_order {
            if g.nodes[node].layer.is_some() {
                continue;
            }
            let lower = predecessor_floor(node, directed, &layer);
            let upper = directed.outgoing[node]
                .iter()
                .map(|&index| i64::from(layer[directed.edges[index].to.0]) - 1)
                .min()
                .unwrap_or(i64::from(lower));
            if upper < i64::from(lower) {
                // Conflicting pins have no feasible interval; preserve the pin.
                continue;
            }
            let incoming: i128 = directed.incoming[node]
                .iter()
                .map(|&index| directed.edges[index].weight)
                .sum();
            let outgoing: i128 = directed.outgoing[node]
                .iter()
                .map(|&index| directed.edges[index].weight)
                .sum();
            // On the feasible interval, span cost is affine with this slope.
            let best = if incoming < outgoing { upper as u32 } else { lower };
            changed |= best != layer[node];
            layer[node] = best;
        }
        if !changed {
            break;
        }
    }
    layer
}

struct WorkNode {
    size: [i32; 2],
    ports: Vec<LPort>,
    order: u32,
    layer: usize,
    dummy: bool,
    at: Option<[i32; 2]>,
}

struct Segment {
    // Ordered geometrically, from the left column to the right column.
    ends: [End; 2],
    drawn: bool,
}

struct Expanded {
    nodes: Vec<WorkNode>,
    segments: Vec<Segment>,
    incident: Vec<Vec<usize>>,
    channels: Vec<Vec<usize>>,
    layers: Vec<Vec<usize>>,
    through: Vec<Vec<usize>>,
    /// The first node and segment of the turns (`LGraph::turns`): everything from there on.
    turns_from: (usize, usize),
}

fn add_segment(expanded: &mut Expanded, from: End, to: End, drawn: bool) {
    let (left, right) = if expanded.nodes[from.0].layer <= expanded.nodes[to.0].layer {
        (from, to)
    } else {
        (to, from)
    };
    let index = expanded.segments.len();
    expanded.segments.push(Segment { ends: [left, right], drawn });
    expanded.incident[left.0].push(index);
    expanded.incident[right.0].push(index);
    let left_layer = expanded.nodes[left.0].layer;
    let right_layer = expanded.nodes[right.0].layer;
    if right_layer > left_layer {
        debug_assert_eq!(right_layer - left_layer, 1, "dummy segments must be adjacent");
        expanded.channels[left_layer].push(index);
    }
}

fn expand_edges(g: &LGraph, directed: &Directed, layer: &[u32], reversed: &[bool]) -> Expanded {
    let count = usize::try_from(*layer.iter().max().unwrap_or(&0))
        .expect("layer indices must fit usize")
        .checked_add(1)
        .expect("column count must fit usize");
    let mut expanded = Expanded {
        nodes: g.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| WorkNode {
                size: node.size,
                ports: node.ports.clone(),
                order: node.order,
                layer: layer[index] as usize,
                dummy: false,
                // Install position pins after the provisional coordinate pass.
                at: None,
            })
            .collect(),
        segments: Vec::new(),
        incident: vec![Vec::new(); g.nodes.len()],
        channels: vec![Vec::new(); count - 1],
        layers: vec![Vec::new(); count],
        through: vec![Vec::new(); g.edges.len()],
        turns_from: (0, 0),
    };
    for (index, edge) in directed.edges.iter().enumerate() {
        if edge.from.0 == edge.to.0 {
            continue;
        }
        let from_layer = layer[edge.from.0] as usize;
        let to_layer = layer[edge.to.0] as usize;
        let forward = from_layer < to_layer;
        let span = from_layer.abs_diff(to_layer);
        let mut previous = edge.from;
        for step in 1..span {
            let dummy_layer = if forward { from_layer + step } else { from_layer - step };
            let dummy = expanded.nodes.len();
            expanded.nodes.push(WorkNode {
                size: [0, 0],
                ports: vec![
                    LPort { side: Side::Left, offset: 0 },
                    LPort { side: Side::Right, offset: 0 },
                ],
                order: g.nodes[edge.from.0].order,
                layer: dummy_layer,
                dummy: true,
                at: None,
            });
            expanded.incident.push(Vec::new());
            expanded.through[index].push(dummy);
            let input_port = if forward { 0 } else { 1 };
            let output_port = 1 - input_port;
            add_segment(&mut expanded, previous, (dummy, input_port), g.edges[index].drawn);
            previous = (dummy, output_port);
        }
        add_segment(&mut expanded, previous, edge.to, g.edges[index].drawn);
        if reversed[index] {
            expanded.through[index].reverse();
        }
    }
    expanded.turns_from = (expanded.nodes.len(), expanded.segments.len());
    for ends in &g.turns {
        let side = g.nodes[ends[0].0 as usize].ports[ends[0].1 as usize].side;
        let east = side == Side::Right;
        let layers = ends.iter().map(|&(node, _)| layer[node as usize] as usize);
        let turn_layer = if east { layers.max().map(|l| l + 1) } else { layers.min().and_then(|l| l.checked_sub(1)) };
        let Some(turn_layer) = turn_layer.filter(|&l| l < count) else { continue };
        let order = g.nodes[ends[0].0 as usize].order;
        let dummy = |expanded: &mut Expanded, layer: usize, ports: Vec<LPort>| {
            expanded.nodes.push(WorkNode { size: [0, 0], ports, order, layer, dummy: true, at: None });
            expanded.incident.push(Vec::new());
            expanded.nodes.len() - 1
        };
        // The turn's one port faces the ends.
        let turn = dummy(&mut expanded, turn_layer, vec![LPort { side: if east { Side::Left } else { Side::Right }, offset: 0 }]);
        for &(node, port) in ends {
            let mut previous = (node as usize, port as usize);
            let from = layer[node as usize] as usize;
            let between: Vec<usize> = if east { (from + 1..turn_layer).collect() } else { (turn_layer + 1..from).rev().collect() };
            for l in between {
                let d = dummy(&mut expanded, l, vec![LPort { side: Side::Left, offset: 0 }, LPort { side: Side::Right, offset: 0 }]);
                add_segment(&mut expanded, previous, (d, if east { 0 } else { 1 }), true);
                previous = (d, if east { 1 } else { 0 });
            }
            add_segment(&mut expanded, previous, (turn, 0), true);
        }
    }
    for (index, node) in expanded.nodes.iter().enumerate() {
        expanded.layers[node.layer].push(index);
    }
    for nodes in &mut expanded.layers {
        nodes.sort_by_key(|&node| (expanded.nodes[node].order, node));
    }
    expanded
}

/// Removes the turns once crossing reduction has used them; true when there were any.
fn drop_turns(expanded: &mut Expanded) -> bool {
    let (nodes, segments) = expanded.turns_from;
    if nodes == expanded.nodes.len() {
        return false;
    }
    expanded.nodes.truncate(nodes);
    expanded.incident.truncate(nodes);
    expanded.segments.truncate(segments);
    for list in &mut expanded.incident {
        list.retain(|&index| index < segments);
    }
    for channel in &mut expanded.channels {
        channel.retain(|&index| index < segments);
    }
    for layer in &mut expanded.layers {
        layer.retain(|&node| node < nodes);
    }
    true
}

fn node_ranks(expanded: &Expanded) -> Vec<usize> {
    let mut ranks = vec![0; expanded.nodes.len()];
    for nodes in &expanded.layers {
        for (rank, &node) in nodes.iter().enumerate() {
            ranks[node] = rank;
        }
    }
    ranks
}

fn connection(segment: &Segment, node: usize) -> (usize, End) {
    if segment.ends[0].0 == node {
        (segment.ends[0].1, segment.ends[1])
    } else {
        (segment.ends[1].1, segment.ends[0])
    }
}

#[derive(Clone, Copy)]
struct PortRank {
    rank: usize,
    offset: i64,
    denominator: i64,
}

impl PortRank {
    fn compare(self, other: Self) -> Ordering {
        // Fractions are in [0, 1); cross products fit i64 for i32 dimensions.
        self.rank.cmp(&other.rank).then_with(|| {
            (self.offset * other.denominator).cmp(&(other.offset * self.denominator))
        })
    }

    fn value(self) -> f64 {
        self.rank as f64 + self.offset as f64 / self.denominator as f64
    }
}

fn port_rank(expanded: &Expanded, ranks: &[usize], end: End) -> PortRank {
    let node = &expanded.nodes[end.0];
    let port = node.ports[end.1];
    let extent = match port.side {
        Side::Left | Side::Right => node.size[1],
        Side::Top | Side::Bottom => node.size[0],
    };
    PortRank {
        rank: ranks[end.0],
        offset: i64::from(port.offset),
        denominator: i64::from(extent) + 1,
    }
}

fn count_crossings(expanded: &Expanded, ranks: &[usize]) -> u32 {
    let mut crossings = 0_u32;
    for channel in &expanded.channels {
        let endpoints: Vec<_> = channel
            .iter()
            .filter(|&&index| expanded.segments[index].drawn)
            .map(|&index| {
                let segment = &expanded.segments[index];
                [
                    port_rank(expanded, ranks, segment.ends[0]),
                    port_rank(expanded, ranks, segment.ends[1]),
                ]
            })
            .collect();
        for (index, ends) in endpoints.iter().enumerate() {
            for other in &endpoints[index + 1..] {
                let left = ends[0].compare(other[0]);
                let right = ends[1].compare(other[1]);
                if matches!(
                    (left, right),
                    (Ordering::Less, Ordering::Greater) | (Ordering::Greater, Ordering::Less)
                ) {
                    crossings = crossings.checked_add(1).expect("crossing count must fit u32");
                }
            }
        }
    }
    crossings
}

fn sweep_layer(expanded: &mut Expanded, layer: usize, fixed: usize, ranks: &mut [usize]) {
    let mut keyed = Vec::with_capacity(expanded.layers[layer].len());
    for &node in &expanded.layers[layer] {
        // Drawn segments pull; undrawn ones only a node with no drawn segment toward `fixed`.
        let mut sums = [(0.0, 0_usize); 2];
        for &index in &expanded.incident[node] {
            let segment = &expanded.segments[index];
            let (_, other) = connection(segment, node);
            if expanded.nodes[other.0].layer == fixed {
                let sum = &mut sums[usize::from(!segment.drawn)];
                sum.0 += port_rank(expanded, ranks, other).value();
                sum.1 += 1;
            }
        }
        let key = match sums.iter().find(|sum| sum.1 > 0) {
            Some(&(total, count)) => total / count as f64,
            None => ranks[node] as f64,
        };
        keyed.push((node, key));
    }
    keyed.sort_by(|&(a, a_key), &(b, b_key)| {
        a_key
            .total_cmp(&b_key)
            .then_with(|| expanded.nodes[a].order.cmp(&expanded.nodes[b].order))
            .then_with(|| a.cmp(&b))
    });
    for (rank, (node, _)) in keyed.into_iter().enumerate() {
        expanded.layers[layer][rank] = node;
        ranks[node] = rank;
    }
}

fn reduce_crossings(expanded: &mut Expanded) -> u32 {
    let mut ranks = node_ranks(expanded);
    let mut best = expanded.layers.clone();
    let mut best_count = count_crossings(expanded, &ranks);
    for pass in 0..12 {
        if pass % 2 == 0 {
            for layer in 1..expanded.layers.len() {
                sweep_layer(expanded, layer, layer - 1, &mut ranks);
            }
        } else {
            for layer in (0..expanded.layers.len().saturating_sub(1)).rev() {
                sweep_layer(expanded, layer, layer + 1, &mut ranks);
            }
        }
        let count = count_crossings(expanded, &ranks);
        if count < best_count {
            best_count = count;
            best.clone_from(&expanded.layers);
        }
    }
    expanded.layers = best;
    let mut ranks = node_ranks(expanded);
    let gain = exchange(expanded, &mut ranks);
    debug_assert_eq!(count_crossings(expanded, &ranks), best_count - gain, "a swap lowers the count by its own gain");
    best_count - gain
}

/// Swaps neighbours in a layer while that lowers the drawn port-level crossing count, until no swap
/// does; returns the fall. A sweep orders a layer by its barycentres on one side only, so it can
/// leave a node on the far side of a bundle from its partners; a swap weighs both channels at port
/// level. Only the pairs of segments of the two swapped nodes change, so each candidate costs the
/// product of their degrees, and every swap lowers the count: at most the sweeps' count of swaps.
fn exchange(expanded: &mut Expanded, ranks: &mut [usize]) -> u32 {
    let mut gain = 0;
    loop {
        let mut swapped = false;
        for layer in 0..expanded.layers.len() {
            for i in 1..expanded.layers[layer].len() {
                let (u, v) = (expanded.layers[layer][i - 1], expanded.layers[layer][i]);
                // Crossings between u's and v's segments with u above (now) and with v above.
                let (mut now, mut then) = (0, 0);
                let drawn = |&&index: &&usize| expanded.segments[index].drawn;
                for &a in expanded.incident[u].iter().filter(drawn) {
                    let (_, far_a) = connection(&expanded.segments[a], u);
                    for &b in expanded.incident[v].iter().filter(drawn) {
                        let (_, far_b) = connection(&expanded.segments[b], v);
                        let side = expanded.nodes[far_a.0].layer;
                        if side == layer || side != expanded.nodes[far_b.0].layer {
                            continue;
                        }
                        match port_rank(expanded, ranks, far_a).compare(port_rank(expanded, ranks, far_b)) {
                            Ordering::Greater => now += 1,
                            Ordering::Less => then += 1,
                            Ordering::Equal => {}
                        }
                    }
                }
                if then < now {
                    expanded.layers[layer].swap(i - 1, i);
                    ranks.swap(u, v);
                    gain += now - then;
                    swapped = true;
                }
            }
        }
        if !swapped {
            return gain;
        }
    }
}

/// Insert pins by wanted y among the free nodes' physical barycentres. Free nodes retain their
/// crossing order; a node that cannot fit between two pins goes below the second one.
fn order_pins(expanded: &mut Expanded, baseline: &[[i32; 2]], o: &LOpts) {
    let gap = i64::from(o.node_gap);
    let mut logged = false;
    for layer in 0..expanded.layers.len() {
        let mut pins: Vec<usize> = expanded.layers[layer].iter().copied()
            .filter(|&node| expanded.nodes[node].at.is_some()).collect();
        if pins.is_empty() {
            continue;
        }
        pins.sort_by_key(|&node| (expanded.nodes[node].order, node));
        let mut accepted: Vec<usize> = Vec::new();
        for node in pins {
            let [x, wanted_y] = expanded.nodes[node].at.expect("a position pin");
            let mut y = i64::from(wanted_y);
            let height = i64::from(expanded.nodes[node].size[1]);
            for &other in &accepted {
                let top = i64::from(expanded.nodes[other].at.expect("an accepted pin")[1]);
                let bottom = top + i64::from(expanded.nodes[other].size[1]);
                if y < bottom + gap && y + height + gap > top {
                    y = bottom + gap;
                }
            }
            if y != i64::from(wanted_y) && !logged {
                eprintln!("fab.explore: overlapping position pins in a layer; later source order moves down");
                logged = true;
            }
            expanded.nodes[node].at = Some([x, grid(y)]);
            accepted.push(node);
            accepted.sort_by_key(|&n| (expanded.nodes[n].at.unwrap()[1], expanded.nodes[n].order, n));
        }
        let mut ordered = Vec::with_capacity(expanded.layers[layer].len());
        let mut next = 0;
        let mut bottom: Option<i64> = None;
        let mut previous_key = f64::NEG_INFINITY;
        for &node in &expanded.layers[layer] {
            let work = &expanded.nodes[node];
            if work.at.is_some() {
                continue;
            }
            // A free node keeps its place in the free drawing, so pins at their drawn places
            // reproduce that drawing, dummies (the runs passing through) included.
            let key = f64::from(baseline[node][1]).max(previous_key);
            previous_key = key;
            let height = i64::from(work.size[1]);
            while let Some(&pin) = accepted.get(next) {
                let y = i64::from(expanded.nodes[pin].at.unwrap()[1]);
                let before_pin = key.total_cmp(&(y as f64))
                    .then_with(|| (work.order, node).cmp(&(expanded.nodes[pin].order, pin)))
                    == Ordering::Less;
                if before_pin && !bottom.is_some_and(|b| b + gap + height + gap > y) {
                    break;
                }
                ordered.push(pin);
                bottom = Some(y + i64::from(expanded.nodes[pin].size[1]));
                next += 1;
            }
            ordered.push(node);
            bottom = bottom.map(|b| b + gap + height);
        }
        ordered.extend_from_slice(&accepted[next..]);
        expanded.layers[layer] = ordered;
    }
}

struct Vertical {
    y: Vec<i64>,
    ranks: Vec<usize>,
    priority: Vec<Vec<usize>>,
    locked: Vec<bool>,
    gap: i64,
}

fn move_toward(expanded: &Expanded, vertical: &mut Vertical, node: usize, target: i64) {
    let nodes = &expanded.layers[expanded.nodes[node].layer];
    let rank = vertical.ranks[node];
    if target < vertical.y[node] {
        let mut destination = target;
        let mut distance = 0;
        for &above in nodes[..rank].iter().rev() {
            distance += i64::from(expanded.nodes[above].size[1]) + vertical.gap;
            if vertical.locked[above] {
                destination = destination.max(vertical.y[above] + distance);
                break;
            }
        }
        vertical.y[node] = destination;
        // Unvisited, lower-priority nodes may be pushed, never passed.
        for position in (0..rank).rev() {
            let above = nodes[position];
            let below = nodes[position + 1];
            let ceiling = vertical.y[below]
                - vertical.gap
                - i64::from(expanded.nodes[above].size[1]);
            if vertical.y[above] <= ceiling {
                break;
            }
            debug_assert!(!vertical.locked[above], "higher-priority nodes must stay fixed");
            vertical.y[above] = ceiling;
        }
    } else if target > vertical.y[node] {
        let mut destination = target;
        let mut distance = i64::from(expanded.nodes[node].size[1]) + vertical.gap;
        for &below in &nodes[rank + 1..] {
            if vertical.locked[below] {
                destination = destination.min(vertical.y[below] - distance);
                break;
            }
            distance += i64::from(expanded.nodes[below].size[1]) + vertical.gap;
        }
        vertical.y[node] = destination;
        for position in rank + 1..nodes.len() {
            let above = nodes[position - 1];
            let below = nodes[position];
            let floor = vertical.y[above]
                + i64::from(expanded.nodes[above].size[1])
                + vertical.gap;
            if vertical.y[below] >= floor {
                break;
            }
            debug_assert!(!vertical.locked[below], "higher-priority nodes must stay fixed");
            vertical.y[below] = floor;
        }
    }
}

/// Pushes the nodes of `node`'s layer clear of it, keeping their order: those below down,
/// those above up, each only as far as its gap needs.
fn push_clear(expanded: &Expanded, vertical: &mut Vertical, node: usize) {
    let nodes = &expanded.layers[expanded.nodes[node].layer];
    let rank = vertical.ranks[node];
    for position in rank + 1..nodes.len() {
        let (above, below) = (nodes[position - 1], nodes[position]);
        let floor = vertical.y[above] + i64::from(expanded.nodes[above].size[1]) + vertical.gap;
        if vertical.y[below] >= floor {
            break;
        }
        vertical.y[below] = floor;
    }
    for position in (0..rank).rev() {
        let (above, below) = (nodes[position], nodes[position + 1]);
        let ceiling = vertical.y[below] - vertical.gap - i64::from(expanded.nodes[above].size[1]);
        if vertical.y[above] <= ceiling {
            break;
        }
        vertical.y[above] = ceiling;
    }
}

fn straighten_layer(expanded: &Expanded, vertical: &mut Vertical, layer: usize, fixed: usize) {
    for position in 0..vertical.priority[layer].len() {
        let node = vertical.priority[layer][position];
        let own_node = &expanded.nodes[node];
        if own_node.at.is_some() {
            continue;
        }
        let mut targets = Vec::new();
        for &index in &expanded.incident[node] {
            let (own_port, other) = connection(&expanded.segments[index], node);
            let other_node = &expanded.nodes[other.0];
            if other_node.layer == fixed {
                let other_offset = port_offset(other_node.size, other_node.ports[other.1])[1];
                let own_offset = port_offset(own_node.size, own_node.ports[own_port])[1];
                targets.push(vertical.y[other.0] + i64::from(other_offset) - i64::from(own_offset));
            }
        }
        if !targets.is_empty() {
            // The lower median is deterministic and minimises absolute offsets.
            let middle = (targets.len() - 1) / 2;
            let (_, target, _) = targets.select_nth_unstable(middle);
            move_toward(expanded, vertical, node, *target);
        }
        vertical.locked[node] = true;
    }
}

fn shift_column(expanded: &Expanded, columns: &mut [[i64; 2]], x: &mut [i64], layer: usize, dx: i64) {
    columns[layer][0] += dx;
    for &node in &expanded.layers[layer] {
        x[node] += dx;
    }
}

/// Positions and columns, normalised by their own top-left; also returns the raw origin used.
/// With pins, `free` is the same graph's free drawing (its raw origin and normalised
/// positions): free nodes keep their places in it, pins land at theirs and push their layer's
/// free nodes clear, and the result keeps that drawing's origin. `lead` is the layer of the
/// pin a drag moves: pinned columns before it give way to the left. Also returns, per layer,
/// whether it is a pinned column that gave way.
fn coordinates(expanded: &Expanded, o: &LOpts, free: Option<([i64; 2], &[[i32; 2]])>, lead: Option<usize>) -> (Vec<[i32; 2]>, Vec<[i32; 2]>, [i64; 2], Vec<bool>) {
    let mut columns = vec![[0_i64; 2]; expanded.layers.len()];
    for (layer, nodes) in expanded.layers.iter().enumerate() {
        columns[layer][1] = nodes
            .iter()
            .map(|&node| i64::from(expanded.nodes[node].size[0]))
            .max()
            .unwrap_or(0);
        if layer > 0 {
            // An original edge contributes exactly one segment to each channel
            // it crosses, regardless of its weight or total span.
            let edge_count = i64::try_from(expanded.channels[layer - 1].len())
                .expect("channel edge count must fit i64");
            columns[layer][0] = columns[layer - 1][0]
                + columns[layer - 1][1]
                + i64::from(o.layer_gap)
                + i64::from(o.per_edge) * edge_count;
        }
    }
    let mut x = vec![0_i64; expanded.nodes.len()];
    let mut vertical = Vertical {
        y: vec![0; expanded.nodes.len()],
        ranks: node_ranks(expanded),
        priority: expanded.layers.clone(),
        locked: vec![false; expanded.nodes.len()],
        gap: i64::from(o.node_gap),
    };
    for (layer, nodes) in expanded.layers.iter().enumerate() {
        let mut y = 0;
        for &node in nodes {
            let work_node = &expanded.nodes[node];
            let all_right = !expanded.incident[node].is_empty()
                && expanded.incident[node].iter().all(|&index| {
                    let (_, other) = connection(&expanded.segments[index], node);
                    expanded.nodes[other.0].layer > layer
                });
            let inset = if work_node.dummy {
                columns[layer][1] / 2
            } else if all_right {
                columns[layer][1] - i64::from(work_node.size[0])
            } else {
                0
            };
            x[node] = columns[layer][0] + inset;
            vertical.y[node] = y;
            y += i64::from(work_node.size[1]) + vertical.gap;
        }
        vertical.priority[layer].sort_by_key(|&node| {
            (
                !expanded.nodes[node].dummy,
                Reverse(expanded.incident[node].len()),
                expanded.nodes[node].order,
                node,
            )
        });
    }
    let pinned = free.filter(|_| expanded.nodes.iter().any(|node| node.at.is_some()));
    let mut moved = vec![false; expanded.layers.len()];
    if let Some((origin, baseline)) = pinned {
        // A pin lands at its position in the free drawing's frame; the free nodes keep theirs
        // and its layer's neighbours are pushed clear in order. Its column becomes the span of
        // its parts where they now are (a column whose parts are all pinned is where its pins
        // are, not where the free drawing had it), the edges passing through run down its
        // middle, and neighbouring columns keep their gaps.
        for (y, at) in vertical.y.iter_mut().zip(baseline) {
            *y = i64::from(at[1]) + origin[1];
        }
        let mut fixed = vec![false; columns.len()];
        for (layer, nodes) in expanded.layers.iter().enumerate() {
            if !nodes.iter().any(|&node| expanded.nodes[node].at.is_some()) {
                continue;
            }
            fixed[layer] = true;
            let (mut left, mut right) = (i64::MAX, i64::MIN);
            for &node in nodes {
                if let Some(at) = expanded.nodes[node].at {
                    x[node] = i64::from(at[0]) + origin[0];
                    vertical.y[node] = i64::from(at[1]) + origin[1];
                    push_clear(expanded, &mut vertical, node);
                }
                if !expanded.nodes[node].dummy {
                    left = left.min(x[node]);
                    right = right.max(x[node] + i64::from(expanded.nodes[node].size[0]));
                }
            }
            for &node in nodes {
                if expanded.nodes[node].dummy {
                    x[node] = left + (right - left) / 2;
                }
            }
            columns[layer] = [left, right - left];
        }
        let gap = |channel: usize| i64::from(o.layer_gap)
            + i64::from(o.per_edge) * expanded.channels[channel].len() as i64;
        let first = fixed.iter().position(|&pin| pin).unwrap_or(0);
        let last = fixed.iter().rposition(|&pin| pin).unwrap_or(0);
        // Free columns before the last pin, and pinned ones before the lead, keep their gap
        // to the next column by moving left.
        for layer in (0..last).rev() {
            if !fixed[layer] || lead.is_some_and(|lead| layer < lead) {
                let dx = (columns[layer + 1][0] - gap(layer)
                    - columns[layer][1] - columns[layer][0]).min(0);
                moved[layer] |= fixed[layer] && dx != 0;
                shift_column(expanded, &mut columns, &mut x, layer, dx);
            }
        }
        // Push is a gesture: without a lead, two pinned columns need only room for their
        // channel's tracks (`channels::track_positions` packs an overflowing channel at unit
        // spacing), so a new run takes the slack between them and moves neither.
        let tracks = |channel: usize| 1 + i64::from(o.per_edge) * expanded.channels[channel].len() as i64;
        // Pinned columns are pushed too: a pin dragged past a pinned column of a later layer
        // (a header dragged right past its pinned MCU) moves that column clear, pins and all,
        // so the columns and their channels always increase.
        for layer in first + 1..columns.len() {
            let room = if lead.is_none() && fixed[layer - 1] && fixed[layer] { tracks(layer - 1) } else { gap(layer - 1) };
            let dx = (columns[layer - 1][0] + columns[layer - 1][1] + room
                - columns[layer][0]).max(0);
            moved[layer] |= fixed[layer] && dx != 0;
            shift_column(expanded, &mut columns, &mut x, layer, dx);
        }
    }
    // A free drawing straightens its runs; a pinned one keeps the free drawing's.
    if pinned.is_none() {
        for pass in 0..8 {
            vertical.locked.fill(false);
            if pass % 2 == 0 {
                for layer in 1..expanded.layers.len() {
                    straighten_layer(expanded, &mut vertical, layer, layer - 1);
                }
            } else {
                for layer in (0..expanded.layers.len().saturating_sub(1)).rev() {
                    straighten_layer(expanded, &mut vertical, layer, layer + 1);
                }
            }
        }
    }
    let [min_x, min_y] = match pinned {
        Some((origin, _)) => origin,
        None => [x.iter().copied().min().unwrap_or(0), vertical.y.iter().copied().min().unwrap_or(0)],
    };
    let positions = expanded.nodes
        .iter()
        .enumerate()
        .map(|(node, work_node)| {
            let x = x[node] - min_x;
            let y = vertical.y[node] - min_y;
            assert!(
                x + i64::from(work_node.size[0]) <= i64::from(i32::MAX)
                    && y + i64::from(work_node.size[1]) <= i64::from(i32::MAX),
                "layout box extents must fit i32"
            );
            [grid(x), grid(y)]
        })
        .collect();
    let columns = columns
        .into_iter()
        .map(|[left, width]| [grid(left - min_x), grid(width)])
        .collect();
    (positions, columns, [min_x, min_y], moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ic(h: i32, left: &[i32], right: &[i32], order: u32) -> LNode {
        LNode {
            size: [6, h],
            ports: left
                .iter()
                .map(|&offset| LPort { side: Side::Left, offset })
                .chain(right.iter().map(|&offset| LPort { side: Side::Right, offset }))
                .collect(),
            order,
            layer: None,
            at: None,
        }
    }

    fn assert_no_overlaps(g: &LGraph, placed: &Placed, o: &LOpts) {
        for nodes in &placed.layers {
            for pair in nodes.windows(2) {
                let above = pair[0] as usize;
                let below = pair[1] as usize;
                assert!(
                    i64::from(placed.pos[above][1])
                        + i64::from(g.nodes[above].size[1])
                        + i64::from(o.node_gap)
                        <= i64::from(placed.pos[below][1]),
                    "boxes in each layer preserve order and the minimum vertical gap"
                );
            }
        }
        for pair in placed.columns.windows(2) {
            assert!(
                i64::from(pair[0][0]) + i64::from(pair[0][1]) + i64::from(o.layer_gap)
                    <= i64::from(pair[1][0]),
                "layer columns do not overlap and preserve the minimum horizontal gap"
            );
        }
    }

    #[test]
    fn a_chain_lays_left_to_right_with_straight_wires() {
        let g = LGraph {
            nodes: vec![ic(4, &[], &[2], 0), ic(4, &[2], &[2], 1), ic(4, &[2], &[], 2)],
            edges: vec![
                LEdge { from: (0, 0), to: (1, 0), weight: 1, drawn: true },
                LEdge { from: (1, 1), to: (2, 0), weight: 1, drawn: true },
            ],
            lead: None,
            turns: Vec::new(),
        };
        let o = LOpts::default();
        let placed = place(&g, &o);
        assert_eq!(placed.layer, vec![0, 1, 2], "a chain takes consecutive flow layers");
        assert!(
            placed.pos[0][0] < placed.pos[1][0] && placed.pos[1][0] < placed.pos[2][0],
            "chain boxes advance strictly left to right"
        );
        for edge in &g.edges {
            assert_eq!(
                port_at(&g, &placed, edge.from.0, edge.from.1)[1],
                port_at(&g, &placed, edge.to.0, edge.to.1)[1],
                "chain edges align their endpoint ports for zero bends"
            );
        }
        assert!(placed.reversed.iter().all(|&value| !value), "acyclic chain edges stay forward");
        assert_eq!(placed.crossings, 0, "a chain has no port crossings");
        assert!(placed.through.iter().all(Vec::is_empty), "short chain edges need no dummy points");
        assert_eq!(placed.pos[0], [0, 0], "the chain drawing starts at the origin");
        assert_eq!(placed, place(&g, &o), "repeated chain placement is deterministic");
        assert_no_overlaps(&g, &placed, &o);
    }

    #[test]
    fn a_cycle_reverses_one_edge_and_long_edges_get_through_points() {
        let mut a = ic(4, &[2], &[2], 0);
        a.layer = Some(0);
        let g = LGraph {
            nodes: vec![a, ic(4, &[2], &[2], 1), ic(4, &[2], &[2], 2)],
            edges: vec![
                LEdge { from: (0, 1), to: (1, 0), weight: 1, drawn: true },
                LEdge { from: (1, 1), to: (2, 0), weight: 1, drawn: true },
                LEdge { from: (2, 1), to: (0, 0), weight: 1, drawn: true },
                LEdge { from: (0, 1), to: (2, 0), weight: 1, drawn: true },
            ],
            lead: None,
            turns: Vec::new(),
        };
        let o = LOpts::default();
        let placed = place(&g, &o);
        assert_eq!(
            placed.reversed.iter().filter(|&&value| value).count(),
            1,
            "cycle breaking selects exactly one feedback edge"
        );
        assert!(placed.reversed[2], "the cycle-closing c-to-a edge is the feedback edge");
        for (index, edge) in g.edges.iter().enumerate() {
            let (from, to) = if placed.reversed[index] {
                (edge.to.0, edge.from.0)
            } else {
                (edge.from.0, edge.to.0)
            };
            assert!(
                placed.layer[from as usize] < placed.layer[to as usize],
                "every internally oriented cycle edge advances to a higher layer"
            );
        }
        assert_eq!(placed.layer[0], 0, "a node pinned to layer zero stays on layer zero");
        assert_eq!(placed.layer[2] - placed.layer[0], 2, "the a-to-c edge spans two layers");
        assert_eq!(placed.through[3].len(), 1, "a two-layer edge has one intermediate dummy point");
        let point = placed.through[3][0];
        let first = placed.columns[placed.layer[0] as usize];
        let last = placed.columns[placed.layer[2] as usize];
        assert!(
            first[0] + first[1] < point[0] && point[0] < last[0],
            "a long edge's dummy lies between its endpoint columns"
        );
        let middle = placed.columns[1];
        assert_eq!(
            point[0],
            middle[0] + middle[1] / 2,
            "dummy points lie at the middle of their intermediate column"
        );
        assert_eq!(placed.through[2].len(), 1, "long feedback edges also retain their dummy points");
        assert_eq!(
            placed.layers.iter().map(Vec::len).sum::<usize>(),
            g.nodes.len(),
            "output layers contain original nodes only"
        );
        assert_no_overlaps(&g, &placed, &o);
    }

    #[test]
    fn port_order_removes_a_crossing() {
        let graph = || LGraph {
            nodes: vec![ic(2, &[], &[1], 0), ic(2, &[], &[1], 1), ic(6, &[1, 5], &[], 2)],
            edges: vec![
                LEdge { from: (0, 0), to: (2, 1), weight: 1, drawn: true },
                LEdge { from: (1, 0), to: (2, 0), weight: 1, drawn: true },
            ],
            lead: None,
            turns: Vec::new(),
        };
        let g = graph();
        // A driver height plus this gap equals the load's port separation,
        // making both edges straight without compromising box separation.
        let o = LOpts { node_gap: 2, ..LOpts::default() };
        let placed = place(&g, &o);
        assert_eq!(placed.crossings, 0, "port-aware sweeps eliminate the initial crossing");
        assert_eq!(placed.layers[0], vec![1, 0], "drivers follow the load's top-to-bottom port order");
        assert!(placed.pos[1][1] < placed.pos[0][1], "the top-port driver is placed above the bottom-port driver");
        for edge in &g.edges {
            assert_eq!(
                port_at(&g, &placed, edge.from.0, edge.from.1)[1],
                port_at(&g, &placed, edge.to.0, edge.to.1)[1],
                "both port-ordered edges are straight when the box spacing permits it"
            );
        }
        assert_no_overlaps(&g, &placed, &o);
        assert_eq!(placed, place(&graph(), &o), "rebuilding the same graph preserves deterministic placement");
    }
}
