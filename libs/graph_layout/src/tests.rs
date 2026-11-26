use super::*;

fn node(id: u64, width: f64, height: f64) -> Node {
    Node { id, width, height }
}

fn edges(pairs: &[(u64, u64)]) -> Vec<Edge> {
    pairs.iter().enumerate().map(|(i, &(from, to))| Edge { id: i as u64, from, to }).collect()
}

fn options(direction: Direction) -> Options {
    Options { direction, node_gap: 9.0, layer_gap: 14.0 }
}

#[test]
fn chain_is_straight_in_either_direction() {
    let nodes = [node(8, 24.0, 32.0), node(3, 50.0, 16.0), node(1, 13.0, 27.0)];
    let links = edges(&[(8, 3), (3, 1)]);
    for direction in [Direction::Down, Direction::Right] {
        let o = options(direction);
        let p = layered(&nodes, &links, &o);
        for (i, (_, path)) in p.edges.iter().enumerate() {
            let (_, x, y) = p.nodes[i];
            let (_, tx, ty) = p.nodes[i + 1];
            let (a, b) = (nodes[i], nodes[i + 1]);
            let expected = match direction {
                Direction::Down => {
                    assert_eq!(y + a.height + o.layer_gap, ty, "chain rows preserve the layer gap");
                    assert_eq!(x + a.width / 2.0, tx + b.width / 2.0, "chain centres share a column");
                    [(x + a.width / 2.0, y + a.height), (tx + b.width / 2.0, ty)]
                }
                Direction::Right => {
                    assert_eq!(x + a.width + o.layer_gap, tx, "chain columns preserve the layer gap");
                    assert_eq!(y + a.height / 2.0, ty + b.height / 2.0, "chain centres share a row");
                    [(x + a.width, y + a.height / 2.0), (tx, ty + b.height / 2.0)]
                }
            };
            assert_eq!(path.as_slice(), expected, "a straight edge joins its two facing sides");
        }
    }
}

#[test]
fn diamond_centres_its_fork_and_join() {
    let nodes = [node(9, 15.0, 12.0), node(7, 30.0, 14.0), node(2, 18.0, 14.0), node(1, 9.0, 12.0)];
    let p = layered(&nodes, &edges(&[(9, 7), (9, 2), (7, 1), (2, 1)]), &options(Direction::Down));
    let x = |i: usize| p.nodes[i].1 + nodes[i].width * 0.5;
    let middle = (x(1) + x(2)) * 0.5;
    assert_eq!(p.nodes[1].2, p.nodes[2].2, "branches occupy the same layer");
    assert!(
        p.nodes[2].1 >= p.nodes[1].1 + nodes[1].width + 9.0,
        "tied branches keep input order and node gap"
    );
    assert!(
        p.nodes[0].2 < p.nodes[1].2 && p.nodes[1].2 < p.nodes[3].2,
        "fork, branches and join occur in order"
    );
    assert_eq!((x(0), x(3)), (middle, middle), "fork and join centre on their neighbours");
}

struct Seed(u64);
impl Seed {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 32) % bound
    }
}

#[test]
fn seeded_graph_boxes_are_disjoint() {
    let mut rng = Seed(1907);
    for case in 0..256 {
        let n = 3 + rng.next(7);
        let nodes: Vec<_> =
            (0..n).map(|id| node(id, 3.0 + rng.next(35) as f64, 2.0 + rng.next(27) as f64)).collect();
        let links: Vec<_> =
            (0..rng.next(2 * n + 1)).map(|id| Edge { id, from: rng.next(n), to: rng.next(n) }).collect();
        let direction = if case % 2 == 0 { Direction::Right } else { Direction::Down };
        let p = layered(&nodes, &links, &options(direction));
        assert_eq!(p.nodes.len(), nodes.len(), "every input box is present in case {case}");
        for (i, &(_, x, y)) in p.nodes.iter().enumerate() {
            for (j, &(_, tx, ty)) in p.nodes.iter().enumerate().take(i) {
                assert!(
                    x + nodes[i].width <= tx
                        || tx + nodes[j].width <= x
                        || y + nodes[i].height <= ty
                        || ty + nodes[j].height <= y,
                    "boxes {j} and {i} overlap in case {case}"
                );
            }
        }
    }
}

