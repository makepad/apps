//! Deterministic orthogonal routing in the vertical strips between columns.
//!
//! Coordinates are integer grid steps, x right and y up. Pairwise crossing
//! costs choose track-order preferences. The weakest preferences participating
//! in cycles are removed, then a stable topological order drives first-fit
//! interval packing. Straight pairs and lone-terminal stubs need no track.
//!
//! Cycle breaking is greedy, not a global crossing minimum. For G tracked
//! groups, E overlapping pairs and P terminals in a channel, preference costs
//! take O(G * P), cycle removal takes O(E * (G + E)) in the worst case, and
//! packing takes O(G^2 + E). Acyclic preferences bypass cycle removal. The
//! final crossing count takes O(H * V) for H horizontal and V vertical pieces.

use crate::graph::Pt;
use std::collections::{BTreeMap, BTreeSet};

/// Which way a terminal's wire leaves its pin into a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// From the column on the left, toward positive x.
    East,
    /// From the column on the right, toward negative x.
    West,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Term {
    pub at: Pt,
    pub face: Face,
}

/// One net's terminals that meet in one channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub net: u32,
    /// Index into the channels slice.
    pub channel: u32,
    /// At least one terminal, in output order.
    pub terms: Vec<Term>,
    /// Importance, used only to break equal-cost cycle-removal ties.
    pub weight: u32,
}

/// A free vertical strip, exclusive of both edges: x0 < x < x1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Channel {
    pub x0: i32,
    pub x1: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ROpts {
    /// Smallest distance between tracks; must be positive.
    pub spacing: i32,
    /// Outward length of a lone terminal's stub; must be nonnegative.
    pub stub: i32,
}

impl Default for ROpts {
    fn default() -> Self {
        Self { spacing: 1, stub: 2 }
    }
}

/// One orthogonal polyline; consecutive points are distinct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub net: u32,
    pub group: u32,
    pub pts: Vec<Pt>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Routed {
    /// Group order, then terminal order, then the group's vertical span.
    pub wires: Vec<Wire>,
    /// Unique (net, point) junctions, sorted by net and point.
    pub junctions: Vec<(u32, Pt)>,
    /// Track x per group, or i32::MIN for straight pairs and lone terminals.
    pub tracks: Vec<i32>,
    /// Proper crossings between different nets' output segments.
    pub crossings: u32,
    /// Channels packed at unit spacing because requested spacing did not fit.
    pub overflow: Vec<u32>,
}

