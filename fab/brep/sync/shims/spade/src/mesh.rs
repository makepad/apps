use super::{side, turn, Point2};
use makepad_csg_boolean::cdt::{Point2 as CdtPoint, CDT};
use std::collections::{HashMap, HashSet};

pub(crate) struct Edge {
    pub vertices: [usize; 2],
    pub face: usize,
    pub twin: usize,
    pub constraint: bool,
}

#[derive(Default)]
pub(crate) struct Mesh {
    pub edges: Vec<Edge>,
    pub inner_faces: usize,
}

impl Mesh {
    pub fn build(points: &[Point2<f64>], constraints: &[[usize; 2]], requested: &HashSet<[usize; 2]>) -> Self {
        let Some(first) = points.first() else { return Self::default() };
        let mut min = CdtPoint { x: first.x, y: first.y };
        let mut max = min;
        for p in points {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        let mut cdt = CDT::new_without_pool_cancellation(min, max);
        let ids: Vec<_> = points.iter().map(|p| cdt.insert_point(p.x, p.y)).collect();
        for &[a, b] in constraints {
            cdt.add_constraint(ids[a], ids[b]);
        }
        cdt.finalize();
        // Both CDT extractors already subtract the supertriangle offset. Each unique point
        // was inserted once, in fixed-handle order, so their indices are our handles.
        let enforced: HashSet<_> = cdt.constrained_edges().into_iter()
            .map(|(a, b)| side(a as usize, b as usize)).collect();
        if let Some(missing) = requested.difference(&enforced).next() {
            panic!("cap CDT invariant: constraint {missing:?} was not enforced");
        }
        let mut triangles: Vec<_> = cdt.get_triangles().into_iter().map(|t| {
            let mut t = t.map(|v| v as usize);
            let [a, b, c] = t.map(|v| points[v]);
            let area = turn(a, b, c);
            assert!(area != 0.0, "cap CDT invariant: degenerate face {t:?}");
            if area < 0.0 {
                t.swap(1, 2);
            }
            let first = if t[0] < t[1] && t[0] < t[2] { 0 } else if t[1] < t[2] { 1 } else { 2 };
            t.rotate_left(first);
            t
        }).collect();
        triangles.sort_unstable();
        let mesh = if triangles.is_empty() && points.iter().skip(2).all(|&p| turn(points[0], points[1], p) == 0.0) {
            Self::collinear(points, requested)
        } else {
            Self::from_faces(&triangles, requested)
        };
        mesh.check(points.len(), requested);
        mesh
    }

    fn from_faces(triangles: &[[usize; 3]], constraints: &HashSet<[usize; 2]>) -> Self {
        let mut edges: Vec<Edge> = Vec::with_capacity(triangles.len() * 3);
        let mut unmatched = HashMap::new();
        for (face, &[a, b, c]) in triangles.iter().enumerate() {
            for [a, b] in [[a, b], [b, c], [c, a]] {
                let index = edges.len();
                let twin = match unmatched.remove(&[b, a]) {
                    Some(twin) => {
                        let opposite: &mut Edge = &mut edges[twin];
                        opposite.twin = index;
                        twin
                    }
                    None => {
                        assert!(unmatched.insert([a, b], index).is_none(), "cap CDT invariant: duplicate directed edge");
                        usize::MAX
                    }
                };
                edges.push(Edge { vertices: [a, b], face: face + 1, twin, constraint: constraints.contains(&side(a, b)) });
            }
        }
        // Walk each outer boundary in order. At the destination, turn through the fan
        // of inner faces until the next exposed edge. There are no hash-order-dependent IDs.
        for start in 0..triangles.len() * 3 {
            if edges[start].twin != usize::MAX { continue; }
            let mut inner = start;
            loop {
                let [a, b] = edges[inner].vertices;
                let constraint = edges[inner].constraint;
                edges[inner].twin = edges.len();
                edges.push(Edge { vertices: [b, a], face: 0, twin: inner, constraint });
                loop {
                    inner = (inner / 3) * 3 + (inner % 3 + 2) % 3;
                    let twin = edges[inner].twin;
                    if twin == usize::MAX || inner == start { break; }
                    assert!(edges[twin].face != 0, "cap CDT invariant: hull walk entered another boundary");
                    inner = twin;
                }
                if inner == start { break; }
            }
        }
        Self { edges, inner_faces: triangles.len() }
    }

    // A point or a line has only an outer face. Its hull walks the line in both directions.
    fn collinear(points: &[Point2<f64>], constraints: &HashSet<[usize; 2]>) -> Self {
        let mut order: Vec<_> = (0..points.len()).collect();
        order.sort_unstable_by(|&a, &b| {
            points[a].x.total_cmp(&points[b].x).then_with(|| points[a].y.total_cmp(&points[b].y))
        });
        let mut edges = Vec::new();
        let segments = order.len().saturating_sub(1);
        for pair in order.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let constraint = constraints.contains(&side(a, b));
            edges.push(Edge { vertices: [a, b], face: 0, twin: 2 * segments - 1 - edges.len(), constraint });
        }
        for twin in (0..segments).rev() {
            let [a, b] = edges[twin].vertices;
            let constraint = edges[twin].constraint;
            edges.push(Edge { vertices: [b, a], face: 0, twin, constraint });
        }
        Self { edges, inner_faces: 0 }
    }

    fn check(&self, vertices: usize, constraints: &HashSet<[usize; 2]>) {
        let mut present = vec![false; vertices];
        let mut missing = constraints.clone();
        for edge in &self.edges {
            let [a, b] = edge.vertices;
            present[a] = true;
            if edge.constraint {
                missing.remove(&side(a, b));
            }
        }
        if let Some(side) = missing.iter().next() {
            panic!("cap CDT invariant: no output edge for constraint {side:?}");
        }
        if vertices > 1 {
            if let Some(v) = present.iter().position(|&seen| !seen) {
                panic!("cap CDT invariant: inserted vertex {v} missing from output");
            }
        }
    }
}