fn side(a: Point, b: Point, p: Point) -> f64 {
    (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
}

#[test]
fn interleaved_tree_has_no_crossings() {
    let nodes: Vec<_> = (0..9).map(|id| node(id, 12.0, 12.0)).collect();
    let p = layered(
        &nodes,
        &edges(&[(0, 1), (0, 2), (1, 3), (2, 4), (1, 5), (2, 6), (1, 7), (2, 8)]),
        &options(Direction::Down),
    );
    for (i, (_, a)) in p.edges.iter().enumerate() {
        for (_, b) in &p.edges[..i] {
            for s in a.windows(2) {
                for t in b.windows(2) {
                    let crosses = side(s[0], s[1], t[0]) * side(s[0], s[1], t[1]) < 0.0
                        && side(t[0], t[1], s[0]) * side(t[0], t[1], s[1]) < 0.0;
                    assert!(!crosses, "two routed branches of a tree cross");
                }
            }
        }
    }
}

#[test]
fn three_cycle_reverses_one_edge_and_preserves_its_arrow() {
    let nodes: Vec<_> = (0..3).map(|id| node(id, 20.0, 20.0)).collect();
    let p = layered(&nodes, &edges(&[(0, 1), (1, 2), (2, 0)]), &options(Direction::Down));
    assert_eq!(p.reversed, [2], "one feedback edge breaks the three-cycle");
    let back = &p.edges[2].1;
    assert_eq!(back[0].1, p.nodes[2].2, "feedback arrow leaves the original source on its top side");
    assert_eq!(
        back.last().unwrap().1,
        p.nodes[0].2 + 20.0,
        "feedback arrow enters the original target on its bottom side"
    );
}

#[test]
fn rebuilding_same_order_preserves_exact_bits() {
    let build = || {
        let nodes: Vec<_> = [8, 4, 9, 1, 3].into_iter().map(|id| node(id, 7.5, 15.25)).collect();
        let links = edges(&[(8, 4), (8, 9), (9, 1), (4, 1), (1, 8), (8, 4), (3, 3)]);
        (nodes, links)
    };
    let signature = |p: Placed| {
        let nodes: Vec<_> = p.nodes.iter().map(|&(id, x, y)| (id, x.to_bits(), y.to_bits())).collect();
        let paths: Vec<_> = p
            .edges
            .iter()
            .map(|(id, path)| {
                (*id, path.iter().map(|&(x, y)| (x.to_bits(), y.to_bits())).collect::<Vec<_>>())
            })
            .collect();
        (nodes, paths, p.reversed, p.skipped)
    };
    let (nodes, links) = build();
    let o = options(Direction::Down);
    let first = signature(layered(&nodes, &links, &o));
    assert_eq!(
        first,
        signature(layered(&nodes, &links, &o)),
        "repeat placement preserves all coordinate bits and IDs"
    );
    let (other_nodes, other_links) = build();
    assert_eq!(
        first,
        signature(layered(&other_nodes, &other_links, &o)),
        "freshly constructed input preserves all coordinate bits and IDs"
    );
}

#[test]
fn malformed_items_are_skipped_and_identified() {
    let nodes = [node(5, 10.0, 9.0), node(5, 99.0, 50.0), node(6, f64::NAN, 5.0), node(6, 8.0, 15.0)];
    let links = [
        Edge { id: 8, from: 4, to: 6 },
        Edge { id: 9, from: 5, to: 6 },
        Edge { id: 9, from: 6, to: 5 },
        Edge { id: 7, from: 5, to: 0 },
    ];
    let p = layered(&nodes, &links, &options(Direction::Down));
    assert_eq!(p.nodes.len(), 2, "only valid distinct boxes are placed");
    assert_eq!(
        p.edges.iter().map(|e| e.0).collect::<Vec<_>>(),
        [9],
        "only the first valid distinct edge is routed"
    );
    assert_eq!(
        p.skipped,
        [
            Skipped { kind: ItemKind::Node, index: 1, id: 5, reason: SkipReason::DuplicateId },
            Skipped { kind: ItemKind::Node, index: 2, id: 6, reason: SkipReason::InvalidSize },
            Skipped { kind: ItemKind::Edge, index: 0, id: 8, reason: SkipReason::MissingEndpoint(4) },
            Skipped { kind: ItemKind::Edge, index: 2, id: 9, reason: SkipReason::DuplicateId },
            Skipped { kind: ItemKind::Edge, index: 3, id: 7, reason: SkipReason::MissingEndpoint(0) },
        ],
        "every skipped input reports its list, index, ID and reason"
    );
    assert_eq!(p.edges[0].1[0].1, 9.0, "a duplicate leaves the first valid node's dimensions intact");
}

#[test]
fn free_source_moves_next_to_its_successor() {
    let nodes: Vec<_> = (0..5).map(|id| node(id, 12.0, 12.0)).collect();
    let p = layered(&nodes, &edges(&[(0, 1), (1, 2), (2, 3), (4, 3)]), &options(Direction::Down));
    assert_eq!(p.nodes[4].2, p.nodes[2].2, "span compaction moves the free source to the last feasible row");
}

#[test]
fn long_edges_clear_intervening_boxes() {
    let nodes = [node(0, 15.0, 8.0), node(1, 60.0, 85.0), node(2, 80.0, 60.0), node(3, 12.0, 10.0)];
    let p = layered(&nodes, &edges(&[(0, 1), (1, 2), (2, 3), (0, 3)]), &options(Direction::Down));
    let path = &p.edges[3].1;
    for i in [1, 2] {
        let (_, left, top) = p.nodes[i];
        let bottom = top + nodes[i].height;
        let right = left + nodes[i].width;
        assert!(
            path.windows(2).any(|s| s[0].1 <= top
                && s[1].1 >= bottom
                && s[0].0 == s[1].0
                && (s[0].0 < left || s[0].0 > right)),
            "long edge must traverse the full depth of box {i} in a clear track"
        );
    }
}

#[test]
fn parallel_edges_and_loops_have_separate_tracks() {
    let nodes = [node(0, 30.0, 20.0), node(1, 30.0, 20.0), node(2, 8.0, 8.0)];
    let links = edges(&[(0, 1), (0, 1), (0, 0), (0, 0)]);
    for direction in [Direction::Down, Direction::Right] {
        let o = options(direction);
        let p = layered(&nodes, &links, &o);
        assert_ne!(p.edges[0].1.first(), p.edges[1].1.first(), "parallel sources occupy distinct ports");
        assert_ne!(p.edges[0].1.last(), p.edges[1].1.last(), "parallel targets occupy distinct ports");
        assert_ne!(p.edges[2].1[1], p.edges[3].1[1], "each self-loop has a distinct turn");
        assert!(
            p.edges[2].1.len() == 4 && p.edges[2].1[0] != p.edges[2].1[3],
            "a self-loop leaves and returns at distinct ports"
        );
        assert!(p.reversed.is_empty(), "self-loops do not create feedback edges");
        let far_edge = p.edges.iter().flat_map(|(_, path)| path).map(|q| q.0).fold(0.0, f64::max);
        assert!(
            p.nodes[2].1 >= far_edge + o.node_gap,
            "the next component clears every wire of the previous one"
        );
    }
}

#[test]
fn sequence_reserves_message_rows_and_extends_lifelines() {
    let participants = [Participant { id: 8, width: 25.0 }, Participant { id: 1, width: 10.0 }];
    let message = |id, from, to, height| Message { id, from, to, height };
    let messages = [message(5, 8, 1, 10.0), message(3, 1, 1, 5.0), message(2, 1, 8, 20.0)];
    let p = sequence(&participants, &messages, 4.0);
    let lines = &p.messages;
    assert_eq!(lines.iter().map(|m| m.0).collect::<Vec<_>>(), [5, 3, 2], "messages retain input order");
    let (_, start, end) = p.lifelines[0];
    assert_eq!(start, (12.5, PARTICIPANT_HEIGHT), "a lifeline starts below its header's centre");
    assert_eq!(
        lines[0].1,
        [(start.0, start.1 + 14.0), (p.lifelines[1].1 .0, start.1 + 14.0)],
        "an arrow is horizontal below its label and gap"
    );
    let loop_path = &lines[1].1;
    assert!(
        loop_path[0].1 >= lines[0].1[1].1 + 9.0
            && loop_path[0].0 == loop_path[3].0
            && loop_path[1].0 > loop_path[0].0
            && loop_path[3].1 > loop_path[0].1,
        "self-message leaves and returns lower on the same lifeline"
    );
    assert!(
        lines[2].1[0].1 >= loop_path[3].1 + 24.0 && lines[2].1[0].0 > lines[2].1[1].0,
        "the reply clears the loop and points to its recipient"
    );
    assert!(
        end.1 > lines[2].1[0].1 && p.lifelines.iter().all(|(_, a, b)| a.0 == b.0 && b.1 == end.1),
        "every vertical lifeline reaches below the final arrow"
    );
}

#[test]
fn sequence_headers_keep_their_width_and_order() {
    let participants = [
        Participant { id: 9, width: 30.0 },
        Participant { id: 2, width: 6.0 },
        Participant { id: 7, width: 75.0 },
    ];
    let p = sequence(&participants, &[], 8.0);
    assert_eq!(
        p.participants,
        [(9, 0.0, 0.0), (2, 38.0, 0.0), (7, 52.0, 0.0)],
        "headers appear in input order with the requested clear gap"
    );
}

#[test]
fn invalid_sequence_items_leave_no_empty_rows() {
    let participants = [Participant { id: 3, width: 10.0 }; 2];
    let m = Message { id: 1, from: 3, to: 3, height: 2.0 };
    let p = sequence(&participants, &[Message { to: 99, ..m }, m, m], 0.0);
    assert_eq!(p.messages.len(), 1, "only a valid distinct message occupies a row");
    assert_eq!(
        p.skipped.iter().map(|s| (s.kind, s.reason)).collect::<Vec<_>>(),
        [
            (ItemKind::Participant, SkipReason::DuplicateId),
            (ItemKind::Message, SkipReason::MissingEndpoint(99)),
            (ItemKind::Message, SkipReason::DuplicateId)
        ],
        "invalid sequence items are reported in input order"
    );
    assert_eq!(
        p.messages,
        sequence(&participants[..1], &[m], 0.0).messages,
        "skipped messages consume no vertical space"
    );
}
