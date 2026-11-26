//! Deterministic layout in canvas units, with the origin at the top left.
//! Accepted items retain input order in every result. IDs are local to each
//! input list. The first valid occurrence owns an ID.

#![forbid(unsafe_code)]

mod layered;
mod order;
mod sequence;

pub use layered::layered;
pub use sequence::{sequence, Message, Participant, SequencePlaced, PARTICIPANT_HEIGHT};

use std::collections::{BTreeMap, BTreeSet};

pub type Point = (f64, f64);
pub type Polyline = Vec<Point>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub id: u64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub id: u64,
    pub from: u64,
    pub to: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Down,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub direction: Direction,
    pub node_gap: f64,
    pub layer_gap: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self { direction: Direction::Down, node_gap: 24.0, layer_gap: 48.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Node,
    Edge,
    Participant,
    Message,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    DuplicateId,
    /// The first missing endpoint in source-to-target order.
    MissingEndpoint(u64),
    InvalidSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub kind: ItemKind,
    /// Zero-based position in the corresponding input slice.
    pub index: usize,
    pub id: u64,
    pub reason: SkipReason,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Placed {
    /// ID and top-left corner of each box.
    pub nodes: Vec<(u64, f64, f64)>,
    /// Polylines always start at the original source and end at the target.
    pub edges: Vec<(u64, Polyline)>,
    /// IDs of feedback edges, in input order. Self-loops are excluded.
    pub reversed: Vec<u64>,
    pub skipped: Vec<Skipped>,
}

fn extent_valid(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

// Invalid gaps collapse to zero. Boxes must have positive finite extents.
fn gap(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn index_boxes<'a, T>(
    input: &'a [T],
    kind: ItemKind,
    describe: impl Fn(&T) -> (u64, bool),
    skipped: &mut Vec<Skipped>,
) -> (Vec<&'a T>, BTreeMap<u64, usize>) {
    let mut boxes = Vec::new();
    let mut indices = BTreeMap::new();
    for (index, item) in input.iter().enumerate() {
        let (id, valid) = describe(item);
        let reason = if indices.contains_key(&id) {
            SkipReason::DuplicateId
        } else if !valid {
            SkipReason::InvalidSize
        } else {
            indices.insert(id, boxes.len());
            boxes.push(item);
            continue;
        };
        skipped.push(Skipped { kind, index, id, reason });
    }
    (boxes, indices)
}

#[derive(Clone, Copy)]
struct Link {
    index: usize,
    from: usize,
    to: usize,
}

fn index_links<T>(
    input: &[T],
    kind: ItemKind,
    describe: impl Fn(&T) -> (u64, u64, u64, bool),
    boxes: &BTreeMap<u64, usize>,
    skipped: &mut Vec<Skipped>,
) -> Vec<Link> {
    let mut ids = BTreeSet::new();
    let mut links = Vec::new();
    for (index, item) in input.iter().enumerate() {
        let (id, from, to, valid) = describe(item);
        let reason = if ids.contains(&id) {
            SkipReason::DuplicateId
        } else if !valid {
            SkipReason::InvalidSize
        } else {
            match (boxes.get(&from), boxes.get(&to)) {
                (Some(&from), Some(&to)) => {
                    ids.insert(id);
                    links.push(Link { index, from, to });
                    continue;
                }
                (None, _) => SkipReason::MissingEndpoint(from),
                (_, None) => SkipReason::MissingEndpoint(to),
            }
        };
        skipped.push(Skipped { kind, index, id, reason });
    }
    links
}

#[cfg(test)]
mod tests;
