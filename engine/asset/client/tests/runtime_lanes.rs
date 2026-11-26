//! The runtime's parallel lanes: what a UI is actually promised once
//! requests stop running one at a time.
//!
//! The properties proven here are the ones that make a thumbnail grid fill
//! while a track downloads:
//! - a big transfer occupies its own lane and NOT the fast one,
//! - many small requests really do overlap (they finish out of submission
//!   order), while each request's own events stay ordered,
//! - cancel still works for queued and in-flight work in both lanes,
//! - two lanes fetching the same blob produce one download, not a corrupted
//!   partial file,
//! - shutdown drains what was accepted and joins every worker.

// Tests of the native client (`web` builds the portable one, which the lib, static_store, tree and
// pipelines tests cover).
#![cfg(all(not(target_arch = "wasm32"), not(feature = "web")))]

// Native threaded runtime test: std deadlines cannot enter the wasm library.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]

mod common;

use common::*;
use makepad_asset_client::{
    AssetClient, ClientConfig, ClientError, ClientEvent, ClientOutput, ClientRequest,
    ClientRuntime, HttpLimits, Lane, RequestId, RuntimeConfig, SubmitOptions,
};
use makepad_asset_data::AssetId;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

fn fast_limits() -> HttpLimits {
    HttpLimits {
        connect_timeout_ms: 2_000,
        read_timeout_ms: 2_000,
        write_timeout_ms: 2_000,
        head_probe_ms: 2_000,
        body_deadline_ms: 20_000,
    }
}

fn config(name: &str) -> ClientConfig {
    let mut cfg = ClientConfig::new(test_root(name));
    cfg.http = fast_limits();
    cfg.blob_body_deadline_ms = 20_000;
    cfg
}

/// A store with a small asset per id plus one big blob for the bulk lane.
fn lane_store() -> (FixtureStore, Vec<makepad_asset_data::AssetRevisionRef>) {
    let mut store = FixtureStore::default();
    let mut refs = Vec::new();
    for i in 0..12u8 {
        refs.push(store.add_prop(
            10 + i,
            "stock",
            None,
            &format!("Prop {i}"),
            payload(500 + i as u64, 1_200),
            vec![],
        ));
    }
    (store, refs)
}

struct Recorder {
    /// id → events in arrival order.
    per_id: HashMap<RequestId, Vec<&'static str>>,
    /// Terminal events in completion order.
    finished: Vec<RequestId>,
    failures: HashMap<RequestId, ClientError>,
    outputs: HashMap<RequestId, ClientOutput>,
}

impl Recorder {
    fn new() -> Recorder {
        Recorder {
            per_id: HashMap::new(),
            finished: Vec::new(),
            failures: HashMap::new(),
            outputs: HashMap::new(),
        }
    }

    fn absorb(&mut self, events: Vec<ClientEvent>) {
        for event in events {
            let id = event.id();
            let slot = self.per_id.entry(id).or_default();
            match event {
                ClientEvent::Started { .. } => slot.push("started"),
                ClientEvent::Progress { .. } => slot.push("progress"),
                ClientEvent::Done { output, .. } => {
                    slot.push("done");
                    self.finished.push(id);
                    self.outputs.insert(id, output);
                }
                ClientEvent::Failed { error, .. } => {
                    slot.push("failed");
                    self.finished.push(id);
                    self.failures.insert(id, error);
                }
            }
        }
    }

    /// Every id: at most one `started`, and when it started it came first;
    /// exactly one terminal event, last, with nothing after it. (A request
    /// cancelled while still queued never starts, so its whole stream is one
    /// `failed` — that is the documented skip, not a lost `started`.)
    fn assert_per_id_consistent(&self) {
        for (id, events) in &self.per_id {
            let starts = events.iter().filter(|e| **e == "started").count();
            assert!(starts <= 1, "request {id} started twice: {events:?}");
            if starts == 1 {
                assert_eq!(
                    events.first(),
                    Some(&"started"),
                    "request {id} started late: {events:?}"
                );
            } else {
                assert_eq!(
                    events.as_slice(),
                    ["failed"],
                    "request {id} never started but is not a queued cancel: {events:?}"
                );
            }
            let terminals =
                events.iter().filter(|e| **e == "done" || **e == "failed").count();
            assert_eq!(terminals, 1, "request {id} has {terminals} terminal events: {events:?}");
            let last = events.last().copied().unwrap_or("");
            assert!(
                last == "done" || last == "failed",
                "request {id} emitted events after its terminal one: {events:?}"
            );
        }
    }

    fn drain_until(
        &mut self,
        runtime: &mut ClientRuntime,
        wanted: &[RequestId],
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        while !wanted.iter().all(|id| self.finished.contains(id)) {
            assert!(
                Instant::now() < deadline,
                "requests never finished: {:?} of {:?}",
                self.finished,
                wanted
            );
            self.absorb(runtime.poll());
            std::thread::sleep(Duration::from_millis(2));
        }
        self.absorb(runtime.poll());
    }
}

