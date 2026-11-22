use super::{f32_at, near, u32_at, Fault, Rule, SceneRecord, Segment};
use crate::format;
use std::collections::HashMap;

/// Check an edge table against its capacity and scene record, returning edges as stored.
pub fn read_vector(bytes: &[u8], capacity: u32, scene: &SceneRecord) -> Result<Vec<Segment>, Fault> {
    if bytes.len() < 56 { return Err(Fault::new(Rule::VectorSize, format!("{} bytes, expected at least 56", bytes.len()))); }
    let count = u32_at(bytes, 40);
    let expected = 56 + 17 * count as u64;
    if bytes.len() as u64 != expected { return Err(Fault::new(Rule::VectorSize, format!("{} bytes, expected {expected} for {count} segments", bytes.len()))); }
    let tail = bytes.len() - 8;
    if bytes[..4] != format::VECTOR_START || bytes[32..36] != format::VECTOR_SEGMENTS_START
        || bytes[tail..tail + 4] != format::VECTOR_SEGMENTS_END || bytes[tail + 4..] != format::VECTOR_END {
        return Err(Fault::new(Rule::VectorMarkers, format!("markers {:?}, {:?}, {:?}, {:?}; expected {:?}, {:?}, {:?}, {:?}", &bytes[..4], &bytes[32..36], &bytes[tail..tail + 4], &bytes[tail + 4..], format::VECTOR_START, format::VECTOR_SEGMENTS_START, format::VECTOR_SEGMENTS_END, format::VECTOR_END)));
    }
    if count > capacity { return Err(Fault::new(Rule::VectorCapacity, format!("{count} segments, capacity {capacity}"))); }
    let mut segments = Vec::with_capacity(count as usize);
    let mut previous = f32::NEG_INFINITY;
    for k in 0..count as usize {
        let at = 48 + 17 * k;
        let from = [f32_at(bytes, at), f32_at(bytes, at + 4)];
        let to = [f32_at(bytes, at + 8), f32_at(bytes, at + 12)];
        let flag = bytes[at + 16];
        if flag > 1 || from[1] > to[1] || (flag == 0 && from[1] >= to[1]) || from[1] < previous {
            return Err(Fault::new(Rule::SegmentOrder, format!("segment {k}: {from:?} to {to:?}, flag {flag}, previous start y {previous}")));
        }
        previous = from[1];
        segments.push(Segment { from, to, swapped: flag == 1 });
    }
    let mut starts = HashMap::with_capacity(segments.len());
    for (k, segment) in segments.iter().enumerate() {
        let (from, _) = directed(segment);
        if starts.insert(key(from), k).is_some() {
            return Err(Fault::new(Rule::Closed, format!("segment {k}: repeated contour start {from:?}")));
        }
    }
    let mut successors = Vec::with_capacity(segments.len());
    let mut incoming = vec![0usize; segments.len()];
    for (k, segment) in segments.iter().enumerate() {
        let (_, to) = directed(segment);
        let next = *starts.get(&key(to)).ok_or_else(|| Fault::new(Rule::Closed, format!("segment {k}: end {to:?} has no matching start")))?;
        incoming[next] += 1;
        successors.push(next);
    }
    if incoming.iter().any(|count| *count != 1) {
        return Err(Fault::new(Rule::Closed, "each contour start must have exactly one incoming edge"));
    }
    for (k, segment) in segments.iter().enumerate() {
        for value in segment.from.into_iter().chain(segment.to) {
            let rounded = ((value as f64 * 1000.0).round() / 1000.0) as f32;
            if !value.is_finite() || value != rounded {
                return Err(Fault::new(Rule::Rounding, format!("segment {k}: coordinate {value}, nearest whole-micrometre value {rounded}")));
            }
        }
    }
    let mut seen = vec![false; segments.len()];
    let mut loops = Vec::new();
    let mut signed = Vec::new();
    let mut boxes = Vec::new();
    for start in 0..segments.len() {
        if seen[start] { continue; }
        let mut contour = Vec::new();
        let mut cross = 0.0;
        let mut bounds = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        let mut k = start;
        while !seen[k] {
            seen[k] = true;
            let (a, b) = directed(&segments[k]);
            contour.push(a);
            cross += a[0] as f64 * b[1] as f64 - b[0] as f64 * a[1] as f64;
            for axis in 0..2 {
                bounds[axis] = bounds[axis].min(a[axis]);
                bounds[axis + 2] = bounds[axis + 2].max(a[axis]);
            }
            k = successors[k];
        }
        loops.push(contour);
        signed.push(cross);
        boxes.push(bounds);
    }
    let mut index = BoundsIndex { nodes: Vec::new() };
    if !loops.is_empty() {
        index.build(&mut (0..loops.len()).collect::<Vec<_>>(), &boxes);
    }
    for (i, contour) in loops.iter().enumerate() {
        let point = contour[0];
        let depth = index.depth(0, i, point, &loops);
        if (depth % 2 == 0 && signed[i] >= 0.0) || (depth % 2 == 1 && signed[i] <= 0.0) {
            return Err(Fault::new(Rule::Winding, format!("contour {i}: signed doubled area {}, nesting depth {depth}", signed[i])));
        }
    }
    let area = f32_at(bytes, 4) as f64;
    let expected = signed.iter().sum::<f64>().abs();
    if !near(area, expected, expected * 1e-3 + 1e-4) {
        return Err(Fault::new(Rule::VectorArea, format!("header doubled area {area}, edge area {expected}")));
    }
    let contours = u32_at(bytes, 28);
    if contours as usize != loops.len() || contours != scene.contours {
        return Err(Fault::new(Rule::VectorContours, format!("header {contours}, closed loops {}, scene {}", loops.len(), scene.contours)));
    }
    let bounds: [f32; 4] = std::array::from_fn(|i| f32_at(bytes, 8 + 4 * i));
    let mut expected = [0.0; 4];
    if !boxes.is_empty() {
        expected = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for b in boxes {
            for axis in 0..2 {
                expected[axis] = expected[axis].min(b[axis]);
                expected[axis + 2] = expected[axis + 2].max(b[axis + 2]);
            }
        }
    }
    if bounds != expected || bounds != scene.bounds {
        return Err(Fault::new(Rule::VectorBounds, format!("header box {bounds:?}, edges {expected:?}, scene {:?}", scene.bounds)));
    }
    Ok(segments)
}

