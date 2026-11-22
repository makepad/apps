//! A rolling record retains whole chunks without breaking a live delta stream.

use fab_elec_sim::live::Latest;
use fab_elec_sim::spill::LogicSpill;
use fab_elec_sim::trace::{is_absent, Chunk, Keep, LogicEdge, MinMax, SignalId, TraceDelta, TracePublisher, TraceStore, CHUNK, LOGIC_CHUNK, LOGIC_KEEP};
use std::mem::size_of;
use std::sync::Arc;

#[test]
fn rolling_record_drops_whole_chunks() {
    let dt = 1e-6;
    let n = 10 * CHUNK + 123;
    let values = |i: usize| [i as f64 + 0.1, (i % 97) as f64 - 48.0];
    let mut producer = TraceStore::new(7, &[true, false]);
    let mut consumer = TraceStore::default();
    let mut marks = Vec::new();
    let append = |producer: &mut TraceStore, consumer: &mut TraceStore, marks: &mut Vec<f64>, start: usize, end: usize| {
        let (mut pending_t, mut pending_v, mut closed) = (Vec::new(), [Vec::new(), Vec::new()], Vec::new());
        let mut pending_marks = Vec::new();
        let mut from = start;
        let cuts = [start + 1, start + 700, start + CHUNK, start + CHUNK + 5, start + 2 * CHUNK + 17, end];
        for i in start..end {
            let t = i as f64 * dt;
            let y = values(i);
            producer.push(t, &y, &mut closed);
            pending_t.push(t);
            for (pending, value) in pending_v.iter_mut().zip(y) {
                pending.push(value as f32);
            }
            if i % 1000 == 0 {
                producer.marks.push(t);
                marks.push(t);
                pending_marks.push(t);
            }
            if cuts.contains(&(i + 1)) {
                let delta = TraceDelta {
                    generation: producer.generation,
                    from,
                    time: std::mem::take(&mut pending_t).into(),
                    values: pending_v.iter_mut().map(|pending| Arc::from(std::mem::take(pending))).collect(),
                    closed: std::mem::take(&mut closed),
                    marks: std::mem::take(&mut pending_marks),
                    logic: Vec::new(),
                    logic_closed: Vec::new(),
                    logic_pages: Vec::new(),
                    folds: Vec::new(),
                    rewind: None,
                };
                from += delta.time.len();
                consumer.apply(&delta);
                assert_eq!(consumer.generation, producer.generation, "each delta preserves the generation");
                assert_eq!(consumer.base, producer.base, "each delta continues the retained base");
                assert_eq!(consumer.len(), producer.len(), "each delta continues the retained length");
            }
        }
    };
    append(&mut producer, &mut consumer, &mut marks, 0, n);

    let ghost = Arc::clone(&producer.signals[0].chunks[1]);
    assert_eq!(producer.trim_before((CHUNK - 1) as f64 * dt), 0, "a chunk ending exactly at the cutoff is retained");
    let seconds = 4.5 * CHUNK as f64 * dt;
    let cutoff = (n - 1) as f64 * dt - seconds;
    let expected_chunks = (0..n / CHUNK).filter(|&chunk| ((chunk + 1) * CHUNK - 1) as f64 * dt < cutoff).count();
    let expected_base = expected_chunks * CHUNK;
    let first_kept = expected_base as f64 * dt;
    let kept_marks: Vec<_> = marks.iter().copied().filter(|&t| t >= first_kept).collect();
    let dropped_mark_bytes = (marks.len() - kept_marks.len()) * size_of::<f64>();
    let mut pyramid_bins = 0;
    let mut level_len = CHUNK / 4;
    while level_len != 0 {
        pyramid_bins += level_len;
        level_len /= 4;
    }
    assert_eq!(ghost.pyramid().map(<[MinMax]>::len), Some(pyramid_bins), "a store with no pool builds each pyramid at close, every level");
    let chunk_bytes = CHUNK * size_of::<f64>()
        + 2 * CHUNK * size_of::<f32>()
        + CHUNK * size_of::<f64>()
        + 2 * pyramid_bins * size_of::<MinMax>();
    let bytes_before = producer.bytes();
    let end_before = producer.base + producer.len();
    let point_bytes = 2 * size_of::<f64>() + 2 * size_of::<f32>();
    assert_eq!(
        bytes_before,
        n * point_bytes + (n / CHUNK) * 2 * pyramid_bins * size_of::<MinMax>() + marks.len() * size_of::<f64>(),
        "bytes count times, both signals, exact copies, all pyramid levels, open tails and marks"
    );
    assert_eq!(producer.keep_last(seconds), expected_chunks, "retention drops exactly the chunks ending before the cutoff");
    assert_eq!(producer.base, expected_base, "base advances by the number of dropped samples");
    assert_eq!(producer.base + producer.len(), end_before, "trimming preserves the next absolute sample index");
    assert_eq!(producer.marks, kept_marks, "retained marks are exactly those at or after the first kept time");
    assert_eq!(
        bytes_before - producer.bytes() - dropped_mark_bytes,
        expected_chunks * chunk_bytes,
        "after accounting for dropped marks, bytes decrease by exactly the dropped chunk payloads, including a shared ghost"
    );
    for position in 0..producer.len() {
        let absolute = producer.base + position;
        assert_eq!(producer.time.at(position), absolute as f64 * dt, "retained time at absolute index {absolute}");
        for (signal, expected) in producer.signals.iter().zip(values(absolute)) {
            assert_eq!(signal.at(position), expected as f32, "retained signal {:?} at absolute index {absolute}", signal.id);
        }
    }
    let exact = producer.signals[0]
        .chunks
        .iter()
        .flat_map(|chunk| chunk.exact.as_deref().expect("signal 0 retains closed exact copies").iter())
        .chain(producer.signals[0].open_exact.as_ref().expect("signal 0 retains its exact open tail").iter());
    for (position, &value) in exact.enumerate() {
        let absolute = producer.base + position;
        assert_eq!(value, values(absolute)[0], "retained exact value at absolute index {absolute}");
    }
    assert_eq!(consumer.keep_last(seconds), expected_chunks, "the consumer trims the same chunk positions");
    let ghost_exact = ghost.exact.as_deref().expect("the ghost retains its exact copy");
    for position in 0..CHUNK {
        let expected = values(CHUNK + position)[0];
        assert_eq!(ghost.y[position], expected as f32, "the ghost preserves display value {position}");
        assert_eq!(ghost_exact[position], expected, "the ghost preserves exact value {position}");
    }

    append(&mut producer, &mut consumer, &mut marks, n, n + 2 * CHUNK);
    assert_eq!(producer.generation, 7, "rolling retention preserves the producer generation");
    assert_eq!(consumer.generation, producer.generation, "continued deltas preserve the consumer generation");
    assert_eq!(consumer.base, expected_base, "continued deltas do not restart the consumer");
    assert_eq!(consumer.base, producer.base, "producer and consumer retain the same base");
    assert_eq!(consumer.base + consumer.len(), n + 2 * CHUNK, "continued deltas reach the new absolute end");
    assert_eq!(consumer.time, producer.time, "continued deltas preserve every retained time");
    let kept_marks: Vec<_> = marks.iter().copied().filter(|&t| t >= first_kept).collect();
    assert_eq!(producer.marks, kept_marks, "new producer marks follow the retained marks");
    assert_eq!(consumer.marks, producer.marks, "continued deltas preserve every retained mark");
    for (actual, expected) in consumer.signals.iter().zip(&producer.signals) {
        assert_eq!(actual.chunks.len(), expected.chunks.len(), "continued deltas preserve signal chunk positions");
        for (a, b) in actual.chunks.iter().zip(&expected.chunks) {
            assert!(Arc::ptr_eq(a, b), "closed chunks remain shared after trimming and continued deltas");
        }
        for position in 0..producer.len() {
            let absolute = producer.base + position;
            assert_eq!(actual.at(position), expected.at(position), "consumer value at absolute index {absolute}");
            assert_eq!(actual.at(position), values(absolute)[actual.id.0 as usize] as f32, "continued value matches its original absolute index {absolute}");
        }
    }

    assert_eq!(producer.index_at(first_kept - dt), None, "time before the first kept point has no index");
    let open_start = producer.time.chunks.len() * CHUNK;
    for position in [0, CHUNK - 1, CHUNK, open_start - 1, open_start, producer.len() - 1] {
        let t = producer.time.at(position);
        let absolute = producer.base + position;
        assert_eq!(producer.index_at(t), Some(absolute), "exact time resolves to absolute index {absolute}");
        if position + 1 < producer.len() {
            let between = (t + producer.time.at(position + 1)) * 0.5;
            assert_eq!(producer.index_at(between), Some(absolute), "time between points resolves to earlier absolute index {absolute}");
        }
    }

    let tail = producer.time.open.clone();
    let tails: Vec<_> = producer.signals.iter().map(|signal| (signal.open.clone(), signal.open_exact.clone())).collect();
    let bytes_without_edges = producer.bytes();
    let mut sealed = Vec::new();
    for (i, t) in [first_kept, tail[0] - dt, tail[0], tail[1]].into_iter().enumerate() {
        producer.push_logic(SignalId(1), LogicEdge { t, high: i % 2 == 1 }, &mut sealed);
    }
    assert_eq!(producer.bytes() - bytes_without_edges, 4 * size_of::<LogicEdge>(), "bytes include the logic entries");
    let remaining_chunks = producer.time.chunks.len();
    let end_before = producer.base + producer.len();
    assert_eq!(producer.trim_before((tail[0] + tail[1]) * 0.5), remaining_chunks, "a cutoff inside the open tail drops only closed chunks");
    assert_eq!(producer.time.open, tail, "trimming inside the open tail preserves every open time");
    assert!(producer.time.chunks.is_empty(), "a cutoff inside the open tail drops every closed time chunk");
    assert_eq!(producer.base, expected_base + remaining_chunks * CHUNK, "tail-only retention advances base by whole chunks");
    assert_eq!(producer.base + producer.len(), end_before, "tail-only retention preserves the absolute end");
    for (signal, (open, open_exact)) in producer.signals.iter().zip(&tails) {
        assert!(signal.chunks.is_empty(), "a cutoff inside the open tail drops every closed signal chunk");
        assert_eq!(&signal.open, open, "trimming preserves the signal's open values");
        assert_eq!(&signal.open_exact, open_exact, "trimming preserves the signal's open exact copy");
    }
    let tail_marks: Vec<_> = marks.iter().copied().filter(|&t| t >= tail[0]).collect();
    assert_eq!(producer.marks, tail_marks, "tail-only retention filters marks at the first kept time");
    let logic = producer.logic(SignalId(1)).expect("the logic trace");
    assert_eq!(logic.len(), 4, "the record's time trims leave the logic history whole");
    assert_eq!((logic.level_at(first_kept - dt), logic.level_at(first_kept), logic.level_at(tail[0] - dt * 0.5), logic.level_at(tail[1])),
        (None, Some(false), Some(true), Some(true)), "a level is its newest entry's, unknown before the first");
    assert_eq!(producer.trim_before(f64::INFINITY), 0, "even a later cutoff cannot drop an open tail");
    assert_eq!(producer.time.open, tail, "the open tail survives a cutoff beyond the newest point");

    let mut empty = TraceStore::new(9, &[true, false]);
    assert_eq!(empty.trim_before(1.0), 0, "trimming an empty store drops no chunks");
    assert_eq!(empty.keep_last(seconds), 0, "retention on an empty store does nothing");
    assert_eq!(empty.bytes(), 0, "an empty store holds no sample payload");
    assert_eq!(empty.index_at(1.0), None, "an empty store has no sample index");
}

