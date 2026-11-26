//! Integer placement costs. Cache each net's box and same-side ratsnest edges.
//! A move rebuilds only incident nets, querying an edge index for their crossing partners.

use super::{PNet, PinRef, PlaceProblem, Pose};
use fab_elec_core::{geom::orient, BBox, Index, Side, P};
use std::ops::ControlFlow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CostTerms {
    /// Sum of weighted net half-perimeters, in nm (rounded down per net).
    pub wirelength: i64,
    /// Strict crossings between edges of different nets on the same placement side.
    pub crossings: i64,
}

#[derive(Clone, Copy)]
struct Edge {
    a: P,
    b: P,
    side: Side,
}

impl Edge {
    fn bbox(self) -> BBox {
        BBox::new(self.a, self.b)
    }

    fn crosses(self, other: Edge) -> bool {
        fn opposite(a: i128, b: i128) -> bool {
            (a < 0 && b > 0) || (a > 0 && b < 0)
        }
        self.side == other.side
            && opposite(orient(self.a, self.b, other.a), orient(self.a, self.b, other.b))
            && opposite(orient(other.a, other.b, self.a), orient(other.a, other.b, self.b))
    }
}

struct NetState {
    bbox: BBox,
    edges: Vec<Edge>,
}

impl NetState {
    fn new(problem: &PlaceProblem, net: &PNet, poses: &[Pose], moved: Option<(usize, Pose)>) -> Self {
        let pins: Vec<_> = net.pins.iter().enumerate().map(|(i, pin)| match *pin {
            PinRef::Mov(m, pad) => {
                let pose = match moved {
                    Some((ix, pose)) if ix == m => pose,
                    _ => poses[m],
                };
                (problem.movables[m].pad_at(pad, pose), pose.side)
            }
            PinRef::Fixed(p) => (p, net.start_sides[i]),
        }).collect();
        Self { bbox: BBox::of_points(pins.iter().map(|pin| pin.0)), edges: mst_edges(&pins) }
    }
}

/// Dense Prim as in the board's private `connect::net_ratsnest`, with one cluster per pad.
/// All pins participate. Tie order is (distance squared, tree pin, outside pin).
/// An edge between opposite-side parts has no single side on which to charge a crossing.
fn mst_edges(pins: &[(P, Side)]) -> Vec<Edge> {
    let mut edges = Vec::with_capacity(pins.len().saturating_sub(1));
    if pins.len() < 2 { return edges }
    let mut in_tree = vec![false; pins.len()];
    let mut nearest = vec![(i128::MAX, usize::MAX); pins.len()];
    let mut joined = 0;
    for _ in 1..pins.len() {
        in_tree[joined] = true;
        let mut best = (i128::MAX, usize::MAX, usize::MAX);
        for (b, pin) in pins.iter().enumerate() {
            if !in_tree[b] {
                nearest[b] = nearest[b].min((pins[joined].0.dist2(pin.0), joined));
                let (dist, a) = nearest[b];
                best = best.min((dist, a, b));
            }
        }
        let (_, a, b) = best;
        if pins[a].1 == pins[b].1 {
            edges.push(Edge { a: pins[a].0, b: pins[b].0, side: pins[a].1 });
        }
        joined = b;
    }
    edges
}

pub struct Cost<'a> {
    problem: &'a PlaceProblem,
    poses: Vec<Pose>,
    nets: Vec<NetState>,
    incident: Vec<Vec<usize>>,
    index: Index<(usize, usize)>,
    total: CostTerms,
}

impl<'a> Cost<'a> {
    pub fn new(problem: &'a PlaceProblem, poses: &[Pose]) -> Self {
        assert_eq!(poses.len(), problem.movables.len(), "each movable needs a pose");
        let mut incident = vec![Vec::new(); poses.len()];
        let mut total = CostTerms::default();
        let mut entries = Vec::new();
        let mut nets = Vec::with_capacity(problem.nets.len());
        for (i, net) in problem.nets.iter().enumerate() {
            for pin in &net.pins {
                if let PinRef::Mov(m, _) = *pin {
                    if incident[m].last() != Some(&i) { incident[m].push(i); }
                }
            }
            let state = NetState::new(problem, net, poses, None);
            total.wirelength += net.wirelength(state.bbox);
            entries.extend(state.edges.iter().enumerate().map(|(j, e)| (e.bbox(), (i, j))));
            nets.push(state);
        }
        let mut cost = Self { problem, poses: poses.to_vec(), nets, incident, index: Index::build(entries), total };
        cost.total.crossings = cost.nets.iter().enumerate().map(|(i, n)| cost.indexed_crossings(n, |j| j < i)).sum();
        cost
    }

    pub fn terms(&self) -> CostTerms {
        self.total
    }

    fn indexed_crossings(&self, state: &NetState, include: impl Fn(usize) -> bool) -> i64 {
        let mut count = 0;
        for edge in &state.edges {
            self.index.query(edge.bbox(), |_, (n, e)| {
                if include(n) && edge.crosses(self.nets[n].edges[e]) { count += 1; }
                ControlFlow::Continue(())
            });
        }
        count
    }

    fn changes(&self, m: usize, pose: Pose) -> (CostTerms, Vec<NetState>) {
        let affected = &self.incident[m];
        let mut states: Vec<NetState> = Vec::with_capacity(affected.len());
        let mut delta = CostTerms::default();
        for &i in affected {
            let net = &self.problem.nets[i];
            let old = &self.nets[i];
            let new = NetState::new(self.problem, net, &self.poses, Some((m, pose)));
            delta.wirelength += net.wirelength(new.bbox) - net.wirelength(old.bbox);
            // Remove crossings to unchanged nets and to earlier changed nets, once per pair.
            let unchanged = |j| affected.binary_search(&j).is_err();
            delta.crossings -= self.indexed_crossings(old, |j| j < i || unchanged(j));
            delta.crossings += self.indexed_crossings(&new, unchanged);
            // Both ends of a changed/changed pair must be tested at the proposed placement.
            for other in &states {
                if new.bbox.intersects(&other.bbox) {
                    for a in &new.edges {
                        delta.crossings += other.edges.iter().filter(|&&b| a.crosses(b)).count() as i64;
                    }
                }
            }
            states.push(new);
        }
        (delta, states)
    }

    pub fn delta(&self, movable: usize, pose: Pose) -> CostTerms {
        self.changes(movable, pose).0
    }

    pub fn apply(&mut self, movable: usize, pose: Pose) {
        let (delta, changes) = self.changes(movable, pose);
        for (&i, new) in self.incident[movable].iter().zip(changes) {
            for (j, edge) in self.nets[i].edges.iter().enumerate() {
                self.index.remove(edge.bbox(), (i, j));
            }
            for (j, edge) in new.edges.iter().enumerate() {
                self.index.insert(edge.bbox(), (i, j));
            }
            self.nets[i] = new;
        }
        self.total.wirelength += delta.wirelength;
        self.total.crossings += delta.crossings;
        self.poses[movable] = pose;
    }
}
