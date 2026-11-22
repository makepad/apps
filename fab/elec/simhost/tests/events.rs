//! The firmware event store: fed in batches that a full feed refuses rather than drops, drained
//! without waiting, every event kept until each reader has read it, and freed in whole chunks once
//! read and outside the visible span of the current run; a void batch drops the newest events,
//! and makes room in the feed like any other.

use fab_elec_simhost::events::{EventLog, EventSource, FwEvent, CHUNK};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Even frames come from the image, odd ones from the trace.
fn source(k: &u32) -> EventSource {
    if k % 2 == 0 { EventSource::Image } else { EventSource::Trace }
}

/// Event `k` at `k` ms.
fn event(k: u32) -> (f64, u32) {
    (f64::from(k) * 1e-3, k)
}

fn fw(k: u32) -> FwEvent<u32> {
    let (t, frame) = event(k);
    FwEvent { t, source: source(&frame), frame }
}

fn frames<'a>(events: impl Iterator<Item = &'a FwEvent<u32>>) -> Vec<u32> {
    events.map(|e| e.frame).collect()
}

/// A reader that pauses while the device runs keeps every event, in order, none missed; one that
/// keeps up reads each once; a full feed refuses a batch, which stays with the device intact until
/// the host drains, and every offer wakes the host; the visible span shows only its length; once
/// both have read, the store frees whole chunks down to what the span shows; an unregistered
/// reader holds nothing.
#[test]
fn a_paused_reader_misses_no_event_and_read_chunks_leave() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake = wakes.clone();
    let (mut log, feed) = EventLog::new(10.5e-3, source, Arc::new(move || {
        wake.fetch_add(1, Ordering::Relaxed);
    }));
    let (paused, live) = (log.store.register(), log.store.register());
    // more one-event batches than the feed holds
    let n = 10 * CHUNK as u32;
    let (mut seen, mut refused) = (Vec::new(), 0);
    for k in 0..n {
        let mut batch = vec![event(k)];
        if !feed.offer(&mut batch) {
            // the device keeps the batch and holds; the host's step drains, the device offers again
            refused += 1;
            assert!(log.full() && batch == [event(k)], "a refused batch stays with the device, intact, and the feed is full");
            log.drain();
            assert!(!log.full(), "a drained feed has room");
            seen.extend(frames(log.store.read(live)));
            assert!(feed.offer(&mut batch) && batch.is_empty(), "the drained feed takes the batch");
        }
    }
    assert!(refused > 0, "the feed filled");
    assert_eq!(wakes.load(Ordering::Relaxed), n as usize + refused, "every offer wakes the host");
    log.drain();
    assert_eq!(log.drain(), 0, "an empty feed gives nothing, without waiting");
    let store = &mut log.store;
    seen.extend(frames(store.read(live)));
    assert_eq!(seen, (0..n).collect::<Vec<_>>(), "the reader that kept up read every event once, in order");
    assert_eq!(store.len(), n as usize, "the paused reader holds every event it has not read");
    assert_eq!(frames(store.visible()), (n - 11..n).collect::<Vec<_>>(), "the span shows the newest 10.5 ms");
    assert_eq!(store.visible().take(2).map(|e| e.source).collect::<Vec<_>>(), [EventSource::Trace, EventSource::Image],
        "with each event's source (event {} is odd)", n - 11);

    assert_eq!(frames(store.read(paused)), (0..n).collect::<Vec<_>>(), "the paused reader gets every event, in order, none missed");
    assert!(frames(store.read(paused)).is_empty(), "and each only once");
    store.push(fw(n));
    assert!(store.len() <= CHUNK + 12, "read chunks outside the span are freed: {} held", store.len());
    assert_eq!(frames(store.visible()), (n - 10..=n).collect::<Vec<_>>(), "the span still shows its length");
    assert_eq!(store.next(), u64::from(n) + 1, "sequence numbers run on across frees");

    let oldest = (store.next() - store.len() as u64) as u32;
    let late = store.register();
    store.unregister(paused);
    let end = n + 2 * CHUNK as u32;
    for k in n + 1..=end {
        store.push(fw(k));
    }
    assert_eq!(frames(store.read(late)), (oldest..=end).collect::<Vec<_>>(),
        "a reader registered later reads from the oldest event held then, none missed");
    assert!(store.len() > 2 * CHUNK, "the reader that stopped reading still holds what it has not read: {}", store.len());
    store.unregister(live);
    store.push(fw(end + 1));
    assert!(store.len() <= CHUNK + 12, "once no reader needs them, read chunks outside the span leave: {} held", store.len());
}

