//! The live seam: the trace store's chunks, pyramid and deltas (C10), and the `Latest`
//! publication.

use fab_elec_sim::live::Latest;
use fab_elec_sim::trace::{Keep, TraceDelta, TracePublisher, TraceStore, CHUNK, PUBLISH_TURNS};
use std::sync::Arc;

/// A producer appends 3.3 chunks of two signals and ships them in uneven deltas; the consumer's
/// store equals the producer's, closed chunks are shared rather than copied, the pyramid is the
/// min/max of its samples, and a new generation starts the consumer over.
#[test]
fn trace_deltas_rebuild_the_store() {
    let n = CHUNK * 3 + CHUNK / 3;
    let mut prod = TraceStore::new(1, &[true, false]);
    let mut cons = TraceStore::default();
    let (mut pending_t, mut pending_v, mut closed) = (Vec::new(), vec![Vec::new(), Vec::new()], Vec::new());
    let (mut pending_marks, mut marks) = (Vec::new(), Vec::new());
    let mut from = 0;
    let cuts = [1usize, 700, CHUNK, CHUNK + 5, 2 * CHUNK + 17, n];
    for i in 0..n {
        let t = i as f64 * 1e-6 + (i as f64 * 0.37).sin() * 1e-8;
        let y = [(t * 1e5).sin(), (i % 97) as f64 - 48.0];
        prod.push(t, &y, &mut closed);
        pending_t.push(t);
        if i % 1000 == 3 {
            pending_marks.push(t);
            marks.push(t);
        }
        for (p, v) in pending_v.iter_mut().zip(y) {
            p.push(v as f32);
        }
        if cuts.contains(&(i + 1)) {
            let d = TraceDelta {
                generation: 1,
                from,
                time: std::mem::take(&mut pending_t).into(),
                values: pending_v.iter_mut().map(|p| Arc::from(std::mem::take(p))).collect(),
                closed: std::mem::take(&mut closed),
                marks: std::mem::take(&mut pending_marks),
                logic: Vec::new(),
                logic_closed: Vec::new(),
                logic_pages: Vec::new(),
                folds: Vec::new(),
                rewind: None,
            };
            from += d.time.len();
            cons.apply(&d);
        }
    }
    assert_eq!(cons.len(), n, "consumer length");
    assert_eq!(cons.time, prod.time, "time axis");
    assert_eq!(cons.marks, marks, "event marks arrive in order, exact");
    for (a, b) in cons.signals.iter().zip(&prod.signals) {
        assert_eq!(a.open, b.open, "open tails");
        assert_eq!(a.chunks.len(), 3, "closed chunks");
        for (ca, cb) in a.chunks.iter().zip(&b.chunks) {
            assert!(Arc::ptr_eq(ca, cb), "a closed chunk travels as the producer's Arc");
        }
    }
    assert!(prod.signals[0].chunks[0].exact.is_some() && prod.signals[1].chunks[0].exact.is_none(), "exact copies where asked");
    let c = &prod.signals[0].chunks[1];
    for k in 0..6 {
        let lv = c.level(k).expect("a store with no pool builds each pyramid at close");
        let w = 4usize.pow(k as u32 + 1);
        assert_eq!(lv.len(), CHUNK / w, "level {k} size");
        for (b, mm) in lv.iter().enumerate() {
            let s = &c.y[b * w..(b + 1) * w];
            let (lo, hi) = (s.iter().copied().fold(f32::INFINITY, f32::min), s.iter().copied().fold(f32::NEG_INFINITY, f32::max));
            assert!(mm.min == lo && mm.max == hi, "level {k} bin {b}");
        }
    }
    let restart = TraceDelta { generation: 2, from: 0, time: Arc::from(vec![0.0]), values: vec![Arc::from(vec![1.0f32]), Arc::from(vec![2.0f32])], closed: Vec::new(), marks: vec![0.0], logic: Vec::new(), logic_closed: Vec::new(), logic_pages: Vec::new(), folds: Vec::new(), rewind: None };
    cons.apply(&restart);
    assert!(cons.generation == 2 && cons.len() == 1 && cons.signals[1].at(0) == 2.0, "a new generation starts over");
    assert_eq!(cons.marks, [0.0], "a new generation starts its marks over");

    let latest = Latest::new();
    assert!(latest.latest().is_none() && latest.published() == 0, "empty before the first publication");
    assert!(latest.publish(Arc::new(1)).is_none());
    assert_eq!(latest.publish(Arc::new(2)).as_deref(), Some(&1), "the replaced value goes back to the producer");
    latest.publish(Arc::new(3));
    let (seq, v) = latest.latest().unwrap();
    assert!(seq == 3 && *v == 3 && latest.published() == 3, "readers see the newest and its number");
}

