//! Wire currents from component pin currents.

/// A net's wire graph.
#[derive(Clone, Debug, Default)]
pub struct NetTree {
    /// Node count; nodes are `0..nodes`.
    pub nodes: u32,
    /// Segment endpoints; current is positive from the first node to the second. Equal endpoints carry zero.
    pub segs: Vec<(u32, u32)>,
    /// Each segment's live row (from the host's segment-row allocator), parallel to `segs`; empty until the host has allocated them.
    pub rows: Vec<u32>,
    /// Pins as (node, index into the pin-current slice).
    pub pins: Vec<(u32, u32)>,
    /// Nodes where an island exchanges current with the rest of the net.
    pub ports: Vec<u32>,
    /// Positive segment conductance for loops; missing or invalid entries use 1.
    pub conductance: Vec<f32>,
}

const NONE: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Node {
    head: usize,
    degree: usize,
    island: usize,
    draw: f64,
}

impl Node {
    const EMPTY: Self = Self { head: NONE, degree: 0, island: NONE, draw: 0.0 };
}

#[derive(Clone, Copy, Default)]
struct Island {
    draw: f64,
    pins: usize,
    ports: usize,
}

struct Link {
    other: usize,
    segment: usize,
    next: usize,
}

#[derive(Clone, Copy, Default)]
struct Edge {
    active: bool,
    conductance: f64,
}

#[derive(Clone, Copy, Default)]
struct SolveNode {
    diagonal: f64,
    phi: f64,
    best_phi: f64,
    residual: f64,
    direction: f64,
    product: f64,
}

/// Reusable scratch; storage grows only when node or segment counts grow.
#[derive(Default)]
pub struct NetCurrents {
    nodes: Vec<Node>,
    islands: Vec<Island>,
    links: Vec<Link>,
    edges: Vec<Edge>,
    queue: Vec<usize>,
    solve: Vec<SolveNode>,
    unconverged: u64,
}

impl NetCurrents {
    /// Start with empty scratch storage.
    pub fn new() -> NetCurrents {
        Self::default()
    }

    /// Number of loop components that have missed the residual tolerance.
    pub fn unconverged(&self) -> u64 {
        self.unconverged
    }

    /// Write segment currents, positive from a to b, clearing `out` first.
    /// `pin_i[j]` is amperes drawn out of the net into component pin j.
    /// Keep `out`'s capacity to avoid allocations on later calls of the same size.
    pub fn currents(&mut self, tree: &NetTree, pin_i: &[f32], out: &mut Vec<f32>) {
        let count = tree.nodes as usize;
        out.clear();
        out.resize(tree.segs.len(), 0.0);
        self.nodes.resize(count, Node::EMPTY);
        self.nodes.fill(Node::EMPTY);
        self.islands.resize(count, Island::default());
        self.islands.fill(Island::default());
        self.edges.resize(tree.segs.len(), Edge::default());
        self.edges.fill(Edge::default());
        self.solve.resize(count, SolveNode::default());
        // Reserve for any topology of this size, including a later loop or one large island.
        self.queue.resize(count, 0);
        self.queue.clear();
        self.links.clear();
        self.links.reserve(tree.segs.len() * 2);

        for (k, &(a, b)) in tree.segs.iter().enumerate() {
            let (Some(a), Some(b)) = (node_index(a, count), node_index(b, count)) else {
                continue;
            };
            if a == b {
                continue;
            }
            let g = tree.conductance.get(k).copied().unwrap_or(1.0);
            self.edges[k] = Edge { active: true, conductance: if g.is_finite() && g > 0.0 { g as f64 } else { 1.0 } };
            self.links.push(Link { other: b, segment: k, next: self.nodes[a].head });
            self.nodes[a].head = self.links.len() - 1;
            self.links.push(Link { other: a, segment: k, next: self.nodes[b].head });
            self.nodes[b].head = self.links.len() - 1;
            self.nodes[a].degree += 1;
            self.nodes[b].degree += 1;
        }

        for root in 0..count {
            if self.nodes[root].island == NONE {
                self.collect_component(root);
            }
        }
        for &(n, pin) in &tree.pins {
            let Some(n) = node_index(n, count) else { continue };
            let draw = pin_i.get(pin as usize).copied().unwrap_or(0.0) as f64;
            self.nodes[n].draw += draw;
            let island = &mut self.islands[self.nodes[n].island];
            island.draw += draw;
            island.pins += 1;
        }
        for &n in &tree.ports {
            let Some(n) = node_index(n, count) else { continue };
            self.islands[self.nodes[n].island].ports += 1;
        }
        for &n in &tree.ports {
            let Some(n) = node_index(n, count) else { continue };
            let island = self.islands[self.nodes[n].island];
            self.nodes[n].draw -= island.draw / island.ports as f64;
        }
        for &(n, _) in &tree.pins {
            let Some(n) = node_index(n, count) else { continue };
            let island = self.islands[self.nodes[n].island];
            if island.ports == 0 {
                self.nodes[n].draw -= island.draw / island.pins as f64;
            }
        }

        self.queue.clear();
        for n in 0..count {
            if self.nodes[n].degree == 1 {
                self.queue.push(n);
            }
        }
        let mut cursor = 0;
        while cursor < self.queue.len() {
            let n = self.queue[cursor];
            cursor += 1;
            if self.nodes[n].degree != 1 {
                continue;
            }
            let mut link = self.nodes[n].head;
            while link != NONE {
                let edge = &self.links[link];
                let k = edge.segment;
                let other = edge.other;
                if self.edges[k].active {
                    let draw = self.nodes[n].draw;
                    out[k] = if tree.segs[k].0 as usize == n { -draw as f32 } else { draw as f32 };
                    self.edges[k].active = false;
                    self.nodes[n].degree = 0;
                    self.nodes[other].degree -= 1;
                    self.nodes[other].draw += draw;
                    if self.nodes[other].degree == 1 {
                        self.queue.push(other);
                    }
                    break;
                }
                link = edge.next;
            }
        }

        // Remove peeled links once so loop iterations visit only the core's segments.
        for node in &mut self.nodes {
            node.island = NONE;
            let mut link = node.head;
            node.head = NONE;
            while link != NONE {
                let edge = &mut self.links[link];
                let next = edge.next;
                if self.edges[edge.segment].active {
                    edge.next = node.head;
                    node.head = link;
                }
                link = next;
            }
        }
        for root in 0..count {
            if self.nodes[root].degree > 0 && self.nodes[root].island == NONE {
                self.collect_component(root);
                self.solve_core();
            }
        }
        for (k, &(a, b)) in tree.segs.iter().enumerate() {
            if self.edges[k].active {
                out[k] = (self.edges[k].conductance * (self.solve[a as usize].best_phi - self.solve[b as usize].best_phi)) as f32;
            }
        }
    }