/// Routes each group independently of other channels, preserving input order.
///
/// Groups must be nonempty and name existing channels, channel edges must be
/// increasing, spacing must be positive, and stub length must be nonnegative.
/// Group and channel indices must fit u32. Invalid input panics.
///
/// Overflow never discards a route: slots are packed from x0 + 1 at unit
/// spacing. If even that cannot fit, tracks extend past the strip's right
/// edge (saturating at i32::MAX). A strip with no interior grid point cannot
/// hold a stub, so that stub has zero length and emits no piece. Crossing
/// counts exceeding u32::MAX saturate.
pub fn route(channels: &[Channel], groups: &[Group], o: &ROpts) -> Routed {
    assert!(o.spacing > 0, "track spacing must be positive");
    assert!(o.stub >= 0, "stub length must be nonnegative");
    assert!(groups.len() <= u32::MAX as usize, "group indices must fit u32");
    assert!(channels.len() <= u32::MAX as usize, "channel indices must fit u32");
    for channel in channels {
        assert!(channel.x0 < channel.x1, "channel edges must be increasing");
    }

    let mut spans = Vec::with_capacity(groups.len());
    let mut members = vec![Vec::new(); channels.len()];
    for (index, group) in groups.iter().enumerate() {
        assert!(!group.terms.is_empty(), "a group must contain a terminal");
        assert!(
            (group.channel as usize) < channels.len(),
            "a group's channel must exist"
        );
        let y = group.terms[0].at[1];
        let span = group.terms.iter().fold([y, y], |span, term| {
            [span[0].min(term.at[1]), span[1].max(term.at[1])]
        });
        spans.push(span);
        if group.terms.len() > 1 && !straight(group) {
            members[group.channel as usize].push(index);
        }
    }

    let mut result = Routed {
        wires: Vec::new(),
        junctions: Vec::new(),
        tracks: vec![i32::MIN; groups.len()],
        crossings: 0,
        overflow: Vec::new(),
    };
    for (channel_index, channel_members) in members.iter().enumerate() {
        if channel_members.is_empty() {
            continue;
        }
        let (slots, count) = pack(channel_members, groups, &spans);
        let (positions, overflow) = track_positions(channels[channel_index], count, o.spacing);
        if overflow {
            result.overflow.push(channel_index as u32);
        }
        for (local, &group) in channel_members.iter().enumerate() {
            result.tracks[group] = positions[slots[local]];
        }
    }

    let mut junctions = BTreeSet::new();
    for (index, group) in groups.iter().enumerate() {
        if group.terms.len() == 1 {
            let term = group.terms[0];
            let channel = channels[group.channel as usize];
            let x = stub_end(term, channel, o.stub);
            emit(&mut result.wires, index, group.net, vec![term.at, [x, term.at[1]]]);
        } else if straight(group) {
            emit(
                &mut result.wires,
                index,
                group.net,
                vec![group.terms[0].at, group.terms[1].at],
            );
        } else {
            let x = result.tracks[index];
            if group.terms.len() == 2 {
                let a = group.terms[0].at;
                let b = group.terms[1].at;
                emit(&mut result.wires, index, group.net, vec![a, [x, a[1]], [x, b[1]], b]);
            } else {
                for term in &group.terms {
                    emit(
                        &mut result.wires,
                        index,
                        group.net,
                        vec![term.at, [x, term.at[1]]],
                    );
                }
                emit(
                    &mut result.wires,
                    index,
                    group.net,
                    vec![[x, spans[index][0]], [x, spans[index][1]]],
                );
            }
            let [lo, hi] = spans[index];
            if lo < hi {
                let mut arrivals = BTreeMap::<i32, usize>::new();
                for term in &group.terms {
                    if term.at[0] != x {
                        *arrivals.entry(term.at[1]).or_default() += 1;
                    }
                }
                for (y, count) in arrivals {
                    if (lo < y && y < hi) || count >= 2 {
                        junctions.insert((group.net, [x, y]));
                    }
                }
            }
        }
    }
    result.junctions = junctions.into_iter().collect();
    result.crossings = count_crossings(&result.wires);
    result
}

fn straight(group: &Group) -> bool {
    group.terms.len() == 2
        && group.terms[0].face != group.terms[1].face
        && group.terms[0].at[1] == group.terms[1].at[1]
}

fn overlaps(a: [i32; 2], b: [i32; 2]) -> bool {
    // Closed spans: touching endpoints conflict; adjacent grid steps do not.
    a[0] <= b[1] && b[0] <= a[1]
}

fn inside(y: i32, span: [i32; 2]) -> bool {
    span[0] < y && y < span[1]
}

/// The weight of a terminal on a row at the end of another group's span. That end is the other
/// group's own terminal on the far side of the channel, so in this order their two horizontals lie
/// on one line between the tracks: two nets drawn as one, worse than any number of crossings.
const SHARED_ROW: u64 = 1 << 20;

