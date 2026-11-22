use super::*;

const E: Pt = [1, 0];
const W: Pt = [-1, 0];
const N: Pt = [0, 1];
const S: Pt = [0, -1];

fn net(id: u32, ends: &[(Pt, Pt)]) -> RNet {
    RNet { net: id, bundle: None, terms: ends.iter().map(|&(at, dir)| RTerm { at, dir }).collect(), tree: Vec::new() }
}

// Inclusive segment/box intersection. Testing the full span catches a wall hidden between
// two polyline corners.
fn touches(seg: &[Pt], [l, b, r, t]: [i32; 4]) -> bool {
    let [x, y] = seg[0];
    let [nx, ny] = seg[1];
    x.min(nx) <= r && x.max(nx) >= l && y.min(ny) <= t && y.max(ny) >= b
}

fn check_wires(sheet: &SheetRoute, nets: &[RNet]) {
    for (_, run) in &sheet.runs {
        assert!(run.len() >= 2, "each run has two distinct ends");
        for seg in run.windows(2) {
            assert_ne!(seg[0][0] == seg[1][0], seg[0][1] == seg[1][1], "each segment has exactly one moving axis");
        }
        for corner in run.windows(3) {
            assert_ne!(corner[0][0] == corner[1][0], corner[1][0] == corner[2][0], "collinear steps merge into one segment");
        }
    }
    for net in nets {
        for term in &net.terms {
            let leaves = sheet.runs.iter().filter(|(id, _)| *id == net.net).any(|(_, run)| {
                run.windows(2).any(|seg| [0, 1].iter().any(|&end| {
                    let [x, y] = seg[end];
                    let [nx, ny] = seg[1 - end];
                    [x, y] == term.at && [(nx - x).signum(), (ny - y).signum()] == term.dir
                }))
            });
            assert!(leaves, "net {} must leave pin {:?} in direction {:?}", net.net, term.at, term.dir);
        }
    }
}

#[test]
fn facing_pins_and_closed_walls() {
    let nets = [net(4, &[([0, 0], E), ([8, 0], W)])];
    let bodies = [
        Obstacle { bx: [-2, -1, 0, 1], ink: Ink::Wall },
        Obstacle { bx: [8, -1, 10, 1], ink: Ink::Wall },
        Obstacle { bx: [3, -3, 5, 3], ink: Ink::Wall },
    ];
    let opts = RouterOpts { margin: 2, ..RouterOpts::default() };
    let straight = route(&nets, &[], &bodies[..2], &opts);
    assert_eq!(straight.runs, [(4, vec![[0, 0], [8, 0]])], "facing boundary pins share a single straight run");
    assert!(straight.junctions.is_empty(), "a straight pair has no junction");
    assert_eq!(route(&nets, &[], &bodies[..2], &RouterOpts { reach: 7, ..opts }).unrouted, [4], "a pin 8 steps from its tree is beyond a reach of 7");
    assert_eq!(route(&nets, &[], &bodies[..2], &RouterOpts { reach: 8, ..opts }).runs, straight.runs, "and within a reach of 8");
    let around = route(&nets, &[], &bodies, &opts);
    assert!(around.unrouted.is_empty(), "the wall leaves space for a detour");
    assert_eq!(around.runs.len(), 1, "one continuous branch goes around the wall");
    assert!(around.runs[0].1.iter().any(|p| p[1].abs() > opts.margin),
        "the search finds a detour beyond the pin row's margin");
    for seg in around.runs[0].1.windows(2) {
        assert!(!touches(seg, bodies[2].bx), "the whole segment clears the wall interior and its boundary");
    }
    check_wires(&around, &nets);
    assert_eq!(around, route(&nets, &[], &bodies, &opts), "equal-cost wall detours have a repeatable tie-break");

    let adjacent = [net(1, &[([0, 0], E), ([1, 0], W)])];
    let thin = [Obstacle { bx: [0, -1, 1, 1], ink: Ink::Wall }];
    let blocked = route(&adjacent, &[], &thin, &opts);
    assert_eq!(blocked.unrouted, [1], "two boundary pins cannot connect through a one-step wall");
    assert!(blocked.runs.is_empty(), "no segment is emitted inside the thin wall");
}

