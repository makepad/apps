use crate::Link;

/// Reverse DFS finishing order turns only ancestor-pointing back edges.
/// An explicit stack also handles chains deeper than the call stack.
fn cycle_order(successors: &[Vec<usize>]) -> Vec<usize> {
    let mut seen = vec![false; successors.len()];
    let mut stack = Vec::new();
    let mut order = Vec::with_capacity(successors.len());
    for root in 0..successors.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        stack.push((root, 0));
        while let Some((node, next)) = stack.last_mut() {
            if let Some(&child) = successors[*node].get(*next) {
                *next += 1;
                if !seen[child] {
                    seen[child] = true;
                    stack.push((child, 0));
                }
            } else {
                order.push(*node);
                stack.pop();
            }
        }
    }
    order.reverse();
    order
}

pub(super) fn assign_layers(count: usize, links: &[Link]) -> Vec<usize> {
    let mut successors = vec![Vec::new(); count];
    for edge in links {
        if edge.from != edge.to {
            successors[edge.from].push(edge.to);
        }
    }
    let order = cycle_order(&successors);
    let mut position = vec![0; count];
    for (i, &node) in order.iter().enumerate() {
        position[node] = i;
    }

    let mut predecessors = vec![0; count];
    for list in &mut successors {
        list.clear();
    }
    for edge in links {
        if edge.from == edge.to {
            continue;
        }
        let (from, to) =
            if position[edge.from] < position[edge.to] { (edge.from, edge.to) } else { (edge.to, edge.from) };
        successors[from].push(to);
        predecessors[to] += 1;
    }
    let mut rank = vec![0; count];
    for &node in &order {
        for &child in &successors[node] {
            rank[child] = rank[child].max(rank[node] + 1);
        }
    }
    // Moving one row later saves one segment per outgoing edge and costs one
    // per incoming edge. Stop just before the closest successor.
    for &node in order.iter().rev() {
        let children = &successors[node];
        if children.len() > predecessors[node] {
            if let Some(next) = children.iter().map(|&child| rank[child]).min() {
                rank[node] = next - 1;
            }
        }
    }
    rank
}

#[derive(Default)]
pub(super) struct Vertex {
    pub breadth: f64,
    pub depth: f64,
    pub loop_room: f64,
    pub before: Vec<usize>,
    pub after: Vec<usize>,
}

pub(super) struct Layers {
    pub vertices: Vec<Vertex>,
    pub rows: Vec<Vec<usize>>,
}

// Inversions between two rows count crossings. Query a whole source group
// before inserting it so edges sharing either endpoint never count.
fn crossings(layers: &Layers, position: &[usize]) -> u128 {
    let mut total = 0;
    let mut tree = Vec::new();
    for pair in layers.rows.windows(2) {
        if pair[0].len() < 2 || pair[1].len() < 2 {
            continue;
        }
        tree.clear();
        tree.resize(pair[1].len() + 1, 0usize);
        let mut inserted = 0;
        for &node in &pair[0] {
            let children = &layers.vertices[node].after;
            for &child in children {
                let mut i = position[child] + 1;
                let mut prefix = 0;
                while i > 0 {
                    prefix += tree[i];
                    i &= i - 1;
                }
                total += (inserted - prefix) as u128;
            }
            for &child in children {
                let mut i = position[child] + 1;
                while i < tree.len() {
                    tree[i] += 1;
                    i += i & i.wrapping_neg();
                }
            }
            inserted += children.len();
        }
    }
    total
}

fn sweep_row(
    row: &mut [usize],
    vertices: &[Vertex],
    down: bool,
    position: &mut [usize],
    key: &mut [f64],
) -> bool {
    if row.len() < 2 {
        return false;
    }
    for &node in row.iter() {
        let adjacent = if down { &vertices[node].before } else { &vertices[node].after };
        key[node] = if adjacent.is_empty() {
            position[node] as f64
        } else {
            adjacent.iter().map(|&other| position[other] as f64).sum::<f64>() / adjacent.len() as f64
        };
    }
    // Dense indices preserve input order, including edge order for dummies.
    row.sort_unstable_by(|&a, &b| key[a].total_cmp(&key[b]).then(a.cmp(&b)));
    let mut changed = false;
    for (i, &node) in row.iter().enumerate() {
        changed |= position[node] != i;
        position[node] = i;
    }
    changed
}

impl Layers {
    pub fn reduce_crossings(&mut self, real_nodes: usize) {
        let mut position = vec![0; self.vertices.len()];
        for row in &self.rows {
            for (i, &node) in row.iter().enumerate() {
                position[node] = i;
            }
        }
        let mut best_score = crossings(self, &position);
        if best_score == 0 {
            return;
        }
        let mut best_rows = self.rows.clone();
        let mut key = vec![0.0; self.vertices.len()];
        for _ in 0..(2 + real_nodes.div_ceil(64)) {
            let mut changed = false;
            for down in [true, false] {
                let last = self.rows.len() - 1;
                for i in 1..=last {
                    let row = if down { i } else { last - i };
                    changed |= sweep_row(&mut self.rows[row], &self.vertices, down, &mut position, &mut key);
                }
                let score = crossings(self, &position);
                if score == 0 {
                    return;
                }
                if score < best_score {
                    best_score = score;
                    best_rows.clone_from(&self.rows);
                }
            }
            if !changed {
                break;
            }
        }
        self.rows = best_rows;
    }
}