fn directed(segment: &Segment) -> ([f32; 2], [f32; 2]) {
    if segment.swapped { (segment.to, segment.from) } else { (segment.from, segment.to) }
}
fn key(point: [f32; 2]) -> [u32; 2] {
    point.map(|x| if x == 0.0 { 0 } else { x.to_bits() })
}
fn contains_box(bounds: [f32; 4], point: [f32; 2]) -> bool {
    (0..2).all(|axis| point[axis] >= bounds[axis] && point[axis] <= bounds[axis + 2])
}
fn inside(contour: &[[f32; 2]], point: [f32; 2]) -> bool {
    let mut inside = false;
    for (a, b) in contour.iter().zip(contour.iter().cycle().skip(1)).take(contour.len()) {
        if (a[1] > point[1]) != (b[1] > point[1]) {
            let x = a[0] as f64 + (point[1] as f64 - a[1] as f64) * (b[0] as f64 - a[0] as f64) / (b[1] as f64 - a[1] as f64);
            if (point[0] as f64) < x { inside = !inside; }
        }
    }
    inside
}

struct BoundsNode {
    bounds: [f32; 4],
    children: Option<[usize; 2]>,
    contour: usize,
}
struct BoundsIndex { nodes: Vec<BoundsNode> }
impl BoundsIndex {
    fn build(&mut self, contours: &mut [usize], boxes: &[[f32; 4]]) -> usize {
        let mut bounds = boxes[contours[0]];
        for &i in contours.iter().skip(1) {
            for axis in 0..2 {
                bounds[axis] = bounds[axis].min(boxes[i][axis]);
                bounds[axis + 2] = bounds[axis + 2].max(boxes[i][axis + 2]);
            }
        }
        let node = self.nodes.len();
        self.nodes.push(BoundsNode { bounds, children: None, contour: contours[0] });
        if contours.len() > 1 {
            let axis = usize::from(bounds[3] as f64 - bounds[1] as f64 > bounds[2] as f64 - bounds[0] as f64);
            let middle = contours.len() / 2;
            contours.select_nth_unstable_by(middle, |a, b| {
                let centre = |i: usize| boxes[i][axis] as f64 + boxes[i][axis + 2] as f64;
                centre(*a).total_cmp(&centre(*b))
            });
            let (left, right) = contours.split_at_mut(middle);
            let children = [self.build(left, boxes), self.build(right, boxes)];
            self.nodes[node].children = Some(children);
        }
        node
    }
    fn depth(&self, node: usize, own: usize, point: [f32; 2], contours: &[Vec<[f32; 2]>]) -> usize {
        let node = &self.nodes[node];
        if !contains_box(node.bounds, point) { return 0; }
        if let Some([left, right]) = node.children {
            self.depth(left, own, point, contours) + self.depth(right, own, point, contours)
        } else {
            usize::from(node.contour != own && inside(&contours[node.contour], point))
        }
    }
}