/// A new run (its time from zero again) shows only its own events in the span and in time queries,
/// while the last run's unread events stay for their reader, in order, before the new run's; once
/// read, the last run's chunks leave although their times are later than the new run's. A batch
/// carries several events, each with its source. A feed whose log is gone takes and discards, so
/// the device never waits for a host that no longer reads.
#[test]
fn a_new_run_shows_only_its_own_span_and_a_dropped_log_never_holds_the_device() {
    let (mut log, feed) = EventLog::new(10.5e-3, source, Arc::new(|| {}));
    let reader = log.store.register();
    let first = 2 * CHUNK as u32 + 5;
    for k in (0..first).step_by(3) {
        let mut batch: Vec<_> = (k..(k + 3).min(first)).map(event).collect();
        assert!(feed.offer(&mut batch), "the feed has room");
    }
    log.drain();
    assert_eq!(frames(log.store.visible()), (first - 11..first).collect::<Vec<_>>(), "the first run's span");

    log.store.restart();
    assert_eq!(log.store.run_start(), u64::from(first), "the new run starts at the next event");
    let second: Vec<(f64, u32)> = (0..20).map(|k| (f64::from(k) * 1e-3, 100_000 + k)).collect();
    assert!(feed.offer(&mut second.clone()), "the feed has room");
    log.drain();
    assert_eq!(frames(log.store.visible()), (100_009..100_020).collect::<Vec<_>>(), "the span shows the new run's newest 10.5 ms only");
    assert_eq!(frames(log.store.span(0.0, 10.0)), (100_000..100_020).collect::<Vec<_>>(), "a time query covers the new run only");
    assert_eq!(log.store.span(0.0, 10.0).map(|e| e.source).take(2).collect::<Vec<_>>(), [EventSource::Image, EventSource::Trace],
        "each event of a batch has its own source");
    let all: Vec<u32> = (0..first).chain(100_000..100_020).collect();
    assert_eq!(frames(log.store.read(reader)), all, "the reader gets the last run's events, then the new run's, in order");
    log.store.set_length(10.5e-3);
    assert_eq!(log.store.len(), 20, "once read, the last run's chunks leave; the new run's are kept for its span");

    drop(log);
    let mut batch = vec![event(1)];
    assert!(feed.offer(&mut batch) && batch.is_empty(), "with the log gone the feed takes and discards");
}

/// A device's earlier copy takes over (a live run went back): its first batch voids the stream's
/// newest events, before its own. The store drops them, across a chunk's edge; a reader that had
/// read past them reads on from the new end, the copy's events; the span ends at the newest kept
/// one; a void with no events goes too; and each rewind is counted. In a new run a void longer
/// than the run drops the run's events only, and the next event opens the run again.
#[test]
fn a_void_batch_drops_the_newest_events_and_readers_read_on() {
    let (mut log, feed) = EventLog::new(10.5e-3, source, Arc::new(|| {}));
    let reader = log.store.register();
    let n = CHUNK as u32 + 40;
    for k in (0..n).step_by(4) {
        let mut batch: Vec<_> = (k..k + 4).map(event).collect();
        assert!(feed.offer(&mut batch), "the feed has room");
    }
    log.drain();
    assert_eq!(frames(log.store.read(reader)), (0..n).collect::<Vec<_>>(), "the reader reads every event");

    let mut batch: Vec<_> = (n - 60..n - 50).map(event).collect();
    assert!(feed.offer_voiding(60, &mut batch) && batch.is_empty(), "the feed takes a batch with void events");
    log.drain();
    assert_eq!((log.store.len(), log.store.rewinds()), (n as usize - 50, 1), "the 60 newest events go, the copy's 10 come");
    assert_eq!(frames(log.store.read(reader)), (n - 60..n - 50).collect::<Vec<_>>(), "the reader reads on from the new end");
    assert_eq!(frames(log.store.visible()), (n - 61..n - 50).collect::<Vec<_>>(), "the span ends at the newest kept event");
    assert!(feed.offer_voiding(5, &mut Vec::new()), "a void with no events goes");
    log.drain();
    assert_eq!((log.store.len(), log.store.rewinds()), (n as usize - 55, 2), "five more go");
    assert!(frames(log.store.read(reader)).is_empty(), "the reader has nothing new");

    log.store.restart();
    let mut batch: Vec<_> = (0..3).map(|k| (f64::from(k) * 1e-3, 200_000 + k)).collect();
    assert!(feed.offer(&mut batch), "the feed has room");
    log.drain();
    assert!(feed.offer_voiding(10, &mut vec![(0.0, 300_000)]), "the feed has room");
    log.drain();
    assert_eq!(log.store.rewinds(), 3, "the rewind is counted");
    assert_eq!(frames(log.store.span(0.0, 10.0)), [300_000], "the void stops at the run's start; the copy's event is the run's");
    assert_eq!(frames(log.store.read(reader)), [300_000], "the reader reads the run's one event");

    // a feed full of batches that only void: draining them makes room, so it is no longer full
    while feed.offer_voiding(1, &mut Vec::new()) {}
    assert!(log.full(), "a void the full feed refused waits with the device");
    assert_eq!(log.drain(), 0, "void-only batches bring no events");
    assert!(!log.full(), "a feed drained of void-only batches has room");
}

/// A run that starts mid-chunk leaves that chunk part full: a reader whose next event lies in a
/// later chunk reads from it, not from where whole chunks would put it, and so does the span.
#[test]
fn a_reader_reads_from_its_place_after_a_run_started_mid_chunk() {
    let (mut log, feed) = EventLog::new(1e3, source, Arc::new(|| {}));
    // one reader holds everything, so the first run's part-full chunk stays
    let (_held, reader) = (log.store.register(), log.store.register());
    let offer = |ks: std::ops::Range<u32>| {
        let mut batch: Vec<_> = ks.map(event).collect();
        assert!(feed.offer(&mut batch), "the feed has room");
    };
    offer(0..10);
    log.drain();
    log.store.restart();
    // the new run: more than a chunk, the reader stopping in its second chunk
    let n = CHUNK as u32;
    offer(10..10 + n / 2);
    log.drain();
    offer(10 + n / 2..10 + n + 5);
    log.drain();
    assert_eq!(frames(log.store.read(reader)), (0..10 + n + 5).collect::<Vec<_>>(), "the reader reads both runs");
    offer(10 + n + 5..10 + n + 20);
    log.drain();
    assert_eq!(frames(log.store.read(reader)), (10 + n + 5..10 + n + 20).collect::<Vec<_>>(), "the reader reads on from its place in the later chunk");
    assert_eq!(frames(log.store.span(0.0, 1e3)), (10..10 + n + 20).collect::<Vec<_>>(), "the span is the new run's");
}