/// Logic past the in-memory bound spills, never drops: the oldest sealed chunks go to the run's
/// file, their summaries stay (levels at their ends, newest edges, density), a consumer mirrors
/// the producer chunk for chunk, a view's ask brings one back (shared, equal to what spilled) and
/// withdrawing it lets go again; the file goes with the spill.
#[test]
fn logic_past_its_bound_spills_and_comes_back_on_a_views_ask() {
    let (id, other) = (SignalId(0), SignalId(5));
    // 1 us apart, but a quiet 3 ms after entry 5000 (inside the second chunk)
    let quiet = 5000;
    let edge = |i: usize| LogicEdge { t: i as f64 * 1e-6 + if i > quiet { 3e-3 } else { 0.0 }, high: i % 2 == 1 };
    let total = LOGIC_KEEP + 3 * LOGIC_CHUNK + 17;
    let mut producer = TraceStore::new(3, &[]);
    let mut consumer = TraceStore::default();
    let mut spill = LogicSpill::new();
    let (mut sent, mut spilled) = (0, 0);
    for burst in [LOGIC_CHUNK - 1, 1, 3 * LOGIC_CHUNK + 5, total - 4 * LOGIC_CHUNK - 5] {
        let (mut closed, mut pages) = (Vec::new(), Vec::new());
        let entries: Vec<_> = (sent..sent + burst).map(edge).collect();
        for &e in &entries {
            producer.push_logic(id, e, &mut closed);
        }
        producer.push_logic(other, edge(sent), &mut closed);
        spilled += producer.spill_logic(&mut spill, &[], &mut pages);
        let logic = vec![(id, Arc::from(entries)), (other, Arc::from(vec![edge(sent)]))];
        consumer.apply(&TraceDelta { generation: 3, from: 0, time: Arc::from(vec![]), values: vec![], closed: Vec::new(), marks: Vec::new(), logic, logic_closed: closed, logic_pages: pages, folds: Vec::new(), rewind: None });
        sent += burst;
        // the samples' record is empty: the logic keeps its own history
        producer.keep_last(1e-9);
        consumer.keep_last(1e-9);
    }
    let theirs = producer.logic(id).expect("the producer's trace");
    let away = theirs.len().saturating_sub(LOGIC_KEEP).div_ceil(LOGIC_CHUNK);
    assert_eq!((theirs.len(), theirs.spilled, spilled), (total, away, away * LOGIC_CHUNK), "every entry is kept; the chunks past the bound spilled, whole");
    assert!(theirs.chunks[..away].iter().all(Option::is_none) && theirs.chunks[away..].iter().all(Option::is_some), "the spilled chunks left memory, the rest stayed");
    assert_eq!(spill.bytes(), (away * (LOGIC_CHUNK * 8 + LOGIC_CHUNK / 8)) as u64, "each spilled chunk is written once: its times and a bit a level");
    assert_eq!(consumer.logic, producer.logic, "a consumer mirrors the producer's spills");
    let mine = consumer.logic(id).expect("the consumer's trace");
    assert!(mine.chunks.iter().zip(&theirs.chunks).all(|(a, b)| match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (a, b) => a.is_none() && b.is_none(),
    }), "every chunk in memory is the producer's, shared");
    let resident = total - away * LOGIC_CHUNK;
    assert!(resident <= LOGIC_KEEP, "the memory holds at most the bound ({resident})");
    assert_eq!(consumer.bytes(), (resident + 4) * size_of::<LogicEdge>(), "bytes count the entries in memory");

    // the summaries answer what a spilled chunk's ends, density and edges say
    let n = LOGIC_CHUNK;
    assert_eq!((mine.get(n), mine.get(n + 1), mine.get(2 * n - 1)), (Some(edge(n)), None, Some(edge(2 * n - 1))), "a spilled chunk keeps its first and last entries");
    assert_eq!((mine.level_at(edge(n + 7).t), mine.level_at(edge(2 * n - 1).t), mine.level_at(edge(total - 1).t)),
        (None, Some((2 * n - 1) % 2 == 1), Some((total - 1) % 2 == 1)), "inside a spilled chunk the level is unknown; at its last entry and in memory it is known");
    assert_eq!(mine.first_time(), Some(0.0), "the history starts at the first entry, spilled or not");
    let brute = |t0: f64, t1: f64, high: bool| (1..total).rev().map(edge).find(|e| e.t <= t1 && e.high == high).filter(|e| e.t >= t0).map(|e| e.t);
    for (t0, t1) in [(0.0, edge(3 * n - 1).t), (0.0, edge(n - 1).t), (edge(n).t, edge(2 * n - 1).t + 1e-7)] {
        for high in [true, false] {
            assert_eq!(mine.last_edge(t0, t1, |h| h == high), brute(t0, t1, high), "the newest {} edge in {t0}..{t1} from the summaries", if high { "rising" } else { "falling" });
        }
    }
    assert_eq!(mine.last_edge(0.0, edge(n + 5).t, |h| h), None, "the answer inside a spilled chunk before its newest edge is unknown");
    let in_quiet = edge(quiet).t + 1e-3;
    assert_eq!(mine.dense_until(0, 2e-6, 1.0), Some(edge(n - 1).t), "a dense stretch ends where a spilled chunk's quiet gap starts");
    assert_eq!(mine.dense_until(2 * n, 2e-6, edge(6 * n).t), Some(edge(7 * n - 1).t), "and runs on from spilled chunks into those in memory");
    let wanted: Vec<f64> = mine.spilled_in(in_quiet - 1e-4, in_quiet + 1e-4, 1e-4).collect();
    assert_eq!(wanted, [edge(n).t], "a window over the quiet stretch at 0.2 ms a column needs that spilled chunk");
    assert_eq!(mine.spilled_in(0.0, edge(away * n - 1).t, 1e-4).count(), 1, "only the chunk with a gap over half a column needs its detail");

    // a view's ask brings it back, shared, as it was; withdrawn, it goes again
    let mut pages = Vec::new();
    producer.load_logic(&mut spill, &[(id, edge(n).t)], &mut pages);
    assert_eq!(pages.len(), 1, "one chunk comes back");
    consumer.apply(&TraceDelta { generation: 3, from: 0, time: Arc::from(vec![]), values: vec![], closed: Vec::new(), marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: pages, folds: Vec::new(), rewind: None });
    let mine = consumer.logic(id).expect("the consumer's trace");
    let back = mine.chunks[1].as_ref().expect("the asked chunk is in memory");
    assert!(Arc::ptr_eq(back, producer.logic(id).and_then(|t| t.chunks[1].as_ref()).expect("the producer's")), "the chunk back is the producer's, shared");
    assert!(back.iter().enumerate().all(|(k, e)| *e == edge(n + k)), "it reads back as it spilled");
    assert_eq!((mine.level_at(edge(n + 7).t), mine.spilled), (Some((n + 7) % 2 == 1), away), "its levels are known again; it is still past the bound");
    let mut again = Vec::new();
    producer.load_logic(&mut spill, &[(id, edge(n).t)], &mut again);
    assert!(again.is_empty(), "an ask for a chunk back already reads nothing");
    producer.load_logic(&mut spill, &[], &mut again);
    consumer.apply(&TraceDelta { generation: 3, from: 0, time: Arc::from(vec![]), values: vec![], closed: Vec::new(), marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: again, folds: Vec::new(), rewind: None });
    assert!(consumer.logic(id).is_some_and(|t| t.chunks[1].is_none()) && consumer.logic == producer.logic, "withdrawn, it leaves memory again, mirrored");

    let path = spill.path().expect("the spill made its file").to_path_buf();
    assert!(cfg!(windows) || !path.exists(), "the file is anonymous: nothing is left behind even if the run never ends cleanly");
    drop(spill);
    assert!(!path.exists(), "the file goes with the run");
}