// Check every pair of segments of different nets, counting only perpendicular crossings
// in both segments' interiors. The fixtures place all pins away from these crossings.
fn crossings(sheet: &SheetRoute) -> usize {
    let mut count = 0;
    for (i, (a, run)) in sheet.runs.iter().enumerate() {
        for (b, other) in &sheet.runs[i + 1..] {
            if a == b { continue; }
            for p in run.windows(2) {
                for q in other.windows(2) {
                    let horizontal = p[0][1] == p[1][1];
                    if horizontal == (q[0][1] == q[1][1]) {
                        let along = if horizontal { 0 } else { 1 };
                        if p[0][1 - along] != q[0][1 - along] { continue; }
                        let low = p[0][along].min(p[1][along]).max(q[0][along].min(q[1][along]));
                        let high = p[0][along].max(p[1][along]).min(q[0][along].max(q[1][along]));
                        assert!(low >= high, "different nets never share a collinear segment");
                    } else {
                        let (h, v) = if horizontal { (p, q) } else { (q, p) };
                        let (x, y) = (v[0][0], h[0][1]);
                        if touches(h, [x, y, x, y]) && touches(v, [x, y, x, y]) {
                            assert!(x != h[0][0] && x != h[1][0] && y != v[0][1] && y != v[1][1],
                                "both nets continue straight through the crossing");
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

#[test]
fn perpendicular_crossing_and_its_cost() {
    let nets = [net(8, &[([0, -4], N), ([0, 4], S)]), net(3, &[([-4, 0], E), ([4, 0], W)])];
    let tight = RouterOpts { margin: 0, ..RouterOpts::default() };
    let sheet = route(&nets, &[], &[], &tight);
    check_wires(&sheet, &nets);
    assert_eq!(sheet.runs[0].0, 3, "equal half-perimeters route the lower net id first");
    assert_eq!(crossings(&sheet), 1, "two edge-to-edge wires must cross exactly once");
    assert!(sheet.junctions.is_empty(), "crossing another net does not make an electrical junction");
    // A one-step grid border leaves room for a cheaper detour around the wire's end.
    let detour = route(&nets, &[], &[], &RouterOpts { cross: 80, margin: 1, ..tight });
    check_wires(&detour, &nets);
    assert_eq!(crossings(&detour), 0, "a high crossing cost sends the wire around");

    let same_row = [net(3, &[([-2, 0], E), ([2, 0], W)]), net(8, &[([-5, 0], E), ([5, 0], W)])];
    let parallel = route(&same_row, &[], &[], &tight);
    assert_eq!(parallel.unrouted, [8], "a single-row grid leaves no alternative to a forbidden overlap");
    let parallel = route(&same_row, &[], &[], &RouterOpts::default());
    check_wires(&parallel, &same_row);
    assert_eq!(crossings(&parallel), 0, "the longer net detours clear of the shorter collinear net");
}

#[test]
fn frame_cost_competes_with_detour_length() {
    let nets = [net(1, &[([0, 0], E), ([8, 0], W)])];
    let draw = |height: i32| route(&nets, &[], &[Obstacle { bx: [2, -height, 6, height], ink: Ink::Frame }], &RouterOpts::default());
    let shallow = draw(1);
    check_wires(&shallow, &nets);
    for seg in shallow.runs[0].1.windows(2) {
        assert!(!touches(seg, [3, 0, 5, 0]), "the short detour avoids all three interior cells");
    }
    let tall = draw(10);
    assert_eq!(tall.runs, [(1, vec![[0, 0], [8, 0]])], "three frame steps cost less than the long detour and four bends");
}

#[test]
fn third_pin_branches_in_the_middle_of_a_run() {
    let nets = [net(7, &[([0, 0], E), ([6, 0], W), ([3, 5], S)])];
    let sheet = route(&nets, &[], &[], &RouterOpts::default());
    assert_eq!(sheet.runs, [(7, vec![[0, 0], [6, 0]]), (7, vec![[3, 0], [3, 5]])],
        "the nearest pin forms the trunk and the third connects to its middle");
    assert_eq!(sheet.junctions, [(7, [3, 0])], "the only junction is the three-way branch");
    check_wires(&sheet, &nets);
}

#[test]
fn single_pin_joins_an_existing_tree() {
    let mut nets = [net(1, &[([5, 6], S)])];
    nets[0].tree.push(vec![[0, 0], [10, 0]]);
    let opts = RouterOpts::default();
    let sheet = route(&nets, &[], &[], &opts);
    assert!(sheet.unrouted.is_empty(), "a single pin can route to an existing tree");
    assert_eq!(sheet.runs, [(1, vec![[5, 6], [5, 0]])], "only the new branch from pin to tree is emitted");
    assert_eq!(sheet.junctions, [(1, [5, 0])], "the two existing edges and the branch form one junction");

    nets[0].terms[0].at = [5, 0];
    let on_tree = route(&nets, &[], &[], &opts);
    assert!(on_tree.unrouted.is_empty(), "a pin on the tree is connected even when it faces across the wire");
    assert!(on_tree.runs.is_empty(), "a pin on the tree needs no branch");
    assert!(on_tree.junctions.is_empty(), "a pin adding no edge to a straight wire creates no junction");
}

#[test]
fn drawn_wire_crossing_and_same_net_join() {
    let mut nets = [net(1, &[([-4, 0], E)])];
    nets[0].tree.push(vec![[4, 0], [7, 0]]);
    let drawn = [(2, vec![[0, -2], [0, 2]]), (1, nets[0].tree[0].clone())];
    let opts = RouterOpts::default();
    let mut sheet = route(&nets, &drawn, &[], &opts);
    assert_eq!(sheet.runs, [(1, vec![[-4, 0], [4, 0]])], "the pin can join its own drawn wire end to end");
    assert!(sheet.junctions.is_empty(), "a foreign crossing and an end-to-end join need no junctions");
    sheet.runs.push(drawn[0].clone());
    assert_eq!(crossings(&sheet), 1, "the branch crosses the foreign drawn wire once at right angles");

    let mut detour = route(&nets, &drawn, &[], &RouterOpts { cross: 80, ..opts });
    assert_eq!(detour.runs.len(), 1, "the pin still reaches its tree when crossing costs more than a detour");
    detour.runs.push(drawn[0].clone());
    assert_eq!(crossings(&detour), 0, "the high crossing cost sends the branch around the foreign drawn wire");
}

#[test]
fn failed_tree_releases_its_trunk() {
    let boxed = [2, 4];
    let walls = [[1, 3, 3, 3], [1, 5, 3, 5], [1, 4, 1, 4], [3, 4, 3, 4]].map(|bx| Obstacle { bx, ink: Ink::Wall });
    let nets = [
        net(9, &[([0, 0], E), ([2, 0], W), (boxed, S)]),
        net(5, &[([-2, 0], E), ([7, 0], W)]),
        net(2, &[([2, 2], N), (boxed, S)]),
        net(6, &[]), net(7, &[(boxed, S)]),
    ];
    let opts = RouterOpts { margin: 1, ..RouterOpts::default() };
    let sheet = route(&nets, &[], &walls, &opts);
    assert_eq!(sheet.unrouted, [9, 2], "boxed nets are reported in input order and lone pins are skipped");
    assert_eq!(sheet.runs, [(5, vec![[-2, 0], [7, 0]])], "the later net can reuse the failed tree's entire trunk");
    assert!(sheet.junctions.is_empty(), "a discarded tree leaves no junctions");
}