    fn collect_component(&mut self, root: usize) {
        self.queue.clear();
        self.queue.push(root);
        self.nodes[root].island = root;
        let mut cursor = 0;
        while cursor < self.queue.len() {
            let n = self.queue[cursor];
            cursor += 1;
            let mut link = self.nodes[n].head;
            while link != NONE {
                let edge = &self.links[link];
                if self.edges[edge.segment].active && self.nodes[edge.other].island == NONE {
                    self.nodes[edge.other].island = root;
                    self.queue.push(edge.other);
                }
                link = edge.next;
            }
        }
    }

    fn solve_core(&mut self) {
        let ground = self.queue[0];
        let mut rhs_squared = 0.0;
        let mut rho = 0.0;
        for &n in &self.queue {
            self.solve[n] = SolveNode::default();
            rhs_squared += self.nodes[n].draw * self.nodes[n].draw;
            if n == ground {
                continue;
            }
            let mut diagonal = 0.0;
            let mut link = self.nodes[n].head;
            while link != NONE {
                let edge = &self.links[link];
                if self.edges[edge.segment].active {
                    diagonal += self.edges[edge.segment].conductance;
                }
                link = edge.next;
            }
            let residual = -self.nodes[n].draw;
            let direction = residual / diagonal;
            self.solve[n].diagonal = diagonal;
            self.solve[n].residual = residual;
            self.solve[n].direction = direction;
            rho += residual * direction;
        }
        if rhs_squared == 0.0 {
            return;
        }
        let tolerance_squared = rhs_squared * 1e-12;
        let mut best_squared = rhs_squared;
        for _ in 0..self.queue.len().saturating_mul(4) {
            let mut denominator = 0.0;
            for &n in self.queue.iter().skip(1) {
                let mut product = 0.0;
                let mut link = self.nodes[n].head;
                while link != NONE {
                    let edge = &self.links[link];
                    if self.edges[edge.segment].active {
                        product += self.edges[edge.segment].conductance * (self.solve[n].direction - self.solve[edge.other].direction);
                    }
                    link = edge.next;
                }
                self.solve[n].product = product;
                denominator += self.solve[n].direction * product;
            }
            if denominator <= 0.0 || !denominator.is_finite() || !rho.is_finite() {
                break;
            }
            let alpha = rho / denominator;
            for &n in self.queue.iter().skip(1) {
                let state = &mut self.solve[n];
                state.phi += alpha * state.direction;
                state.residual -= alpha * state.product;
            }
            // Measure the actual KCL error, including the grounded node, before keeping a solution.
            let residual_squared = self.residual_squared();
            if residual_squared < best_squared {
                best_squared = residual_squared;
                for &n in &self.queue {
                    self.solve[n].best_phi = self.solve[n].phi;
                }
            }
            if best_squared <= tolerance_squared {
                return;
            }
            let mut next_rho = 0.0;
            for &n in self.queue.iter().skip(1) {
                let state = &self.solve[n];
                next_rho += state.residual * (state.residual / state.diagonal);
            }
            if rho <= 0.0 || next_rho <= 0.0 || !next_rho.is_finite() {
                break;
            }
            let beta = next_rho / rho;
            for &n in self.queue.iter().skip(1) {
                let state = &mut self.solve[n];
                state.direction = state.residual / state.diagonal + beta * state.direction;
            }
            rho = next_rho;
        }
        self.unconverged = self.unconverged.saturating_add(1);
    }

    fn residual_squared(&self) -> f64 {
        let mut squared = 0.0;
        for &n in &self.queue {
            let mut residual = self.nodes[n].draw;
            let mut link = self.nodes[n].head;
            while link != NONE {
                let edge = &self.links[link];
                if self.edges[edge.segment].active {
                    residual += self.edges[edge.segment].conductance * (self.solve[n].phi - self.solve[edge.other].phi);
                }
                link = edge.next;
            }
            squared += residual * residual;
        }
        squared
    }
}

fn node_index(node: u32, count: usize) -> Option<usize> {
    let node = node as usize;
    debug_assert!(node < count, "net node index is out of range");
    (node < count).then_some(node)
}

/// Half the sum of the net's absolute pin currents, in amperes.
pub fn through_current(tree: &NetTree, pin_i: &[f32]) -> f32 {
    let mut sum = 0.0;
    for &(node, pin) in &tree.pins {
        if node_index(node, tree.nodes as usize).is_some() {
            sum += (pin_i.get(pin as usize).copied().unwrap_or(0.0) as f64).abs();
        }
    }
    (sum * 0.5) as f32
}