#[test]
fn a_record_past_its_budget_folds_its_past_keeping_span_extremes_and_continuity() {
    let dt = 1e-6;
    let positions = 12;
    let n = positions * CHUNK + 50;
    // a fast square wave with a wobble (exact copy kept) and a slow ramp with rare spikes
    let values = |i: usize| [if (i / 3) % 2 == 0 { 1.0 + (i % 7) as f64 * 0.01 } else { -1.0 }, i as f64 * 1e-3 + if i % 1013 == 0 { 50.0 } else { 0.0 }];
    let mut producer = TraceStore::new(9, &[true, false]);
    let mut consumer = TraceStore::default();
    let mut closed = Vec::new();
    let (mut pending_t, mut pending_v, mut from) = (Vec::new(), [Vec::new(), Vec::new()], 0);
    for i in 0..n {
        let (t, y) = (i as f64 * dt, values(i));
        producer.push(t, &y, &mut closed);
        pending_t.push(t);
        for (pending, value) in pending_v.iter_mut().zip(y) {
            pending.push(value as f32);
        }
        if (i + 1) % 1500 == 0 || i + 1 == n {
            let time: Arc<[f64]> = std::mem::take(&mut pending_t).into();
            let values = pending_v.iter_mut().map(|pending| Arc::from(std::mem::take(pending))).collect();
            let delta = TraceDelta { generation: 9, from, time, values, closed: std::mem::take(&mut closed), marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: Vec::new(), rewind: None };
            from += delta.time.len();
            consumer.apply(&delta);
        }
    }
    let whole = producer.clone();
    let per = producer.position_bytes();
    assert_eq!(per, CHUNK * (8 + 2 * 4 + 8) + 2 * whole.signals[0].chunks[0].build().len() * size_of::<MinMax>(), "a position's bytes: times, values, pyramids, the exact copy");
    let budget = 5 * per;
    let mut made = Vec::new();
    let folded = producer.fold(budget, &mut made);
    assert_eq!(made.len(), folded, "each fold is told, for the deltas");
    let kept = producer.time.chunks.len();
    assert!(kept * per <= budget && kept >= 4, "the closed positions fit the budget ({kept} kept)");
    assert_eq!(folded, positions - kept, "each fold makes one position of two");
    assert_eq!((producer.base + producer.len(), producer.time.open.len()), (whole.base + whole.len(), 50), "the deltas' continuity and the open tail hold");
    assert_eq!((producer.time.at(0), producer.time.at(producer.len() - 1)), (0.0, (n - 1) as f64 * dt), "the record keeps its whole span");
    assert!((1..producer.len()).all(|i| producer.time.at(i) > producer.time.at(i - 1)), "times still increase");
    assert!(producer.folds.windows(2).all(|w| w[0] >= w[1]) && producer.folds[kept - 1] == 0, "older positions are folded more, the newest whole: {:?}", producer.folds);
    let newest = (0..kept).rev().take_while(|&c| producer.folds[c] == 0).count();
    assert!(newest >= 2, "the newest positions stay whole ({newest})");
    for k in 0..2 {
        let (mine, theirs) = (&producer.signals[k].chunks, &whole.signals[k].chunks);
        assert!((0..newest).all(|j| Arc::ptr_eq(&mine[kept - 1 - j], &theirs[positions - 1 - j])), "whole positions are the same chunks");
    }
    // every folded position keeps each signal's extremes over the time it spans
    let index = |t: f64| (t / dt).round() as usize;
    for c in 0..kept - newest {
        let (first, last) = (producer.time.chunks[c][0], producer.time.chunks[c][CHUNK - 1]);
        let span = index(first)..=index(last);
        for k in 0..2 {
            let chunk = &producer.signals[k].chunks[c];
            let lo = chunk.y.iter().copied().fold(f32::INFINITY, f32::min);
            let hi = chunk.y.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let want_lo = span.clone().map(|i| values(i)[k] as f32).fold(f32::INFINITY, f32::min);
            let want_hi = span.clone().map(|i| values(i)[k] as f32).fold(f32::NEG_INFINITY, f32::max);
            assert_eq!((lo, hi), (want_lo, want_hi), "position {c} signal {k} keeps its lowest and highest values");
            let top = chunk.level(chunk.build().len().ilog(4) as usize).unwrap_or_default();
            assert!(!top.is_empty() && top.iter().all(|m| m.min >= want_lo && m.max <= want_hi), "its pyramid stays within them");
        }
        let exact = producer.signals[0].chunks[c].exact.as_ref().expect("the measured signal keeps its exact copy");
        assert!(exact.iter().zip(producer.signals[0].chunks[c].y.iter()).all(|(&e, &y)| e as f32 == y), "the exact copy folds with its values");
    }
    // a consumer holding the same chunks makes the producer's folds as its delta brings them
    let from = consumer.base + consumer.len();
    let values = vec![Arc::from(Vec::new()), Arc::from(Vec::new())];
    consumer.apply(&TraceDelta { generation: 9, from, time: Arc::from(vec![]), values, closed: Vec::new(), marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: made, rewind: None });
    assert_eq!((consumer.base + consumer.len(), &consumer.folds), (producer.base + producer.len(), &producer.folds), "the consumer folds as the producer did");
    assert_eq!(consumer.time, producer.time, "to the same times");
    assert!((0..kept).all(|c| (0..2).all(|k| Arc::ptr_eq(&consumer.signals[k].chunks[c], &producer.signals[k].chunks[c]))), "with the same chunks, shared, not made again");
    assert_eq!(consumer.fold(budget, &mut Vec::new()), 0, "and so is within the budget");
    assert_eq!(producer.fold(budget, &mut Vec::new()), 0, "within the budget nothing more folds");
}