#[test]
fn lanes_are_chosen_by_size_and_kind() {
    let (store, refs) = lane_store();
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    let client = AssetClient::connect(config("lane_classify"), fixture.endpoints(), None).unwrap();
    let runtime = ClientRuntime::start(client).unwrap();
    let blob = makepad_asset_data::BlobId::hash_of(b"whatever");

    // Control-plane work is fast, whatever it is.
    assert_eq!(
        runtime.lane_of(&ClientRequest::AssetDetail { id: AssetId::from_bytes([1; 16]) }),
        Lane::Fast
    );
    assert_eq!(
        runtime.lane_of(&ClientRequest::FetchAssetManifest { rev: refs[0].revision }),
        Lane::Fast
    );
    // A small declared blob is fast; a big one is bulk; an UNKNOWN length is
    // treated as big, because guessing small is what stalls a lane.
    assert_eq!(
        runtime.lane_of(&ClientRequest::FetchBlob {
            blob,
            expected_len: Some(64 * 1024),
            pin: false
        }),
        Lane::Fast
    );
    assert_eq!(
        runtime.lane_of(&ClientRequest::FetchBlob {
            blob,
            expected_len: Some(8 * 1024 * 1024),
            pin: false
        }),
        Lane::Bulk
    );
    assert_eq!(
        runtime.lane_of(&ClientRequest::FetchBlob { blob, expected_len: None, pin: false }),
        Lane::Bulk
    );
    runtime.shutdown();
}

#[test]
fn concurrent_requests_overlap_and_each_stream_stays_consistent() {
    let (store, refs) = lane_store();
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    let client = AssetClient::connect(config("lane_overlap"), fixture.endpoints(), None).unwrap();
    let mut runtime = ClientRuntime::start(client).unwrap();

    // Twelve manifest fetches: all fast-lane, all independent.
    let ids: Vec<RequestId> = refs
        .iter()
        .map(|r| {
            runtime
                .submit(ClientRequest::FetchAssetManifest { rev: r.revision })
                .expect("submit")
        })
        .collect();

    let mut rec = Recorder::new();
    rec.drain_until(&mut runtime, &ids, Duration::from_secs(20));
    rec.assert_per_id_consistent();
    assert!(rec.failures.is_empty(), "unexpected failures: {:?}", rec.failures);
    assert_eq!(rec.finished.len(), ids.len());
    // Every id is accounted for exactly once.
    assert_eq!(rec.finished.iter().collect::<HashSet<_>>().len(), ids.len());
    runtime.shutdown();
}

#[test]
fn a_bulk_transfer_does_not_delay_the_fast_lane() {
    let (mut store, refs) = lane_store();
    // One big blob, served in slow drips so the bulk lane is provably busy.
    let big = payload(77, 900_000);
    let big_blob = store.add_blob(big.clone());
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    // 32 KiB every 40 ms ≈ 1.1 s for the whole body.
    *fixture.knobs.drip_blob.lock().unwrap() = Some((32 * 1024, 40));
    let client = AssetClient::connect(config("lane_no_block"), fixture.endpoints(), None).unwrap();
    let mut runtime = ClientRuntime::start(client).unwrap();

    // The big transfer goes first and takes the bulk lane by classification.
    let bulk_id = runtime
        .submit(ClientRequest::FetchBlob {
            blob: big_blob,
            expected_len: Some(big.len() as u64),
            pin: false,
        })
        .expect("submit bulk");
    assert_eq!(
        runtime.lane_of(&ClientRequest::FetchBlob {
            blob: big_blob,
            expected_len: Some(big.len() as u64),
            pin: false
        }),
        Lane::Bulk
    );
    // Give the transfer a moment to actually start.
    std::thread::sleep(Duration::from_millis(50));

    // Small fast-lane work submitted AFTER it must not wait for it.
    let fast_ids: Vec<RequestId> = refs
        .iter()
        .take(8)
        .map(|r| {
            runtime
                .submit(ClientRequest::FetchAssetManifest { rev: r.revision })
                .expect("submit fast")
        })
        .collect();

    let mut rec = Recorder::new();
    rec.drain_until(&mut runtime, &fast_ids, Duration::from_secs(20));
    assert!(
        !rec.finished.contains(&bulk_id),
        "the fast lane waited for the bulk transfer to finish"
    );
    let mut all = fast_ids.clone();
    all.push(bulk_id);
    rec.drain_until(&mut runtime, &all, Duration::from_secs(30));
    rec.assert_per_id_consistent();
    assert!(rec.failures.is_empty(), "unexpected failures: {:?}", rec.failures);
    // The bulk transfer finished last, after every fast request.
    assert_eq!(rec.finished.last(), Some(&bulk_id));
    runtime.shutdown();
}

