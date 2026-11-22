use fab_elec_bench::align::{self, Alignment, Edge, Kind, Map, Matching};

fn pwm(period: f64, duty: f64, phase: f64, cycles: usize) -> Vec<Edge> {
    (0..cycles).flat_map(|i| {
        let t = phase + i as f64 * period;
        [Edge { t, rising: true }, Edge { t: t + period * duty, rising: false }]
    }).collect()
}
fn partition(expected: &[Edge], actual: &[Edge], matching: &Matching) {
    let mut e = vec![0; expected.len()]; let mut a = vec![0; actual.len()];
    for &(i, j) in &matching.pairs {
        e[i] += 1; a[j] += 1;
        assert_eq!(expected[i].rising, actual[j].rising, "paired edges have the same direction");
    }
    for &i in &matching.missing { e[i] += 1; }
    for &j in &matching.extra { a[j] += 1; }
    assert!(e.iter().all(|&n| n == 1), "every expected index appears exactly once");
    assert!(a.iter().all(|&n| n == 1), "every actual index appears exactly once");
    assert!(matching.pairs.windows(2).all(|p| p[0].0 < p[1].0 && p[0].1 < p[1].1), "paired indices increase on both sides");
}
fn recovered(alignment: &Alignment, offset: f64, gain: f64) {
    assert!((alignment.map.offset - offset).abs() < 0.1e-6, "shared fit recovers offset within 0.1 us: {:?}", alignment.map);
    assert!((alignment.map.gain - gain).abs() < 0.0001, "shared fit recovers skew within 0.01 percent: {:?}", alignment.map);
}

#[test]
fn two_pwm_clocks_recover_skew_and_locate_missing_extra_and_late_edges() {
    let expected = [pwm(1e-3, 0.3, 0.1e-3, 20), pwm(0.4e-3, 0.5, 0.08e-3, 50)];
    let offset = 12.4e-6; let gain = 1.0031; let tolerance = 1e-6;
    let jitter = [-0.2e-6, 0.1e-6, 0.0, 0.2e-6, -0.1e-6];
    let mut actual: Vec<Vec<Edge>> = expected.iter().enumerate().map(|(c, edges)| edges.iter().enumerate().map(|(i, e)| {
        Edge { t: offset + gain * e.t + jitter[(i + c) % jitter.len()], rising: e.rising }
    }).collect()).collect();
    let removed = [expected[1][40].t, expected[1][41].t];
    drop(actual[1].drain(40..42));
    let moved = expected[0][24].t; actual[0][24].t += 5e-6;
    let extra = [6.6e-3, 6.6e-3 + 5e-6 / gain];
    for (i, &t) in extra.iter().enumerate() { actual[0].push(Edge { t: offset + gain * t, rising: i == 0 }); }
    actual[0].sort_unstable_by(|a, b| a.t.total_cmp(&b.t));
    let channels = [(expected[0].as_slice(), actual[0].as_slice()), (expected[1].as_slice(), actual[1].as_slice())];
    let fitted = align::align(&channels, Map { offset, gain: 1.0 }, tolerance, true);
    recovered(&fitted, offset, gain);
    assert_eq!(fitted.matched, 138, "only the removed pulse's two expected edges are unmatched");
    for (c, matching) in fitted.channels.iter().enumerate() {
        partition(&expected[c], &actual[c], matching);
        let differences = align::differences(&expected[c], &actual[c], matching, fitted.map, tolerance);
        assert!(differences.windows(2).all(|p| p[0].t <= p[1].t), "channel {c} differences are in expected time order");
        if c == 0 {
            assert_eq!(matching.missing.len(), 0, "channel 0 has no missing edges");
            assert_eq!(matching.extra.len(), 2, "channel 0 has exactly the inserted pulse's edges");
            assert_eq!(differences.len(), 3, "channel 0 has two extras and one late edge only");
            for (i, &t) in extra.iter().enumerate() {
                assert_eq!(differences[i].kind, Kind::Extra, "inserted pulse edge {i} is extra");
                assert_eq!(differences[i].rising, i == 0, "inserted pulse edge {i} retains its direction");
                assert!((differences[i].t - t).abs() < 0.3e-6, "inserted pulse edge {i} is located within 0.3 us");
            }
            assert_eq!(differences[2].kind, Kind::Late, "moved edge is late");
            assert_eq!(differences[2].t, moved, "late edge is located at its expected time");
            assert!(differences[2].rising, "moved rising edge retains its direction");
            assert!((differences[2].dt - 5e-6).abs() < 0.3e-6, "late residual is within 0.3 us of 5 us");
        } else {
            assert_eq!(matching.extra.len(), 0, "channel 1 has no extra edges");
            assert_eq!(matching.missing.len(), 2, "channel 1 has exactly two missing edges");
            assert_eq!(differences.len(), 2, "channel 1 has only the removed pulse's differences");
            for (i, &t) in removed.iter().enumerate() {
                assert_eq!(differences[i].kind, Kind::Missing, "removed pulse edge {i} is missing");
                assert_eq!(differences[i].t, t, "removed pulse edge {i} retains its expected time");
                assert_eq!(differences[i].rising, i == 0, "removed pulse edge {i} retains its direction");
            }
        }
    }
    let (searched, runner_up) = align::search_offset(&channels, tolerance, true);
    recovered(&searched, offset, gain);
    assert_eq!(searched.map, fitted.map, "unanchored search reaches the same fitted map");
    assert_eq!(searched.channels, fitted.channels, "unanchored search reaches the same edge partitions");
    assert!(runner_up < searched.matched, "the winning unanchored match is unique by matched count");
    let identical = [(expected[0].as_slice(), expected[0].as_slice()), (expected[1].as_slice(), expected[1].as_slice())];
    let same = align::align(&identical, Map { offset: 0.0, gain: 1.0 }, tolerance, false);
    assert_eq!(same.map.gain, 1.0, "skew none keeps gain exactly one");
    assert_eq!(same.map.offset, 0.0, "identical clocks have zero offset");
    for (c, &(e, a)) in identical.iter().enumerate() {
        assert!(align::differences(e, a, &same.channels[c], same.map, tolerance).is_empty(), "identical channel {c} has no differences");
        partition(e, a, &same.channels[c]);
    }
}