/// What putting `left`'s track left of `right`'s costs: a crossing for each of `right`'s
/// west-side terminals within `left`'s span, and for each of `left`'s east-side terminals within
/// `right`'s; `SHARED_ROW` for one on a row that ends the other's span.
fn cost(left: &Group, left_span: [i32; 2], right: &Group, right_span: [i32; 2]) -> u64 {
    let weight = |y: i32, span: [i32; 2]| if inside(y, span) { 1 } else if y == span[0] || y == span[1] { SHARED_ROW } else { 0 };
    right.terms.iter().filter(|term| term.face == Face::East).map(|term| weight(term.at[1], left_span)).sum::<u64>()
        + left.terms.iter().filter(|term| term.face == Face::West).map(|term| weight(term.at[1], right_span)).sum::<u64>()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Preference {
    from: usize,
    to: usize,
    penalty: u64,
    active: bool,
}

fn pack(members: &[usize], groups: &[Group], spans: &[[i32; 2]]) -> (Vec<usize>, usize) {
    let mut edges = Vec::new();
    let mut outgoing = vec![Vec::new(); members.len()];
    let mut incoming = vec![Vec::new(); members.len()];
    for a in 0..members.len() {
        for b in a + 1..members.len() {
            let ga = members[a];
            let gb = members[b];
            if !overlaps(spans[ga], spans[gb]) {
                continue;
            }
            let ab = cost(&groups[ga], spans[ga], &groups[gb], spans[gb]);
            let ba = cost(&groups[gb], spans[gb], &groups[ga], spans[ga]);
            // Members are in input order, so equal costs put the lower index left.
            let (from, to) = if ab <= ba { (a, b) } else { (b, a) };
            let index = edges.len();
            edges.push(Preference { from, to, penalty: ab.abs_diff(ba), active: true });
            outgoing[from].push(index);
            incoming[to].push(index);
        }
    }

    let mut order = topological(&edges, &outgoing);
    if order.len() != members.len() {
        let mut weakest: Vec<_> = (0..edges.len()).collect();
        weakest.sort_by_key(|&index| {
            let edge = edges[index];
            let a = members[edge.from];
            let b = members[edge.to];
            (
                edge.penalty,
                u64::from(groups[a].weight) + u64::from(groups[b].weight),
                groups[a].net,
                groups[b].net,
                a,
                b,
            )
        });
        let mut seen = vec![false; members.len()];
        let mut pending = Vec::new();
        for index in weakest {
            let edge = edges[index];
            // An edge participates in a cycle exactly when its target can
            // reach its source. Removing edges cannot create a new cycle, so
            // one pass from lightest to heaviest suffices. Equal penalties
            // sacrifice lower total importance, then net ids and indices.
            seen.fill(false);
            pending.clear();
            pending.push(edge.to);
            seen[edge.to] = true;
            while let Some(node) = pending.pop() {
                if node == edge.from {
                    edges[index].active = false;
                    break;
                }
                for &next in &outgoing[node] {
                    let next = edges[next];
                    if next.active && !seen[next.to] {
                        seen[next.to] = true;
                        pending.push(next.to);
                    }
                }
            }
        }
        order = topological(&edges, &outgoing);
    }

    let mut slots = vec![0; members.len()];
    let mut occupants: Vec<Vec<usize>> = Vec::new();
    for node in order {
        let first = incoming[node].iter().filter_map(|&index| {
            let edge = edges[index];
            edge.active.then_some(slots[edge.from] + 1)
        }).max().unwrap_or(0);
        let mut slot = first;
        while slot < occupants.len()
            && occupants[slot].iter().any(|&other| {
                overlaps(spans[members[node]], spans[members[other]])
            })
        {
            slot += 1;
        }
        if slot == occupants.len() {
            occupants.push(Vec::new());
        }
        occupants[slot].push(node);
        slots[node] = slot;
    }
    (slots, occupants.len())
}

fn topological(edges: &[Preference], outgoing: &[Vec<usize>]) -> Vec<usize> {
    let mut degrees = vec![0usize; outgoing.len()];
    for edge in edges.iter().filter(|edge| edge.active) {
        degrees[edge.to] += 1;
    }
    let mut ready = BTreeSet::new();
    for (node, &degree) in degrees.iter().enumerate() {
        if degree == 0 {
            ready.insert(node);
        }
    }
    let mut order = Vec::with_capacity(outgoing.len());
    while let Some(&node) = ready.iter().next() {
        ready.remove(&node);
        order.push(node);
        for &index in &outgoing[node] {
            let edge = edges[index];
            if edge.active {
                degrees[edge.to] -= 1;
                if degrees[edge.to] == 0 {
                    ready.insert(edge.to);
                }
            }
        }
    }
    order
}

fn track_positions(channel: Channel, count: usize, spacing: i32) -> (Vec<i32>, bool) {
    let x0 = i64::from(channel.x0);
    let width = i64::from(channel.x1) - x0;
    let total = count as i64;
    let spacing = i64::from(spacing);
    let overflow = 2 + (total - 1) * spacing > width;
    let mut positions = Vec::with_capacity(count);
    let mut next = x0 + 1;
    for slot in 0..total {
        let x = if overflow {
            x0 + slot + 1
        } else {
            // Positive integer rounding, with exact halves rounded upward.
            // Wide arithmetic also handles channels spanning all of i32.
            let denominator = i128::from(total + 1);
            let offset = (i128::from(slot + 1) * i128::from(width) + denominator / 2)
                / denominator;
            let ideal = x0 + offset as i64;
            // Reserve room for all remaining slots before choosing this x.
            let last = i64::from(channel.x1) - 1 - (total - slot - 1) * spacing;
            ideal.clamp(next, last)
        };
        positions.push(x.min(i64::from(i32::MAX)) as i32);
        next = x + spacing;
    }
    (positions, overflow)
}

fn stub_end(term: Term, channel: Channel, length: i32) -> i32 {
    let x = i64::from(term.at[0]);
    if i64::from(channel.x1) - i64::from(channel.x0) <= 1 {
        return term.at[0];
    }
    let end = match term.face {
        Face::East => (x + i64::from(length)).min(i64::from(channel.x1) - 1).max(x),
        Face::West => (x - i64::from(length)).max(i64::from(channel.x0) + 1).min(x),
    };
    end as i32
}

fn emit(wires: &mut Vec<Wire>, group: usize, net: u32, mut pts: Vec<Pt>) {
    pts.dedup();
    if pts.len() > 1 {
        wires.push(Wire { net, group: group as u32, pts });
    }
}

fn count_crossings(wires: &[Wire]) -> u32 {
    let mut horizontals = Vec::new();
    let mut verticals = Vec::new();
    for wire in wires {
        for pair in wire.pts.windows(2) {
            let a = pair[0];
            let b = pair[1];
            if a[1] == b[1] {
                horizontals.push((wire.net, a[1], a[0].min(b[0]), a[0].max(b[0])));
            } else {
                verticals.push((wire.net, a[0], a[1].min(b[1]), a[1].max(b[1])));
            }
        }
    }
    let mut count = 0u32;
    for &(hnet, y, x0, x1) in &horizontals {
        for &(vnet, x, y0, y1) in &verticals {
            if hnet != vnet && x0 < x && x < x1 && y0 < y && y < y1 {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pin_nets_are_straight_or_take_one_bend() {
        let channels = [Channel { x0: 0, x1: 10 }, Channel { x0: 20, x1: 22 }];
        let groups = [
            Group {
                net: 10, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [0, 0], face: Face::East },
                    Term { at: [10, 0], face: Face::West },
                ],
            },
            Group {
                net: 20, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [10, 5], face: Face::West },
                    Term { at: [0, 2], face: Face::East },
                ],
            },
            Group {
                net: 30, channel: 0, weight: 1,
                terms: vec![Term { at: [0, 8], face: Face::East }],
            },
            Group {
                net: 40, channel: 1, weight: 1,
                terms: vec![Term { at: [22, 9], face: Face::West }],
            },
        ];
        let routed = route(&channels, &groups, &ROpts::default());
        assert_eq!(routed.wires.len(), 4, "each pair or lone terminal emits one wire");
        assert_eq!(routed.wires[0].pts, vec![[0, 0], [10, 0]], "aligned pins route straight");
        assert_eq!(routed.tracks[0], i32::MIN, "an aligned pair needs no track");
        let x = routed.tracks[1];
        assert!(0 < x && x < 10, "the bent pair's track lies strictly inside its channel");
        assert_eq!(
            routed.wires[1].pts,
            vec![[10, 5], [x, 5], [x, 2], [0, 2]],
            "a bent pair emits one four-point polyline in terminal order"
        );
        assert_eq!(routed.wires[2].pts, vec![[0, 8], [2, 8]], "an East stub extends outward");
        assert_eq!(
            routed.wires[3].pts,
            vec![[22, 9], [21, 9]],
            "a West stub extends outward and clamps to the open strip"
        );
        assert_eq!(&routed.tracks[2..], &[i32::MIN; 2], "lone terminals need no tracks");
        assert!(routed.junctions.is_empty(), "plain corners and stubs are not junctions");
        assert_eq!(routed.crossings, 0, "disjoint wires have no crossings");
        assert!(routed.overflow.is_empty(), "a fitting track does not report overflow");
        assert_eq!(
            routed.wires.iter().map(|wire| (wire.group, wire.net)).collect::<Vec<_>>(),
            vec![(0, 10), (1, 20), (2, 30), (3, 40)],
            "wires preserve group order and caller net ids"
        );
    }

    #[test]
    fn overlapping_groups_take_ordered_tracks_without_crossings() {
        let channels = [Channel { x0: 0, x1: 10 }];
        let groups = [
            Group {
                net: 1, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [0, 0], face: Face::East },
                    Term { at: [10, 6], face: Face::West },
                ],
            },
            Group {
                net: 2, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [0, 4], face: Face::East },
                    Term { at: [10, 10], face: Face::West },
                ],
            },
            Group {
                net: 3, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [0, 11], face: Face::East },
                    Term { at: [10, 13], face: Face::West },
                ],
            },
        ];
        let options = ROpts { spacing: 7, ..ROpts::default() };
        let routed = route(&channels, &groups, &options);
        assert_eq!(routed.crossings, 0, "the cheaper pair order avoids both possible crossings");
        assert_ne!(routed.tracks[0], routed.tracks[1], "overlapping groups use distinct tracks");
        assert!(routed.tracks[1] < routed.tracks[0], "crossing cost overrides input order");
        assert_eq!(
            routed.tracks[2], routed.tracks[1],
            "a group separated by one grid step reuses the leftmost track"
        );
        assert!(
            routed.tracks[0] - routed.tracks[1] >= options.spacing,
            "track positions enforce spacing even when uniform rounding is too tight"
        );
        assert!(
            routed.tracks.iter().all(|&x| 0 < x && x < 10),
            "spacing adjustment keeps tracks strictly inside the channel"
        );
        assert!(routed.overflow.is_empty(), "a feasible spacing must not report overflow");
        assert_eq!(routed.wires.len(), 3, "packing preserves one polyline per two-pin group");
    }

    #[test]
    fn a_hyperedge_gets_junctions_and_output_is_deterministic() {
        let channels = [Channel { x0: 0, x1: 12 }, Channel { x0: 20, x1: 24 }];
        let groups = [
            Group {
                net: 7, channel: 0, weight: 4,
                terms: vec![
                    Term { at: [0, 0], face: Face::East },
                    Term { at: [0, 3], face: Face::East },
                    Term { at: [12, 7], face: Face::West },
                    Term { at: [12, 10], face: Face::West },
                ],
            },
            Group {
                net: 8, channel: 0, weight: 1,
                terms: vec![
                    Term { at: [0, 11], face: Face::East },
                    Term { at: [12, 11], face: Face::West },
                    Term { at: [0, 15], face: Face::East },
                    Term { at: [12, 19], face: Face::West },
                ],
            },
            Group {
                net: 9, channel: 1, weight: 1,
                terms: vec![
                    Term { at: [20, 0], face: Face::East },
                    Term { at: [24, 6], face: Face::West },
                ],
            },
            Group {
                net: 10, channel: 1, weight: 1,
                terms: vec![
                    Term { at: [20, 4], face: Face::East },
                    Term { at: [24, 10], face: Face::West },
                ],
            },
        ];
        let options = ROpts { spacing: 3, ..ROpts::default() };
        let routed = route(&channels, &groups, &options);
        let x = routed.tracks[0];
        assert_eq!(
            routed.junctions,
            vec![(7, [x, 3]), (7, [x, 7]), (8, [x, 11]), (8, [x, 15])],
            "interior arrivals and two arrivals at a span end are the only junctions"
        );
        assert_eq!(
            routed.wires.iter().filter(|wire| wire.group == 0).count(), 5,
            "a four-terminal hyperedge emits four horizontals and one vertical"
        );
        assert_eq!(routed.wires[4].pts, vec![[x, 0], [x, 10]], "one track spans the hyperedge");
        for (index, term) in groups[0].terms.iter().enumerate() {
            assert_eq!(
                routed.wires[index].pts, vec![term.at, [x, term.at[1]]],
                "hyperedge horizontals retain terminal order and meet the same track"
            );
        }
        assert_eq!(routed.tracks[1], x, "disjoint hyperedges can share a track");
        assert_eq!(routed.overflow, vec![1], "only the channel too narrow for spacing overflows");
        assert_eq!(&routed.tracks[2..], &[22, 21], "overflow packs ordered tracks at unit spacing");
        assert_eq!(routed.wires.len(), 12, "overflow preserves every group's wire pieces");
        for index in 0..groups.len() {
            assert!(
                routed.wires.iter().any(|wire| wire.group == index as u32),
                "every input group retains a route, including in an overflowing channel"
            );
        }
        assert_eq!(routed.crossings, 0, "junctions of one net are not crossings");
        assert_eq!(routed, route(&channels, &groups, &options), "routing is deterministic");
    }
}