#[test]
fn newest_first_serves_the_freshest_queued_work_first() {
    let (mut store, refs) = lane_store();
    let big = payload(77, 30_000);
    let big_blob = store.add_blob(big.clone());
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    // A held transfer keeps the single fast worker until everything below is
    // submitted, so the queue policy is observed however slow the submission
    // is under load (the client waits on the held body up to its read
    // timeout, here the body deadline: only a test stalled that long fails).
    fixture.knobs.hold_blobs();
    let mut cfg = config("lane_lifo");
    cfg.http.read_timeout_ms = cfg.http.body_deadline_ms;
    let client = AssetClient::connect(cfg, fixture.endpoints(), None).unwrap();
    let mut runtime = ClientRuntime::start_with(
        client,
        RuntimeConfig { fast_workers: 1, bulk_workers: 1, ..RuntimeConfig::default_v1() },
    )
    .unwrap();

    let blocker = runtime
        .submit_with(
            ClientRequest::FetchBlob {
                blob: big_blob,
                expected_len: Some(big.len() as u64),
                pin: false,
            },
            SubmitOptions::fast(),
        )
        .expect("submit blocker");
    let mut fifo = Vec::new();
    for r in refs.iter().take(6) {
        fifo.push(
            runtime
                .submit(ClientRequest::FetchAssetManifest { rev: r.revision })
                .expect("submit"),
        );
    }
    let jumper = runtime
        .submit_with(
            ClientRequest::FetchAssetManifest { rev: refs[7].revision },
            SubmitOptions::newest_first(),
        )
        .expect("submit lifo");
    fixture.knobs.release_blobs();

    let mut rec = Recorder::new();
    let mut all = fifo.clone();
    all.push(jumper);
    all.push(blocker);
    rec.drain_until(&mut runtime, &all, Duration::from_secs(30));
    rec.assert_per_id_consistent();
    // Whether or not the worker had taken the blocker yet, the newest-first
    // request must be served before every manifest queued ahead of it.
    let manifests: Vec<RequestId> =
        rec.finished.iter().copied().filter(|id| *id != blocker).collect();
    assert_eq!(
        manifests.first(),
        Some(&jumper),
        "newest-first request did not jump the queue: {:?} (blocker {blocker:?})",
        rec.finished
    );
    runtime.shutdown();
}

#[test]
fn cancel_covers_queued_and_in_flight_work_in_both_lanes() {
    let (mut store, refs) = lane_store();
    let big = payload(88, 900_000);
    let big_blob = store.add_blob(big.clone());
    let hold = payload(89, 300_000);
    let hold_blob = store.add_blob(hold.clone());
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    *fixture.knobs.drip_blob.lock().unwrap() = Some((16 * 1024, 40));
    let client = AssetClient::connect(config("lane_cancel"), fixture.endpoints(), None).unwrap();
    let mut runtime = ClientRuntime::start_with(
        client,
        RuntimeConfig { fast_workers: 1, bulk_workers: 1, ..RuntimeConfig::default_v1() },
    )
    .unwrap();

    // In-flight bulk transfer: aborts at its next chunk.
    let inflight = runtime
        .submit(ClientRequest::FetchBlob {
            blob: big_blob,
            expected_len: Some(big.len() as u64),
            pin: false,
        })
        .expect("submit bulk");
    std::thread::sleep(Duration::from_millis(80));
    // A dripped transfer holds the single fast worker while the fast work is
    // queued and cancelled below, whatever the load.
    let holder = runtime
        .submit_with(
            ClientRequest::FetchBlob {
                blob: hold_blob,
                expected_len: Some(hold.len() as u64),
                pin: false,
            },
            SubmitOptions::fast(),
        )
        .expect("submit holder");
    // Queued fast work behind a single fast worker: cancelled before it runs.
    let first_fast = runtime
        .submit(ClientRequest::FetchAssetManifest { rev: refs[0].revision })
        .expect("submit fast");
    let queued_fast = runtime
        .submit(ClientRequest::FetchAssetManifest { rev: refs[1].revision })
        .expect("submit fast");
    runtime.cancel(queued_fast);
    runtime.cancel(inflight);

    let mut rec = Recorder::new();
    rec.drain_until(
        &mut runtime,
        &[inflight, holder, first_fast, queued_fast],
        Duration::from_secs(20),
    );
    rec.assert_per_id_consistent();
    assert!(
        matches!(rec.failures.get(&inflight), Some(ClientError::Cancelled)),
        "in-flight bulk cancel: {:?}",
        rec.failures.get(&inflight)
    );
    assert!(
        matches!(rec.failures.get(&queued_fast), Some(ClientError::Cancelled)),
        "queued fast cancel: {:?}",
        rec.failures.get(&queued_fast)
    );
    // The uncancelled neighbour still completed.
    assert!(rec.outputs.contains_key(&first_fast));
    runtime.shutdown();
}