#[test]
fn sparse_edges_keep_finite_windows_and_offset_only_fits() {
    let e = [Edge { t: 2.0, rising: true }, Edge { t: 2.0, rising: false }];
    let a = [Edge { t: 2.25, rising: true }, Edge { t: 2.5, rising: false }];
    assert_eq!(align::fit(&e, &a, &[], true), None, "no pairs cannot fit a clock");
    for edges in [&[][..], &e[..1], &e[..]] {
        assert_eq!(align::search_window(edges, 0.01), 0.02, "no positive edge interval uses twice tolerance");
    }
    let (map, rms) = align::fit(&e, &a, &[(0, 0), (1, 1)], true).unwrap();
    assert_eq!(map, Map { offset: 0.375, gain: 1.0 }, "coincident expected times fit only mean offset");
    assert_eq!(rms, 0.125, "coincident expected times report the actual residual RMS");
    let identity = Map { offset: 0.0, gain: 1.0 };
    let empty = align::match_edges(&e, &[], identity, 1.0);
    partition(&e, &[], &empty);
    assert_eq!(empty.missing, vec![0, 1], "an empty actual list leaves every expected edge missing");
    let empty = align::match_edges(&[], &a, identity, 1.0);
    partition(&[], &a, &empty);
    assert_eq!(empty.extra, vec![0, 1], "an empty expected list leaves every actual edge extra");
    let e = [Edge { t: 1.0, rising: true }];
    let a = [Edge { t: 0.5, rising: false }, Edge { t: 0.8, rising: true }, Edge { t: 1.1, rising: true }];
    let matching = align::match_edges(&e, &a, identity, 1.0);
    partition(&e, &a, &matching);
    assert_eq!(matching.pairs, vec![(0, 2)], "opposite early edge and farther same-direction edge cannot steal the closer match");
    let differences = align::differences(&e, &a, &matching, Map { offset: 0.2, gain: 1.0 }, 0.05);
    assert_eq!(differences.last().unwrap().kind, Kind::Early, "a negative out-of-tolerance residual is early");
}
