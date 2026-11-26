use crate::order::{assign_layers, Layers, Vertex};
use crate::{
    extent_valid, gap, index_boxes, index_links, Direction, Edge, ItemKind, Link, Node, Options, Placed,
    Point, Polyline,
};
use std::collections::BTreeMap;

/// Place weakly connected components side by side in first-node order.
/// Spacing is in canvas units. Negative and nonfinite gaps become zero.
/// A long edge takes one routing slot per intermediate layer.
pub fn layered(nodes: &[Node], edges: &[Edge], options: &Options) -> Placed {
    let mut placed = Placed::default();
    let (boxes, indices) = index_boxes(
        nodes,
        ItemKind::Node,
        |n| (n.id, extent_valid(n.width) && extent_valid(n.height)),
        &mut placed.skipped,
    );
    let links =
        index_links(edges, ItemKind::Edge, |e| (e.id, e.from, e.to, true), &indices, &mut placed.skipped);

    placed.nodes = boxes.iter().map(|n| (n.id, 0.0, 0.0)).collect();
    placed.edges = links.iter().map(|e| (edges[e.index].id, Vec::new())).collect();
    let mut feedback = vec![false; links.len()];
    let mut left = 0.0;
    for component in components(boxes.len(), &links) {
        left += component.place(&boxes, options, left, &mut placed, &mut feedback) + gap(options.node_gap);
    }
    placed.reversed =
        placed.edges.iter().zip(feedback).filter_map(|((id, _), reversed)| reversed.then_some(*id)).collect();
    placed
}

#[derive(Default)]
struct Component {
    /// Indices into the accepted boxes, in input order.
    members: Vec<usize>,
    /// Local endpoints and the edge's slot in the output.
    links: Vec<Link>,
}

fn components(count: usize, links: &[Link]) -> Vec<Component> {
    let mut neighbours = vec![Vec::new(); count];
    for edge in links {
        if edge.from != edge.to {
            neighbours[edge.from].push(edge.to);
            neighbours[edge.to].push(edge.from);
        }
    }
    let mut owner = vec![usize::MAX; count];
    let mut parts = Vec::new();
    let mut stack = Vec::new();
    for root in 0..count {
        if owner[root] != usize::MAX {
            continue;
        }
        let part = parts.len();
        parts.push(Component::default());
        owner[root] = part;
        stack.push(root);
        while let Some(node) = stack.pop() {
            for &other in &neighbours[node] {
                if owner[other] == usize::MAX {
                    owner[other] = part;
                    stack.push(other);
                }
            }
        }
    }
    let mut local = vec![0; count];
    for (node, &part) in owner.iter().enumerate() {
        local[node] = parts[part].members.len();
        parts[part].members.push(node);
    }
    for (index, edge) in links.iter().enumerate() {
        parts[owner[edge.from]].links.push(Link { index, from: local[edge.from], to: local[edge.to] });
    }
    parts
}

// All placement and routing uses (across, along). Transpose only at the boundary.
fn orient(direction: Direction, across: f64, along: f64) -> Point {
    match direction {
        Direction::Down => (across, along),
        Direction::Right => (along, across),
    }
}

const LOOP_SPACING: f64 = 8.0;

impl Component {
    fn place(
        &self,
        boxes: &[&Node],
        options: &Options,
        left: f64,
        placed: &mut Placed,
        feedback: &mut [bool],
    ) -> f64 {
        let rank = assign_layers(self.members.len(), &self.links);
        let mut layers = Layers {
            vertices: Vec::new(),
            rows: vec![Vec::new(); rank.iter().max().copied().unwrap_or(0) + 1],
        };
        for (i, &member) in self.members.iter().enumerate() {
            let node = boxes[member];
            let (breadth, depth) = orient(options.direction, node.width, node.height);
            layers.vertices.push(Vertex { breadth, depth, ..Vertex::default() });
            layers.rows[rank[i]].push(i);
        }

        let mut groups = BTreeMap::new();
        let mut routes = Vec::with_capacity(self.links.len());
        for link in &self.links {
            let backward = rank[link.from] > rank[link.to];
            feedback[link.index] = backward;
            let (start, end) = if backward { (link.to, link.from) } else { (link.from, link.to) };
            let count = groups.entry((start, end)).or_insert(0usize);
            let lane = *count;
            *count += 1;
            let mut through = Vec::new();
            if start == end {
                layers.vertices[start].loop_room = LOOP_SPACING * *count as f64;
            } else {
                let mut previous = start;
                for row in &mut layers.rows[rank[start] + 1..rank[end]] {
                    let dummy = layers.vertices.len();
                    layers.vertices.push(Vertex::default());
                    layers.vertices[previous].after.push(dummy);
                    layers.vertices[dummy].before.push(previous);
                    row.push(dummy);
                    through.push(dummy);
                    previous = dummy;
                }
                layers.vertices[previous].after.push(end);
                layers.vertices[end].before.push(previous);
            }
            routes.push(Route { start, end, lane, through });
        }

        layers.reduce_crossings(self.members.len());
        let geometry = Geometry::new(&layers, gap(options.node_gap), gap(options.layer_gap));
        let to_canvas = |(across, along)| {
            let (x, y) = orient(options.direction, across, along);
            (left + x, y)
        };
        for (i, &member) in self.members.iter().enumerate() {
            let v = &layers.vertices[i];
            let (x, y) = to_canvas((geometry.centers[i] - v.breadth * 0.5, geometry.top(i, v)));
            placed.nodes[member] = (boxes[member].id, x, y);
        }
        for (route, link) in routes.iter().zip(&self.links) {
            let count = groups[&(route.start, route.end)];
            let mut points = route.points(&geometry, &layers.vertices, count);
            if feedback[link.index] {
                points.reverse();
            }
            for p in &mut points {
                *p = to_canvas(*p);
            }
            placed.edges[link.index].1 = points;
        }
        orient(options.direction, geometry.breadth, geometry.depth).0
    }
}