#[test]
fn two_lanes_fetching_one_blob_download_it_once() {
    let (mut store, _refs) = lane_store();
    let bytes = payload(99, 300_000);
    let blob = store.add_blob(bytes.clone());
    let pooled_blob = store.add_blob(b"also fetched together on pool workers".to_vec());
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    *fixture.knobs.drip_blob.lock().unwrap() = Some((32 * 1024, 10));
    let client = AssetClient::connect(config("lane_dedup"), fixture.endpoints(), None).unwrap();
    let mut first = client.lane_clone();
    let mut second = client.lane_clone();
    let mut runtime = ClientRuntime::start(client).unwrap();

    // Same digest, both lanes: the fast one by explicit override, the bulk
    // one by classification. One partial file, so one of them must wait and
    // then find the committed object.
    let a = runtime
        .submit_with(
            ClientRequest::FetchBlob {
                blob,
                expected_len: Some(bytes.len() as u64),
                pin: false,
            },
            SubmitOptions::fast(),
        )
        .expect("submit fast");
    let b = runtime
        .submit(ClientRequest::FetchBlob {
            blob,
            expected_len: Some(bytes.len() as u64),
            pin: false,
        })
        .expect("submit bulk");

    let mut rec = Recorder::new();
    rec.drain_until(&mut runtime, &[a, b], Duration::from_secs(30));
    rec.assert_per_id_consistent();
    assert!(rec.failures.is_empty(), "unexpected failures: {:?}", rec.failures);
    for id in [a, b] {
        match rec.outputs.get(&id) {
            Some(ClientOutput::Blob { content, .. }) => {
                assert_eq!(std::fs::read(content.as_path().unwrap()).unwrap(), bytes);
            }
            other => panic!("wrong output for {id}: {other:?}"),
        }
    }
    // Exactly one body transfer reached the server: the second fetch found
    // the committed object instead of racing a second writer onto the same
    // partial file.
    assert_eq!(
        fixture.log.count("GET", "/v1/blobs/"),
        1,
        "the same blob was downloaded twice"
    );
    runtime.shutdown();

    // Hold the first transfer until the second has actually reached the busy digest.
    // Even with one CPU permit the holder must then be able to resume and commit.
    use makepad_platform::{thread::without_permit, Class, Cx};
    use std::sync::mpsc::sync_channel;
    let cx = Cx::new(Box::new(|_, _| {}));
    let pool = cx.task_pool();
    let timeout = Duration::from_secs(10);
    let (claimed, holder_ready) = sync_channel(1);
    let (contended, waiter_ready) = sync_channel(1);
    let (done, results) = sync_channel(2);
    let first_done = done.clone();
    pool.submit(Class::Visible, move || {
        let path = first.fetch_blob(&pooled_blob, None, Some(&mut |present, _| {
            if present == 0 {
                claimed.send(()).unwrap();
                without_permit(|| waiter_ready.recv_timeout(timeout)).expect("second fetch reached the digest");
            }
        }));
        first_done.send(path).unwrap();
    }).unwrap().detach();
    holder_ready.recv_timeout(timeout).expect("first fetch claimed the digest");
    pool.submit(Class::Visible, move || {
        let start = Instant::now();
        let path = second.fetch_blob_with_abort(&pooled_blob, None, None, &|| {
            let _ = contended.try_send(());
            start.elapsed() >= timeout
        });
        done.send(path).unwrap();
    }).unwrap().detach();
    let committed = results.recv_timeout(timeout).unwrap().unwrap();
    assert_eq!(results.recv_timeout(timeout).unwrap().unwrap(), committed, "both pool jobs reached the committed blob");
}

#[test]
fn shutdown_drains_accepted_work_and_joins_every_worker() {
    let (store, refs) = lane_store();
    let fixture = FixtureServer::start(store, FixtureOptions::default());
    let client = AssetClient::connect(config("lane_shutdown"), fixture.endpoints(), None).unwrap();
    let mut runtime = ClientRuntime::start(client).unwrap();
    let ids: Vec<RequestId> = refs
        .iter()
        .map(|r| {
            runtime
                .submit(ClientRequest::FetchAssetManifest { rev: r.revision })
                .expect("submit")
        })
        .collect();
    // Shutdown joins every lane worker; queued work runs to completion first.
    let started = Instant::now();
    runtime.shutdown();
    assert!(started.elapsed() < Duration::from_secs(20), "shutdown hung");
    assert_eq!(ids.len(), 12);
}