/// The producer merges and publishes the consumer's store (the UI thread merges nothing): 2.5
/// chunks of two signals in deltas of 97 points, a rolling record of about 3000 points. Every
/// publication equals a store that applied the same deltas and kept the same record, closed chunks
/// shared; a reader that takes every second publication, and a ghost kept from a third of the way
/// on, cost a few copies of the store, the rest catch up by deltas; the backlog stays within the
/// turns, and a held store never changes.
#[test]
fn the_producer_publishes_the_consumer_store() {
    let (n, step, record) = (CHUNK * 2 + CHUNK / 2, 97, 3000e-6);
    let out = Arc::new(Latest::new());
    let mut publisher = TracePublisher::new(out.clone());
    let mut prod = TraceStore::new(1, &[false, false]);
    let mut cons = TraceStore::default();
    let (mut pending_t, mut pending_v, mut closed) = (Vec::new(), vec![Vec::new(), Vec::new()], Vec::new());
    let (mut from, mut k) = (0, 0u64);
    let (mut reader, mut ghost): (Option<Arc<TraceStore>>, Option<(Arc<TraceStore>, usize, usize)>) = (None, None);
    let deltas = n.div_ceil(step) as u64;
    for i in 0..n {
        let t = i as f64 * 1e-6;
        let y = [(t * 1e5).sin(), (i % 89) as f64];
        prod.push(t, &y, &mut closed);
        pending_t.push(t);
        for (p, v) in pending_v.iter_mut().zip(y) {
            p.push(v as f32);
        }
        if (i + 1) % step != 0 && i + 1 != n {
            continue;
        }
        let d = TraceDelta {
            generation: 1,
            from,
            time: std::mem::take(&mut pending_t).into(),
            values: pending_v.iter_mut().map(|p| Arc::from(std::mem::take(p))).collect(),
            closed: std::mem::take(&mut closed),
            marks: Vec::new(),
            logic: Vec::new(),
            logic_closed: Vec::new(),
            logic_pages: Vec::new(),
            folds: Vec::new(),
            rewind: None,
        };
        from += d.time.len();
        cons.apply(&d);
        cons.keep_last(record);
        publisher.publish(d, &Keep::last(record));
        k += 1;
        let (seq, snap) = out.latest().expect("a publication");
        assert_eq!(seq, k, "one publication per delta");
        assert!(snap.base == cons.base && snap.time == cons.time, "publication {k}: the consumer's record, base {} of {}", snap.base, cons.base);
        for (a, b) in snap.signals.iter().zip(&cons.signals) {
            assert_eq!(a.open, b.open, "publication {k}: open tails");
            assert!(a.chunks.len() == b.chunks.len() && a.chunks.iter().zip(&b.chunks).all(|(x, y)| Arc::ptr_eq(x, y)), "publication {k}: closed chunks shared");
        }
        assert!(publisher.backlog() <= PUBLISH_TURNS, "publication {k}: backlog {} within the turns", publisher.backlog());
        if k % 2 == 0 {
            reader = Some(snap.clone());
        }
        if k == deltas / 3 {
            ghost = Some((snap.clone(), snap.base, snap.len()));
        }
    }
    assert!(cons.base > 0, "the record trimmed whole chunks");
    let (g, base, len) = ghost.expect("the ghost was taken");
    assert!(g.base == base && g.len() == len, "a held store never changes");
    assert!(publisher.copies() <= 3, "{} copies over {k} publications: growing to the turns, and once for the ghost", publisher.copies());
    drop(reader);
}