struct Geometry {
    centers: Vec<f64>,
    /// Near and far boundaries of the layer occupied by each vertex.
    bands: Vec<(f64, f64)>,
    breadth: f64,
    depth: f64,
}

impl Geometry {
    fn new(layers: &Layers, node_gap: f64, layer_gap: f64) -> Self {
        let mut packed = vec![0.0; layers.vertices.len()];
        let mut bands = vec![(0.0, 0.0); packed.len()];
        let mut depth = 0.0;
        for (i, row) in layers.rows.iter().enumerate() {
            let mut width = 0.0;
            let mut height: f64 = 0.0;
            for &node in row {
                let v = &layers.vertices[node];
                packed[node] = width + v.breadth * 0.5;
                width += v.breadth + v.loop_room + node_gap;
                height = height.max(v.depth);
            }
            if i > 0 {
                depth += layer_gap;
            }
            for &node in row {
                packed[node] -= (width - node_gap) * 0.5;
                bands[node] = (depth, depth + height);
            }
            depth += height;
        }
        let mut centers = packed.clone();
        let mut blocks = Vec::new();
        for _ in 0..2 {
            for row in layers.rows.iter().skip(1) {
                align_row(row, &layers.vertices, true, &packed, &mut centers, &mut blocks);
            }
            for row in layers.rows.iter().rev().skip(1) {
                align_row(row, &layers.vertices, false, &packed, &mut centers, &mut blocks);
            }
        }

        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for (&center, v) in centers.iter().zip(&layers.vertices) {
            min = min.min(center - v.breadth * 0.5);
            max = max.max(center + v.breadth * 0.5 + v.loop_room);
        }
        for center in &mut centers {
            *center -= min;
        }
        Self { centers, bands, breadth: max - min, depth }
    }

    fn top(&self, node: usize, v: &Vertex) -> f64 {
        let (near, far) = self.bands[node];
        near + (far - near - v.depth) * 0.5
    }
}

struct Block {
    first: usize,
    weight: f64,
    shift: f64,
}

// The packed row already has the required gaps. Its shifts must be
// nondecreasing. Pool adjacent violations to fit neighbour centres in O(row + edges).
fn align_row(
    row: &[usize],
    vertices: &[Vertex],
    down: bool,
    packed: &[f64],
    centers: &mut [f64],
    blocks: &mut Vec<Block>,
) {
    blocks.clear();
    for (first, &node) in row.iter().enumerate() {
        let neighbours = if down { &vertices[node].before } else { &vertices[node].after };
        let weight = neighbours.len().max(1) as f64;
        let target = if neighbours.is_empty() {
            centers[node]
        } else {
            neighbours.iter().map(|&other| centers[other]).sum::<f64>() / weight
        };
        let mut block = Block { first, weight, shift: target - packed[node] };
        while let Some(prev) = blocks.last() {
            if prev.shift <= block.shift {
                break;
            }
            block.first = prev.first;
            block.weight += prev.weight;
            block.shift += (prev.shift - block.shift) * (prev.weight / block.weight);
            blocks.pop();
        }
        blocks.push(block);
    }
    let mut end = row.len();
    for block in blocks.iter().rev() {
        for &node in &row[block.first..end] {
            centers[node] = packed[node] + block.shift;
        }
        end = block.first;
    }
}

struct Route {
    start: usize,
    end: usize,
    lane: usize,
    through: Vec<usize>,
}

impl Route {
    fn points(&self, g: &Geometry, vertices: &[Vertex], count: usize) -> Polyline {
        let v = &vertices[self.start];
        let top = g.top(self.start, v);
        if self.start == self.end {
            let side = g.centers[self.start] + v.breadth * 0.5;
            let turn = side + LOOP_SPACING * (self.lane + 1) as f64;
            let inset = v.depth * 0.25 * (count - self.lane) as f64 / count as f64;
            let (a, b) = (top + inset, top + v.depth - inset);
            return vec![(side, a), (turn, a), (turn, b), (side, b)];
        }
        let port = (self.lane as f64 + 0.5) / count as f64 - 0.5;
        let from = g.centers[self.start] + port * v.breadth;
        let target = &vertices[self.end];
        let to = g.centers[self.end] + port * target.breadth;
        let mut path = vec![(from, top + v.depth)];
        // Turn only in the open channel. Cross each occupied layer on its
        // dummy's track, even when a neighbouring box fills the layer depth.
        extend(&mut path, (from, g.bands[self.start].1));
        for &dummy in &self.through {
            let x = g.centers[dummy];
            let (near, far) = g.bands[dummy];
            extend(&mut path, (x, near));
            extend(&mut path, (x, far));
        }
        extend(&mut path, (to, g.bands[self.end].0));
        let end = (to, g.top(self.end, target));
        extend(&mut path, end);
        if path.len() == 1 {
            path.push(end);
        }
        path
    }
}

fn extend(path: &mut Polyline, point: Point) {
    if path.last() == Some(&point) {
        return;
    }
    if let [.., a, b] = path.as_slice() {
        if (a.0 == b.0 && b.0 == point.0) || (a.1 == b.1 && b.1 == point.1) {
            path.pop();
        }
    }
    path.push(point);
}