#[test]
fn a_record_keeps_its_probes_across_folds_probe_changes_and_a_rewind() {
    // One position per simulated second. On these increasing ramps the two extremes of a block
    // are its endpoints, so the original value at every retained time is known even after a fold.
    fn append(store: &mut TraceStore, points: std::ops::Range<usize>) -> TraceDelta {
        let from = store.base + store.len();
        let (mut time, mut values, mut closed) = (Vec::new(), [Vec::new(), Vec::new(), Vec::new()], Vec::new());
        for i in points {
            let t = i as f64 / CHUNK as f64;
            let y = [t, t + 32.0, t + 64.0];
            store.push(t, &y, &mut closed);
            time.push(t);
            for (v, y) in values.iter_mut().zip(y) {
                v.push(y as f32);
            }
        }
        TraceDelta {
            generation: store.generation, from, time: time.into(), values: values.into_iter().map(Arc::from).collect(),
            closed, marks: Vec::new(), logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: Vec::new(), rewind: None,
        }
    }

    let out = Arc::new(Latest::new());
    let mut publisher = TracePublisher::new(out.clone());
    let mut publish = |producer: &mut TraceStore, mut d: TraceDelta, keep: &Keep| {
        producer.keep(keep, &mut d.folds);
        publisher.publish(d, keep);
        let (_, consumer) = out.latest().expect("the consumer published its store");
        assert_eq!((consumer.base, consumer.rewinds), (producer.base, producer.rewinds), "the consumer follows folds and rewinds");
        assert_eq!(consumer.time, producer.time, "the consumer keeps every retained time");
        assert_eq!(consumer.signals.len(), producer.signals.len(), "the consumer keeps every lane");
        for (mine, theirs) in consumer.signals.iter().zip(&producer.signals) {
            assert_eq!(mine.chunks.len(), theirs.chunks.len(), "signal {:?} has the producer's closed positions", mine.id);
            assert!(mine.chunks.iter().zip(&theirs.chunks).all(|(a, b)| Arc::ptr_eq(a, b)), "signal {:?} shares every closed chunk", mine.id);
            assert!(mine.open.iter().map(|v| v.to_bits()).eq(theirs.open.iter().map(|v| v.to_bits())), "signal {:?} has the same open values, including gaps", mine.id);
        }
    };

    let mut keep = Keep { span: f64::INFINITY, budget: 768 * 1024, whole: Some(Arc::from([true])), front: 1.0 };
    let mut producer = TraceStore::new(1, &[false, true, true]);
    let mut full = TraceStore::default();
    for p in 0..16 {
        let d = append(&mut producer, p * CHUNK..(p + 1) * CHUNK);
        full.apply(&d);
        publish(&mut producer, d, &keep);
        // Fold the all-signals reference to the same number of positions, hence the same times.
        let budget = producer.time.chunks.len() * full.position_bytes();
        full.fold(budget, &mut Vec::new());
        assert!(producer.bytes() <= keep.budget, "the closed positions fit the byte budget after position {p}");
    }
    assert!(producer.folds.iter().any(|&level| level > 0), "the small budget exercised a fold");
    assert_eq!(producer.time, full.time, "the byte comparison covers exactly the same points");
    assert!(producer.bytes() * 3 < full.bytes() * 2, "thinning uses {} bytes for {} bytes of whole signals", producer.bytes(), full.bytes());
    let last = producer.time.at(producer.len() - 1);
    assert_eq!((producer.time.at(0), last), (0.0, 16.0 - 1.0 / CHUNK as f64), "folding preserves both ends of the take");
    let cutoff = last - keep.front;
    let old = producer.time.chunks.partition_point(|times| times[CHUNK - 1] < cutoff);
    assert!(old > 0 && old < producer.time.chunks.len(), "both history and live front are present");
    for (p, times) in producer.time.chunks.iter().enumerate() {
        for (s, signal) in producer.signals.iter().enumerate() {
            let absent = s != 0 && p < old;
            assert_eq!(is_absent(&signal.chunks[p]), absent, "signal {s} position {p} shares the absent chunk exactly behind its front");
            assert!(times.iter().enumerate().all(|(k, &t)| {
                let value = signal.at(p * CHUNK + k);
                if absent { value.is_nan() } else { value == (t + s as f64 * 32.0) as f32 }
            }), "signal {s} position {p} reads the original ramp or its gap");
        }
    }

    let present_from = producer.time.chunks[old][0];
    keep.whole = Some(Arc::from([true, true]));
    for p in 16..24 {
        let d = append(&mut producer, p * CHUNK..(p + 1) * CHUNK);
        publish(&mut producer, d, &keep);
    }
    assert!((0..producer.len()).all(|i| {
        let t = producer.time.at(i);
        let value = producer.signals[1].at(i);
        if t < present_from { value.is_nan() } else { value == (t + 32.0) as f32 }
    }), "a newly whole signal keeps its gap and every value from its surviving front onward");

    let p = producer.signals[2].chunks.iter().rposition(is_absent).expect("a thinned position to rewind into");
    let t = producer.time.chunks[p][CHUNK / 2];
    let rewind = producer.rewind_producer(t, &mut LogicSpill::new());
    let mut d = append(&mut producer, 0..0);
    d.rewind = Some(rewind);
    publish(&mut producer, d, &keep);
    assert_eq!((producer.time.chunks.len(), producer.time.open.len()), (p, CHUNK / 2 + 1), "the rewind reopens the thinned position through the cut");
    assert_eq!(producer.time.open.last(), Some(&t), "the cut ends at the rewind time");
    assert!(producer.signals[2].open.iter().all(|v| v.is_nan()), "the reopened absent chunk reads as a gap");

    let next = (t * CHUNK as f64) as usize + 1;
    let d = append(&mut producer, next..next + 3 * CHUNK);
    publish(&mut producer, d, &keep);
    assert!(producer.bytes() <= keep.budget + full.position_bytes(), "the record including its open tail fits the budget plus one position");
    let cutoff = producer.time.at(producer.len() - 1) - keep.front;
    let old = producer.time.chunks.partition_point(|times| times[CHUNK - 1] < cutoff);
    assert!(producer.signals[2].chunks[..old].iter().all(is_absent), "the refilled position thins again as the front passes it");

    keep.whole = Some(Arc::from([true]));
    let d = append(&mut producer, 0..0);
    publish(&mut producer, d, &keep);
    assert!(producer.signals[1].chunks[..old].iter().all(is_absent), "removing a probe revisits positions already thinned for the old mask");
}

