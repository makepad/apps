use fab_elec_sim::trace::{Rewind, TraceDelta, TraceStore, CHUNK};
use fab_elec_view::trace::colcache::{CacheStats, ColumnCache};
use fab_elec_view::trace::columns::{Column, Columns};
use std::ops::Range;
use std::sync::Arc;

fn append(store: &mut TraceStore, points: Range<usize>) {
    let mut closed = Vec::new();
    for i in points {
        // This sample rounds onto the next pixel boundary when stored as f32.
        let t = if i == CHUNK - 24 { (i as f64).next_down() } else { i as f64 };
        let y = (i % 17) as f64 - (i % 11) as f64;
        store.push(t, &[y, -y], &mut closed);
    }
}

fn check(cache: &mut ColumnCache, store: &TraceStore, signal: usize, window: (f64, f64, u32), why: &str) {
    let (t0, t1, cols) = window;
    let reference = Columns::reduce_trace(store, signal, t0, t1, cols);
    assert_eq!(cache.columns(store, signal, t0, t1, cols), &reference, "{why}: columns and neighbours at {} points, {cols} pixels", store.len());
}

fn check_full(cache: &mut ColumnCache, store: &TraceStore, signal: usize, window: (f64, f64, u32), why: &str) {
    let old = cache.stats();
    check(cache, store, signal, window, why);
    assert_eq!(cache.stats(), CacheStats { full: old.full + 1, ..old }, "{why}: one full reduction");
}

#[test]
fn a_lane_tracks_appends_and_invalidates_on_edits() {
    let mut store = TraceStore::new(1, &[false, false]);
    let t0 = (CHUNK - 48) as f64;
    let t1 = t0 + 96.0;
    let mut lanes = [(128, ColumnCache::new()), (4, ColumnCache::default())];
    append(&mut store, 0..CHUNK - 52);
    for (cols, cache) in &mut lanes {
        check_full(cache, &store, 0, (t0, t1, *cols), "first frame beyond the record");
    }

    // The narrow lane crosses its sparse limit at 33 visible samples. Both see a chunk seal
    // and the first following neighbour arrive exactly at the excluded window end.
    let ends = [
        CHUNK - 49, CHUNK - 24, CHUNK - 23, CHUNK - 16, CHUNK - 15,
        CHUNK - 1, CHUNK, CHUNK + 48, CHUNK + 49, CHUNK + 52,
    ];
    for (batch, end) in ends.into_iter().enumerate() {
        let from = store.len();
        append(&mut store, from..end);
        for (cols, cache) in &mut lanes {
            check(cache, &store, 0, (t0, t1, *cols), "append");
            assert_eq!(
                cache.stats(), CacheStats { full: 1, partial: batch as u64 + 1, reused: batch as u64 },
                "only the suffix is reduced at {end} points, {cols} pixels"
            );
            check(cache, &store, 0, (t0, t1, *cols), "playhead-only frame");
            assert_eq!(cache.stats().reused, batch as u64 + 1, "unchanged frame reuses {cols} columns");
        }
    }

    // Append exactly as many samples as the fold removes. Length alone cannot detect the edit.
    let from = store.len();
    append(&mut store, from..from + CHUNK);
    let mut consumer = store.clone();
    let mut folds = Vec::new();
    assert_eq!(store.fold(store.position_bytes(), &mut folds), 1, "two closed chunks fold into one");
    let delta = TraceDelta {
        generation: consumer.generation,
        from: consumer.base + consumer.len(),
        time: Arc::from([]),
        values: vec![Arc::from([]), Arc::from([])],
        closed: Vec::new(),
        marks: Vec::new(),
        logic: Vec::new(),
        logic_closed: Vec::new(),
        logic_pages: Vec::new(),
        folds,
        rewind: None,
    };
    consumer.apply(&delta);
    assert_eq!((store.folds_made, consumer.folds_made), (1, 1), "creating and applying a fold each advance the stamp once");
    for ((cols, cache), folded) in lanes.iter_mut().zip([&store, &consumer]) {
        assert_eq!(folded.len(), from, "the fold restores the cached length");
        check_full(cache, folded, 0, (t0, t1, *cols), "fold in a store of unchanged length");
    }

    let cache = &mut lanes[1].1;
    let window = (t0, t1, 4);
    let last = store.time.at(store.len() - 1) as usize;
    store.rewind(&Rewind { t: (last - 1) as f64, resent: Vec::new() });
    append(&mut store, last..last + 1);
    check_full(cache, &store, 0, window, "rewind and refill");
    store.generation += 1;
    check_full(cache, &store, 0, window, "new generation");
    let moved = (t0 + 1.0, t1 + 1.0, 4);
    check_full(cache, &store, 0, moved, "pan");
    let resized = (moved.0, moved.1, 5);
    check_full(cache, &store, 0, resized, "resize");
    check_full(cache, &store, 1, resized, "another signal");
    store.trim_before(t1 + CHUNK as f64);
    check_full(cache, &store, 1, resized, "shorter store");
    cache.clear();
    check_full(cache, &store, 1, resized, "explicit clear");
}

#[test]
fn a_density_change_preserves_nan_payloads() {
    let mut store = TraceStore::new(1, &[false]);
    let mut closed = Vec::new();
    let mut cache = ColumnCache::new();
    for i in 0..24 {
        let y = if i < 10 { f32::from_bits(0x7fc0_0001 + i) } else { i as f32 };
        store.push(f64::from(i), &[f64::from(y)], &mut closed);
    }
    let first = cache.columns(&store, 0, 0.0, 30.0, 3).cols_data[0];
    store.push(24.0, &[24.0], &mut closed);
    let reference = Columns::reduce_trace(&store, 0, 0.0, 30.0, 3);
    let actual = cache.columns(&store, 0, 0.0, 30.0, 3);
    let bits = |c: &Column| [c.x0, c.x1, c.y0, c.y1, c.lo, c.hi].map(f32::to_bits);
    assert_eq!(actual.cols_data.len(), reference.cols_data.len(), "every dense column is present");
    for (a, b) in actual.cols_data.iter().zip(&reference.cols_data) {
        assert_eq!((bits(a), a.count), (bits(b), b.count), "the cached column matches the dense reduction bit for bit");
    }
    let needs_full = bits(&first) != bits(&reference.cols_data[0]);
    assert_eq!(
        cache.stats(), CacheStats { full: 1 + u64::from(needs_full), partial: u64::from(!needs_full), reused: 0 },
        "only a changed NaN payload needs a full reduction"
    );
}
