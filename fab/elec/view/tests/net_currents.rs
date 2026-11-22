use fab_elec_view::live::net::*;

#[test]
fn pin_currents_become_wire_currents() {
    fn close(actual: f32, expected: f32, tolerance: f32, what: &str) {
        assert!((actual - expected).abs() <= tolerance, "{what}: got {actual}, expected {expected}");
    }

    fn kcl(tree: &NetTree, currents: &[f32], draws: &[f64], tolerance: f64, what: &str) {
        assert_eq!(currents.len(), tree.segs.len(), "{what}: output segment count changed");
        let mut residual = draws.to_vec();
        for (&(a, b), &current) in tree.segs.iter().zip(currents) {
            residual[a as usize] += current as f64;
            residual[b as usize] -= current as f64;
        }
        for (node, error) in residual.into_iter().enumerate() {
            assert!(error.abs() <= tolerance, "{what}: KCL at node {node} is off by {error} A");
        }
    }

    let mut scratch = NetCurrents::new();
    let mut out = vec![42.0];
    let mut chain = NetTree {
        nodes: 6,
        segs: (0..5).map(|n| (n, n + 1)).collect(),
        pins: vec![(0, 0), (5, 1)],
        ..NetTree::default()
    };
    scratch.currents(&chain, &[-0.001, 0.001], &mut out);
    assert_eq!(out.len(), 5, "chain: output was not cleared and resized");
    for &current in &out {
        close(current, 0.001, 1e-10, "chain: current must flow from source to load");
    }
    chain.segs[2] = (3, 2);
    scratch.currents(&chain, &[-0.001, 0.001], &mut out);
    for (k, &current) in out.iter().enumerate() {
        close(current, if k == 2 { -0.001 } else { 0.001 }, 1e-10, "chain: reversing an edge must flip only its sign");
    }

    let t = NetTree { nodes: 4, segs: vec![(0, 1), (1, 2), (1, 3)], pins: vec![(0, 0), (2, 1), (3, 2)], ..NetTree::default() };
    let t_pins = [-0.005, 0.002, 0.003];
    scratch.currents(&t, &t_pins, &mut out);
    for (&current, expected) in out.iter().zip([0.005, 0.002, 0.003]) {
        close(current, expected, 1e-9, "T: trunk and legs must carry the sum of their loads");
    }
    kcl(&t, &out, &[-0.005, 0.0, 0.002, 0.003], 1e-9, "T junction");
    close(through_current(&t, &t_pins), 0.005, 1e-9, "T: through current must be 5 mA");

    let mut dangling = t.clone();
    dangling.nodes = 6;
    dangling.segs.extend([(1, 4), (4, 5)]);
    scratch.currents(&dangling, &t_pins, &mut out);
    assert_eq!(&out[3..], &[0.0, 0.0], "dangling branch: an unloaded branch must carry exactly zero");

    let paths = NetTree {
        nodes: 4,
        segs: vec![(0, 1), (1, 3), (0, 2), (2, 3)],
        pins: vec![(0, 0), (3, 1)],
        conductance: vec![1.0, 1.0, 3.0, 3.0],
        ..NetTree::default()
    };
    scratch.currents(&paths, &[-0.004, 0.004], &mut out);
    for (&current, expected) in out.iter().zip([0.001, 0.001, 0.003, 0.003]) {
        close(current, expected, 4e-9, "parallel paths: currents must split in proportion to conductance");
    }
    kcl(&paths, &out, &[-0.004, 0.0, 0.0, 0.004], 0.004 * 1e-6, "parallel paths");

    let side = 20u32;
    let mut grid = NetTree { nodes: side * side, pins: vec![(0, 0), (side * side - 1, 1)], ..NetTree::default() };
    let mut horizontal = vec![usize::MAX; (side * side) as usize];
    let mut vertical = horizontal.clone();
    for y in 0..side {
        for x in 0..side {
            let n = y * side + x;
            if x + 1 < side {
                horizontal[n as usize] = grid.segs.len();
                grid.segs.push((n, n + 1));
            }
            if y + 1 < side {
                vertical[n as usize] = grid.segs.len();
                grid.segs.push((n, n + side));
            }
        }
    }
    scratch.currents(&grid, &[-0.001, 0.001], &mut out);
    let mut draws = vec![0.0; grid.nodes as usize];
    draws[0] = -0.001;
    draws[grid.nodes as usize - 1] = 0.001;
    kcl(&grid, &out, &draws, 0.001 * 1e-5, "grid");
    for y in 0..side {
        for x in 0..side - 1 {
            let a = horizontal[(y * side + x) as usize];
            let b = vertical[(x * side + y) as usize];
            close(out[a], out[b], 0.001 * 1e-5, "grid: reflected paths must carry equal currents");
        }
    }
    assert_eq!(scratch.unconverged(), 0, "loop solves: grid and parallel paths must reach the residual tolerance");

    let islands = NetTree {
        nodes: 6,
        segs: vec![(0, 1), (1, 2), (3, 4), (4, 5)],
        pins: vec![(2, 0), (5, 1)],
        ports: vec![0, 3],
        ..NetTree::default()
    };
    scratch.currents(&islands, &[0.002, -0.002], &mut out);
    for (&current, expected) in out.iter().zip([0.002, 0.002, -0.002, -0.002]) {
        close(current, expected, 1e-10, "islands: ports must supply or receive their own island's current");
    }
    kcl(&islands, &out, &[-0.002, 0.0, 0.002, 0.002, 0.0, -0.002], 1e-9, "island ports");

    let tolerance_pins = [-0.005f32, 0.002, 0.003001];
    let correction = tolerance_pins.iter().map(|&i| i as f64).sum::<f64>() / 3.0;
    scratch.currents(&t, &tolerance_pins, &mut out);
    let corrected = [tolerance_pins[0] as f64 - correction, 0.0, tolerance_pins[1] as f64 - correction, tolerance_pins[2] as f64 - correction];
    kcl(&t, &out, &corrected, 0.005 * 1e-6, "tolerance: imbalance must be removed evenly from pins");

    let mut multiple_ports = t.clone();
    multiple_ports.pins = vec![(1, 0)];
    multiple_ports.ports = vec![0, 2, 3];
    scratch.currents(&multiple_ports, &[0.006], &mut out);
    kcl(&multiple_ports, &out, &[-0.002, 0.006, -0.002, -0.002], 1e-9, "multiple ports: inflow must be split evenly");

    let mut attached = paths.clone();
    attached.nodes = 6;
    attached.segs.extend([(4, 0), (3, 5)]);
    attached.pins = vec![(4, 0), (5, 1)];
    scratch.currents(&attached, &[-0.004, 0.004], &mut out);
    for (&current, expected) in out.iter().zip([0.001, 0.001, 0.003, 0.003, 0.004, 0.004]) {
        close(current, expected, 4e-9, "loop with tails: peeled draws must reach the core with the right sign");
    }
    kcl(&attached, &out, &[0.0, 0.0, 0.0, 0.0, -0.004, 0.004], 4e-9, "loop with tails");

    let parallel = NetTree {
        nodes: 2,
        segs: vec![(0, 1), (1, 0), (0, 0)],
        pins: vec![(0, 0), (1, 1)],
        conductance: vec![1.0, 3.0, 1.0],
        ..NetTree::default()
    };
    scratch.currents(&parallel, &[-0.004, 0.004], &mut out);
    close(out[0], 0.001, 4e-9, "duplicate segments: the first must carry its share");
    close(out[1], -0.003, 4e-9, "duplicate segments: reversed orientation must keep its sign");
    assert_eq!(out[2], 0.0, "self loop: zero length must carry zero current");

    let loop_islands = NetTree {
        nodes: 4,
        segs: vec![(0, 1), (0, 1), (2, 3), (3, 2)],
        pins: vec![(1, 0), (2, 1)],
        ports: vec![0, 3],
        conductance: vec![1.0, 3.0, 1.0, 1.0],
        ..NetTree::default()
    };
    scratch.currents(&loop_islands, &[0.004, -0.002], &mut out);
    for (&current, expected) in out.iter().zip([0.001, 0.003, 0.001, -0.001]) {
        close(current, expected, 4e-9, "loop islands: each component needs its own ground and port balance");
    }
    kcl(&loop_islands, &out, &[-0.004, 0.004, -0.002, 0.002], 4e-9, "loop islands");

    let isolated = NetTree { nodes: 3, pins: vec![(0, 0), (1, 1), (2, 99)], ports: vec![0], ..NetTree::default() };
    scratch.currents(&isolated, &[0.002, -0.002], &mut out);
    assert!(out.is_empty(), "isolated pins and ports: no segments means no output");
    close(through_current(&isolated, &[0.002, -0.002]), 0.002, 1e-10, "through current: a missing pin index must count as zero");
    scratch.currents(&chain, &[], &mut out);
    assert_eq!(out, vec![0.0; 5], "missing pin currents: every wire must carry zero");
    scratch.currents(&NetTree::default(), &[], &mut out);
    assert!(out.is_empty(), "empty net: stale output must be cleared");
    assert_eq!(through_current(&NetTree::default(), &[]), 0.0, "empty net: through current must be zero");
    assert_eq!(scratch.unconverged(), 0, "all loop components must converge when scratch is reused");
}
