use fab_elec_mcu_emu::{Clock, PcSet, TIME_EXACT_LIMIT, tick_at, time};

#[test]
fn rational_clocks_project_and_partition_edges_exactly() {
    for num in [32768, 136000, 17_500_000] {
        let clock = Clock::new(num, 1, 37, 480_000_000);
        let k = u128::from(clock.tick_hz) * u128::from(clock.den);
        let near = (((1u128 << 40) - u128::from(clock.start)) * u128::from(num) / k) as u64;
        for n in [0, 1, 2, 1_000_000, near - 1, near, near + 1] {
            let expected = clock.start + ((u128::from(n) * k + u128::from(num) - 1) / u128::from(num)) as u64;
            let edge = clock.edge(n);
            assert_eq!(edge, expected, "{num} Hz edge {n} uses the closed form");
            assert_eq!(clock.edges_through(edge), n + 1, "{num} Hz edge {n} is counted inclusively");
            if n != 0 {
                assert_eq!(clock.edges_through(edge - 1), n, "{num} Hz edge {n} is not counted early");
            }
            assert_eq!(clock.next_edge(edge), (n, edge), "next edge includes equality at {num} Hz");
            let (next, at) = clock.next_edge(edge + 1);
            assert_eq!(next, n + 1, "next edge advances after edge {n} at {num} Hz");
            assert_eq!(at, clock.edge(next), "next edge lands on the clock at {num} Hz");
            assert!(at >= edge + 1, "next edge never precedes its query at {num} Hz");
        }
        assert_eq!(clock.edges_through(36), 0, "no edge before the origin at {num} Hz");
        assert_eq!(clock.next_edge(0), (0, 37), "first edge is the origin at {num} Hz");
        for (a, b, c) in [(0, 37, 38), (37, clock.edge(2), clock.edge(5)),
            (clock.edge(near) - 1, clock.edge(near), clock.edge(near + 2) + 1)] {
            assert_eq!(clock.edges_in(a, c), clock.edges_in(a, b) + clock.edges_in(b, c),
                "half-open edge intervals partition at {num} Hz");
        }
    }
    let rational = Clock::new(3, 2, 0, 5);
    assert_eq!((rational.edge(1), rational.edge(3)), (4, 10), "non-unit denominator stays rational");
    let dense = Clock::new(3, 1, 0, 1);
    assert_eq!(dense.next_edge(1), (1, 1), "coincident edges return the first edge index");
    assert_eq!(dense.edges_in(1, 2), 3, "coincident edges are all counted");
}

#[test]
fn tick_time_round_trips_with_a_reset_offset() {
    let reset = 0.125;
    for hz in [16_000_000, 480_000_000] {
        // Keep the offset inside the absolute exact-time horizon too.
        let end = (TIME_EXACT_LIMIT as u64 - 1) * hz;
        for center in [0, 1, hz - 1, end - 1, end] {
            for tick in center.saturating_sub(2)..=center + 2 {
                let t = time(reset, tick, hz);
                let next = time(reset, tick + 1, hz);
                assert_eq!(tick_at(reset, t, hz), tick, "{hz} Hz tick {tick} round trips exactly");
                assert!(next > t, "{hz} Hz neighboring ticks near {tick} are strictly ordered");
                let middle = t + (next - t) / 2.0;
                if middle > t {
                    assert_eq!(tick_at(reset, middle, hz), tick + 1,
                        "{hz} Hz inverse rounds a between-tick time upward");
                }
            }
        }
    }
}

#[test]
fn pc_marks_cover_only_inserted_addresses() {
    let mut set = PcSet::new(100, 130);
    assert!(set.is_empty(), "a new set has no marks");
    let marked = [100, 163, 164, 229];
    for pc in marked { set.insert(pc); }
    set.insert(164);
    assert!(!set.is_empty(), "inserting a mark makes the set nonempty");
    for pc in 0..260 {
        assert_eq!(set.contains(pc), marked.contains(&pc), "membership at address {pc}");
    }
    assert!(!set.contains(u32::MAX), "a far-out address cannot wrap into the range");
    let mut last = PcSet::new(u32::MAX, 1);
    last.insert(u32::MAX);
    assert!(last.contains(u32::MAX), "the final address unit is representable");
    assert!(!last.contains(0), "the final-address range does not wrap");
    assert!(PcSet::default().is_empty(), "the default set is empty");
    assert!(!PcSet::default().contains(0), "the default set contains no address zero");
}
