//! Pen and arrow marks in a finding's picture coordinates.
use makepad_town_core::{
    entity::{field, kind, Entity},
    event::{Id, Key, Value},
    fold::State,
    key::PublicKey,
};
use makepad_widgets::DVec2;
use std::{collections::HashSet, fmt::Write, sync::{Arc, LazyLock, Mutex}};

pub const UNIT: i32 = 65536;
pub const DEFAULT_WIDTH: i32 = 256;
pub const CHUNK_POINTS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Head { None, End }

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mark {
    pub id: Id,
    pub author: PublicKey,
    pub width: i32,
    pub head: Head,
    pub pts: Arc<[[i32; 2]]>,
}

/// Ramer-Douglas-Peucker in screen pixels. Use a work stack for long drags.
pub fn simplify(points: &[DVec2], epsilon: f64) -> Vec<DVec2> {
    if points.len() <= 2 { return points.to_vec() }
    let last = points.len() - 1;
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[last] = true;
    let mut work = vec![(0, last)];
    while let Some((start, end)) = work.pop() {
        let axis = points[end] - points[start];
        let length_sq = axis.x * axis.x + axis.y * axis.y;
        let mut max_sq = epsilon * epsilon;
        let mut split = None;
        for i in start + 1..end {
            let offset = points[i] - points[start];
            let t = if length_sq == 0. { 0. } else {
                ((offset.x * axis.x + offset.y * axis.y) / length_sq).clamp(0., 1.)
            };
            let delta = offset - axis * t;
            let dist_sq = delta.x * delta.x + delta.y * delta.y;
            if dist_sq > max_sq {
                max_sq = dist_sq;
                split = Some(i);
            }
        }
        if let Some(i) = split {
            keep[i] = true;
            work.extend([(start, i), (i, end)]);
        }
    }
    points.iter().zip(keep).filter_map(|(&point, keep)| keep.then_some(point)).collect()
}

pub fn encode_pts(pts: &[[i32; 2]]) -> String {
    let mut result = String::from("[");
    let mut previous = [0i32; 2];
    for (i, &[x, y]) in pts.iter().enumerate() {
        if i != 0 { result.push(' '); }
        write!(result, "{} {}", i64::from(x) - i64::from(previous[0]), i64::from(y) - i64::from(previous[1])).unwrap();
        previous = [x, y];
    }
    result.push(']');
    result
}

pub fn decode_pts(literal: &str) -> Option<Vec<[i32; 2]>> {
    let mut tokens = literal.trim().strip_prefix('[')?.strip_suffix(']')?.split_ascii_whitespace();
    let mut points = Vec::new();
    let mut point = [0i32; 2];
    while let Some(x) = tokens.next() {
        for (coord, delta) in point.iter_mut().zip([x, tokens.next()?]) {
            *coord = coord.checked_add(delta.parse::<i32>().ok()?)?;
            if !(-UNIT..=2 * UNIT).contains(coord) { return None }
        }
        points.push(point);
    }
    (points.len() >= 2).then_some(points)
}

fn read_mark(e: &Entity) -> Option<Mark> {
    let named = |name: &str| e.value(Key::Name(name.into()));
    let integer = |name, default| match named(name) {
        None => Some(default),
        Some(Value::Literal(s)) => s.trim().parse::<i32>().ok(),
        _ => None,
    };
    let width = integer("width", DEFAULT_WIDTH)?;
    if width <= 0 { return None }
    let head = match integer("head", 0)? { 0 => Head::None, 1 => Head::End, _ => return None };
    let Value::Literal(literal) = named("pts")? else { return None };
    Some(Mark { id: e.id, author: e.author, width, head, pts: decode_pts(literal)?.into() })
}

pub fn marks_on(state: &State, target: Id) -> Vec<Mark> {
    static REPORTED: LazyLock<Mutex<HashSet<Id>>> = LazyLock::new(Default::default);
    let mut marks = Vec::new();
    for e in state.entities.values().filter(|e| e.kind == kind::STROKE && !e.flag(field::DELETED)
        && e.value(field::PARENT) == Some(&Value::Id(target))) {
        if let Some(mark) = read_mark(e) {
            marks.push((e.created, mark));
        } else {
            let first = REPORTED.lock().unwrap().insert(e.id);
            if first { makepad_widgets::log!("Town skipped malformed mark {}", makepad_town_core::hex(&e.id)); }
        }
    }
    marks.sort_unstable_by_key(|(created, mark)| (*created, mark.id));
    marks.into_iter().map(|(_, mark)| mark).collect()
}

/// Each chunk starts at the previous chunk's end, so all segments survive.
pub fn chunks(pts: &[[i32; 2]]) -> Vec<Vec<[i32; 2]>> {
    (0..pts.len().saturating_sub(1)).step_by(CHUNK_POINTS - 1)
        .map(|start| pts[start..(start + CHUNK_POINTS).min(pts.len())].to_vec()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::dvec2;

    #[test]
    fn points_simplify_and_roundtrip() {
        let line = (0..200).map(|i| dvec2(i as f64, -2. * i as f64)).collect::<Vec<_>>();
        assert_eq!(simplify(&line, 0.75), [line[0], line[199]], "a line keeps only its ends");
        let elbow = (0..100).map(|i: i32| dvec2(i.min(49) as f64, (i - 49).max(0) as f64)).collect::<Vec<_>>();
        assert_eq!(simplify(&elbow, 0.75), [elbow[0], elbow[49], elbow[99]], "the right angle survives");
        let pts = [[12, UNIT], [8, 13], [-UNIT, 2 * UNIT]];
        let literal = encode_pts(&pts);
        assert!(Key::Name("pts".into()).accepts(&Value::Literal(literal.clone())), "the fold accepts the stored literal");
        assert_eq!(decode_pts(&literal).as_deref(), Some(pts.as_slice()), "signed deltas round-trip");
        for invalid in ["[1 2 3]", "[1 2]", "[1 2 3.5 4]", "[131072 0 1 0]", "[1 0 2147483647 0]"] {
            assert_eq!(decode_pts(invalid), None, "invalid or overflowing mark {invalid}");
        }
        let long: Vec<_> = (0..1100).map(|i| [i, i]).collect();
        let parts = chunks(&long);
        assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [512, 512, 78]);
        assert!(parts.windows(2).all(|pair| pair[0].last() == pair[1].first()), "chunks share their ends");
        assert!(parts.iter().flat_map(|part| part.windows(2)).eq(long.windows(2)), "every segment survives in order");
    }
}