/// A batch of rows (`TraceStore::push_rows`, a round's merged points) stores what its points
/// pushed in turn do: the same tails, chunks (an exact copy among them), positions and closed
/// chunks in order, in batches ending anywhere, one across a chunk's end; the values it also
/// hands a delta (`push_rows_into`) are the store's own, a gap where it wrote none; and a store
/// that leaves its pyramids to a background job (`defer_pyramids`) closes its chunks with none
/// built, each listed once for the job (`take_unbuilt`), the shared absent chunk never.
#[test]
fn rows_pushed_at_once_store_as_points_pushed_in_turn() {
    let n = CHUNK + 500;
    let y = |i: usize, s: usize| (i * (s + 3)) as f64 * 1e-3 - s as f64;
    let t: Vec<f64> = (0..n).map(|i| i as f64 * 1e-6).collect();
    let (mut one, mut rows) = (TraceStore::new(5, &[true, false, false]), TraceStore::new(5, &[true, false, false]));
    let (mut one_closed, mut rows_closed) = (Vec::new(), Vec::new());
    let mut also = vec![Vec::new(); 3];
    rows.defer_pyramids(true);
    for (i, &ti) in t.iter().enumerate() {
        one.push(ti, &[y(i, 0), y(i, 1), y(i, 2)], &mut one_closed);
    }
    let mut from = 0;
    for size in [1, 80, 33].into_iter().cycle() {
        if from == n {
            break;
        }
        let to = (from + size).min(n);
        rows.push_rows_into(&t[from..to], |r, s| y(from + r, s), &mut rows_closed, &mut also, from);
        from = to;
    }
    assert_eq!(one_closed.len(), 3, "the points close one chunk of each signal");
    let listed = |unbuilt: &[Arc<Chunk>], closed: &[&(SignalId, Arc<Chunk>)]| {
        unbuilt.len() == closed.len() && unbuilt.iter().zip(closed).all(|(u, (_, c))| Arc::ptr_eq(u, c) && c.pyramid().is_none())
    };
    assert!(listed(&rows.take_unbuilt(), &rows_closed.iter().collect::<Vec<_>>()), "the deferring store's closed chunks: no pyramid, each listed for the job");
    assert!(rows_closed == one_closed, "the same chunks closed, in the same order");
    assert!(rows == one, "the same store: tails, chunks, exact copies and positions");

    // the store writes only its fine signals (`TraceStore::fine`), changed between any two
    // points: signal 0 alone (the others past the mask), then 1 and 2 over a whole position, then
    // every one again; each reads its values where written and a gap elsewhere, its exact copy
    // alike, and a position it has no point in is the shared absent chunk
    let stages: [(Option<Arc<[bool]>>, usize); 3] = [(Some(Arc::from([true])), 700), (Some(Arc::from([false, true, true])), 2 * CHUNK + 3108), (None, 500)];
    let written = |i: usize, s: usize| {
        let mut end = n;
        if i < end {
            return true;
        }
        for (fine, len) in &stages {
            end += len;
            if i < end {
                return fine.as_ref().is_none_or(|m| m.get(s).copied().unwrap_or(false));
            }
        }
        unreachable!("point {i} is pushed in a stage")
    };
    let mut from = n;
    for (fine, len) in &stages {
        (one.fine, rows.fine) = (fine.clone(), fine.clone());
        let to = from + len;
        let t: Vec<f64> = (from..to).map(|i| i as f64 * 1e-6).collect();
        for (r, &ti) in t.iter().enumerate() {
            one.push(ti, &[y(from + r, 0), y(from + r, 1), y(from + r, 2)], &mut one_closed);
        }
        for batch in t.chunks(80).enumerate() {
            let at = from + batch.0 * 80;
            rows.push_rows_into(batch.1, |r, s| y(at + r, s), &mut rows_closed, &mut also, at);
        }
        from = to;
    }
    let fresh: Vec<_> = rows_closed[3..].iter().filter(|(_, c)| !is_absent(c)).collect();
    assert!(fresh.len() < rows_closed.len() - 3 && listed(&rows.take_unbuilt(), &fresh), "the shared absent chunk is never listed for a pyramid");
    let closed_alike = rows_closed.len() == one_closed.len() && rows_closed.iter().zip(&one_closed).all(|(a, b)| a.0 == b.0 && same_chunk(&a.1, &b.1));
    assert!(closed_alike && same_bits(&rows, &one), "a fine mask stores the same in rows as in points, bit for bit");
    for s in 0..3 {
        for i in 0..from {
            let (v, ok) = (one.signals[s].at(i), written(i, s));
            assert!(if ok { v == y(i, s) as f32 } else { v.is_nan() }, "signal {s} at point {i}: {v} where it was {}written", if ok { "" } else { "not " });
        }
        for (p, chunk) in one.signals[s].chunks.iter().enumerate() {
            let none = !(p * CHUNK..(p + 1) * CHUNK).any(|i| written(i, s));
            assert_eq!(is_absent(chunk), none, "signal {s} position {p} is absent exactly when it has no point");
            if let Some(exact) = chunk.exact.as_deref() {
                assert!(exact.iter().enumerate().all(|(k, &e)| if written(p * CHUNK + k, s) { e == y(p * CHUNK + k, s) } else { e.is_nan() }), "signal {s} position {p}'s exact copy has its gaps");
            }
        }
    }
    for (s, v) in also.iter().enumerate() {
        let alike = (0..from).all(|i| v.get(i).copied().unwrap_or(f32::NAN).to_bits() == one.signals[s].at(i).to_bits());
        assert!(alike && v.len() <= from, "signal {s}: the values handed on are the store's, bit for bit, a gap where it wrote none");
    }
    assert!(one.signals[0].chunks.iter().any(is_absent), "a stage covered a whole position without signal 0");
    let k = from - one.time.chunks.len() * CHUNK;
    assert!(one.signals[0].open.len() == k && one.signals[0].open_exact.as_ref().is_some_and(|e| e.len() == k), "a signal written again fills its gap up to the point it is written at");
}

/// Two chunks alike bit for bit (a gap's NaN as a gap's): values, pyramid and exact copy, and
/// both the shared absent chunk or neither.
fn same_chunk(a: &Arc<Chunk>, b: &Arc<Chunk>) -> bool {
    let f = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    let d = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    is_absent(a) == is_absent(b) && f(&a.y) == f(&b.y) && a.exact.as_deref().map(d) == b.exact.as_deref().map(d)
        && a.build().len() == b.build().len() && a.build().iter().zip(b.build()).all(|(p, q)| p.min.to_bits() == q.min.to_bits() && p.max.to_bits() == q.max.to_bits())
}

/// Two stores alike bit for bit: times, and every signal's closed chunks and open tail.
fn same_bits(a: &TraceStore, b: &TraceStore) -> bool {
    let f = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    let d = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    a.time == b.time && a.signals.len() == b.signals.len() && a.signals.iter().zip(&b.signals).all(|(s, t)| {
        s.chunks.len() == t.chunks.len() && s.chunks.iter().zip(&t.chunks).all(|(x, y)| same_chunk(x, y))
            && f(&s.open) == f(&t.open) && s.open_exact.as_deref().map(d) == t.open_exact.as_deref().map(d)
    })
}
