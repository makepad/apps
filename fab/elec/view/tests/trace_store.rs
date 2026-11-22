use fab_elec_sim::trace::*;
use fab_elec_instr::synth::Synth;
use fab_elec_view::trace::{columns::*, store::*};
use std::sync::Arc;

#[test]
fn the_pyramid_reads_what_a_scan_reads() {
    let (t, y) = Synth::pwm().record(300_000);
    let other = y.iter().map(|&y| y * -0.5 + 1.0).collect::<Vec<_>>();
    let values = [y, other];
    let n = t.len();
    let mut store = TraceStore::new(1, &[false, false]);
    let mut received = TraceStore::new(1, &[false, false]);
    // the producer leaves its pyramids to a background job (`defer_pyramids`), shared with the
    // delta's receiver
    store.defer_pyramids(true);
    let mut closed = Vec::new();
    for from in (0..n).step_by(1000) {
        closed.clear();
        let end = (from + 1000).min(n);
        for i in from..end {
            store.push(t[i], &[f64::from(values[0][i]), f64::from(values[1][i])], &mut closed);
        }
        received.apply(&TraceDelta {
            generation: 1,
            from,
            time: Arc::from(&t[from..end]),
            values: values.iter().map(|y| Arc::from(&y[from..end])).collect(),
            closed: closed.clone(),
            marks: Vec::new(),
            logic: Vec::new(),
            logic_closed: Vec::new(),
            logic_pages: Vec::new(),
            folds: Vec::new(),
            rewind: None,
        });
    }
    closed.clear();
    let tail = store.time.chunks.len() * CHUNK;
    assert_eq!(store.time.chunks.len(), 73, "the fixture must exercise many closed chunks");
    assert_eq!(store.time.open.len(), 992, "the fixture must exercise an open tail");
    // the job done for every other position: every read below crosses chunks with a pyramid and
    // chunks still without one (their values, scanned)
    let unbuilt = store.take_unbuilt();
    assert_eq!(unbuilt.len(), 2 * 73, "each closed chunk waits for its pyramid");
    for c in unbuilt.iter().enumerate().filter(|(k, _)| k / 2 % 2 == 0).map(|(_, c)| c) {
        c.build();
    }
    assert!(received.signals[1].chunks[0].pyramid().is_some() && received.signals[1].chunks[1].pyramid().is_none(), "the receiver shares the producer's chunks, pyramids and all");
    let after_last = f64::from_bits(t[n - 1].to_bits() + 1);
    let cases = [
        ("whole record", t[0], after_last, 1600),
        ("partial end chunks", t[CHUNK + 37], t[n - CHUNK - 53], 1000),
        ("deep zoom", t[2 * CHUNK + 123], t[2 * CHUNK + 143], 1600),
        ("two chunk pyramid", t[3 * CHUNK + 100], t[3 * CHUNK + 5100], 300),
        ("open tail", t[tail + 7], t[n - 3], 60),
        ("before record", t[0] - 2.0, t[0] - 1.0, 1600),
        ("after record", after_last + 1.0, after_last + 2.0, 1600),
        ("closed into open", t[tail - 1234], after_last, 200),
        ("single column", t[0], after_last, 1),
        ("more columns than samples", t[500], t[507], 1600),
        ("between samples", (t[500] + t[501]) * 0.5, t[501], 100),
        ("zero columns", t[0], after_last, 0),
        ("empty window", t[500], t[500], 100),
        ("reversed window", t[501], t[500], 100),
        ("infinite start", f64::NEG_INFINITY, after_last, 100),
        ("infinite end", t[0], f64::INFINITY, 100),
        ("overflowed duration", -f64::MAX, f64::MAX, 100),
    ];
    let mut buffer = Columns { cols_data: Vec::with_capacity(1600), ..Columns::default() };
    let capacity = buffer.cols_data.capacity();
    for (signal, y) in values.iter().enumerate() {
        for &(name, t0, t1, cols) in &cases {
            let expected = Columns::reduce(&t, y, t0, t1, cols);
            assert_eq!(Columns::reduce_trace(&store, signal, t0, t1, cols), expected, "column reduction: {name}, signal {signal}");
            buffer.reduce_trace_into(&received, signal, t0, t1, cols);
            assert_eq!(buffer, expected, "delta column reduction: {name}, signal {signal}");
            assert_eq!(buffer.cols_data.capacity(), capacity, "buffer capacity lost: {name}, signal {signal}");
        }
    }

    let mut state = 0x82a2_b175_229d_6a5bu64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state as usize
    };
    for case in 0..2000 {
        // Cover all size scales, then favour small ranges to keep the scan cheap.
        let (a, b) = if case < 19 {
            let scale = 1usize << case;
            let len = (scale + random() % scale).min(n);
            let a = random() % (n - len + 1);
            (a, a + len)
        } else if case == 19 {
            (0, n)
        } else {
            match case % 3 {
                0 => {
                    let chunk = random() % store.time.chunks.len();
                    let offset = random() % CHUNK;
                    let a = chunk * CHUNK + offset;
                    (a, a + 1 + random() % (CHUNK - offset))
                }
                1 => {
                    let boundary = (1 + random() % store.time.chunks.len()) * CHUNK;
                    (boundary - 1 - random() % 64, boundary + 1 + random() % 64)
                }
                _ => {
                    let a = tail + random() % (n - tail);
                    (a, a + 1 + random() % (n - a))
                }
            }
        };
        for (signal, y) in values.iter().enumerate() {
            let mut expected = MinMax { min: f32::INFINITY, max: f32::NEG_INFINITY };
            for &value in &y[a..b] {
                expected.min = expected.min.min(value);
                expected.max = expected.max.max(value);
            }
            assert_eq!(range_minmax(&store.signals[signal], a, b), Some(expected), "extremes: range {case}, {a}..{b}, signal {signal}");
        }
    }
    assert_eq!(range_minmax(&store.signals[0], 123, 123), None, "an empty range has no extremes");
    assert_eq!(range_minmax(&store.signals[0], 124, 123), None, "a reversed range has no extremes");

    for i in (0..n).step_by(997).chain([CHUNK - 1, CHUNK, tail - 1, tail, n - 1]) {
        let just_before = if t[i] == 0.0 { -f64::from_bits(1) } else { f64::from_bits(t[i].to_bits() - 1) };
        let just_after = f64::from_bits(t[i].to_bits() + 1);
        for query in [just_before, t[i], just_after] {
            assert_eq!(time_index(&store.time, query), t.partition_point(|&time| time < query), "time lower bound at sample {i}, query {query}");
            assert_eq!(
                partition_time(&store.time, |time| time <= query),
                t.partition_point(|&time| time <= query),
                "time predicate partition at sample {i}, query {query}"
            );
        }
        for (signal, y) in values.iter().enumerate() {
            assert_eq!(value_at(&store, signal, t[i]), Some(y[i]), "exact sample interpolation: sample {i}, signal {signal}");
            if i + 1 < n {
                let midpoint = t[i] + (t[i + 1] - t[i]) * 0.5;
                let expected = (f64::from(y[i]) + f64::from(y[i + 1])) * 0.5;
                let actual = f64::from(value_at(&store, signal, midpoint).expect("midpoint must have a value"));
                assert!((actual - expected).abs() <= 1e-6 * expected.abs().max(1e-12), "midpoint interpolation: sample {i}, signal {signal}, {actual} != {expected}");
            }
        }
    }
    for query in [t[0] - 1.0, after_last + 1.0] {
        assert_eq!(time_index(&store.time, query), t.partition_point(|&time| time < query), "time lower bound outside the record: {query}");
    }
    for (signal, y) in values.iter().enumerate() {
        assert_eq!(value_at(&store, signal, t[0] - 1.0), None, "no value before the record: signal {signal}");
        assert_eq!(value_at(&store, signal, after_last + 1.0), Some(y[n - 1]), "hold the last value: signal {signal}");
    }

    let empty = TraceStore::new(1, &[false, false]);
    assert_eq!(time_index(&empty.time, 0.0), 0, "an empty time axis partitions at zero");
    assert_eq!(value_at(&empty, 0, 0.0), None, "an empty store has no interpolated value");
    assert_eq!(Columns::reduce_trace(&empty, 0, 0.0, 1.0, 100), Columns::reduce(&[], &[], 0.0, 1.0, 100), "an empty store has no columns");
    buffer.reduce_trace_into(&store, 0, t[0], after_last, 1600);
    buffer.reduce_trace_into(&store, 2, 0.0, 1.0, 100);
    assert_eq!(buffer, Columns::reduce(&[], &[], 0.0, 1.0, 100), "an invalid signal clears existing columns and neighbours");
    assert_eq!(value_at(&store, 2, t[0]), None, "an invalid signal has no interpolated value");
    assert_eq!(value_at(&store, 0, f64::NAN), None, "a NaN query has no interpolated value");
    let mut mismatched = store.clone();
    let _ = mismatched.signals[0].open.pop();
    assert_eq!(
        Columns::reduce_trace(&mismatched, 0, t[0], after_last, 100),
        Columns::reduce(&t, &values[0][..n - 1], t[0], after_last, 100),
        "mismatched times and values have no columns"
    );
    assert_eq!(value_at(&mismatched, 0, t[0]), None, "mismatched times and values cannot be interpolated");
    buffer.reduce_trace_into(&store, 0, f64::NAN, 1.0, 100);
    assert!(buffer.t0.is_nan() && buffer.cols_data.is_empty() && buffer.before.is_none() && buffer.after.is_none(), "a NaN window has no columns or neighbours");

    let mut gaps = TraceStore::new(1, &[false]);
    let mut gap_values = vec![f32::NAN; CHUNK + 3];
    gap_values[3] = 2.0;
    gap_values[CHUNK - 1] = -3.0;
    gap_values[CHUNK + 1] = 4.0;
    for (i, &value) in gap_values.iter().enumerate() {
        closed.clear();
        gaps.push(i as f64, &[f64::from(value)], &mut closed);
    }
    for (a, b) in [(0, 3), (4, CHUNK - 1), (0, CHUNK + 3), (CHUNK, CHUNK + 3)] {
        let expected = MinMax {
            min: gap_values[a..b].iter().copied().fold(f32::INFINITY, f32::min),
            max: gap_values[a..b].iter().copied().fold(f32::NEG_INFINITY, f32::max),
        };
        assert_eq!(range_minmax(&gaps.signals[0], a, b), Some(expected), "NaN samples must be skipped in {a}..{b}");
    }
    let missing = Columns::reduce_trace(&gaps, 0, 4.0, (CHUNK - 1) as f64, 1);
    assert_eq!(missing.cols_data.len(), 1, "all-NaN samples still occupy a column");
    assert!(missing.cols_data[0].lo.is_nan() && missing.cols_data[0].hi.is_nan(), "an all-NaN column must match the sample loop's extremes");

    // A record that keeps signal 0 whole and signal 1 for its front only: signal 1's past reads
    // absent and draws nothing (no line reaches into it from what is kept), at any zoom.
    let front = t[n - 1] - t[n - 20 * CHUNK];
    let mut thin = store.clone();
    thin.keep(&Keep { span: f64::INFINITY, budget: usize::MAX, whole: Some(Arc::from([true, false])), front }, &mut Vec::new());
    let gone = thin.time.chunks.iter().take_while(|chunk| chunk[CHUNK - 1] < t[n - 1] - front).count();
    assert!(gone > 10 && thin.signals[1].chunks[..gone].iter().all(is_absent), "signal 1 is absent before its front ({gone} positions)");
    assert!(thin.bytes() < store.bytes(), "an absent chunk costs nothing: {} of {} bytes", thin.bytes(), store.bytes());
    let kept_from = t[gone * CHUNK];
    for &(name, t0, t1, cols) in &cases[..5] {
        assert_eq!(Columns::reduce_trace(&thin, 0, t0, t1, cols), Columns::reduce(&t, &values[0], t0, t1, cols), "the whole signal reads as before: {name}");
        let thinned = Columns::reduce_trace(&thin, 1, t0, t1, cols);
        let at = |x: f32| t0 + f64::from(x) / f64::from(cols) * (t1 - t0);
        let finite = thinned.before.into_iter().chain(thinned.cols_data.iter().flat_map(|c| [(c.x0, c.y0), (c.x1, c.y1)])).filter(|(_, y)| y.is_finite());
        for (x, y) in finite {
            assert!(at(x) >= kept_from - 1e-9 * (t1 - t0).abs().max(1e-9), "{name}: a point of signal 1 drawn at {} s ({y}), in its absent past before {kept_from} s", at(x));
        }
    }
}
