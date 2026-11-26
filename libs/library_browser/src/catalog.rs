//! The library's catalogue worker for a window with no documents of its
//! own (Stage: SCORE's explorer, the VJ grid's REMOVE): a lane's rows and
//! their thumbnails, and who uses an asset. One long-lived worker owns an
//! asset client; the UI thread only `try_send`s jobs and `try_recv`s
//! replies (bounded both ways, a full queue is retried by the caller on a
//! later pump).

use crate::lanes::{catalogue, load_thumb, LibLane, LibRow, Thumb};
use makepad_asset_client::dto::DependentsDto;
use makepad_asset_client::AssetClient;
use makepad_asset_connection::{self as asset_connection, redact};
use makepad_asset_data::AssetRevisionRef;
use makepad_widgets::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

const QUEUE: usize = 8;
const WORKER_TICK: Duration = Duration::from_millis(250);
const PUBLISH_WAIT: Duration = Duration::from_millis(20);

pub enum CatalogJob {
    /// A lane's items matching `text` (and `tag`), newest first or A–Z.
    Query { serial: u64, lane: LibLane, text: String, tag: Option<String>, az: bool },
    /// Who uses `reference`'s asset.
    Dependents { reference: AssetRevisionRef },
}

pub enum CatalogReply {
    /// A lane's rows and its tags with their counts.
    Rows { serial: u64, lane: LibLane, result: Result<(Vec<LibRow>, Vec<(String, u64)>), String> },
    Thumb { reference: AssetRevisionRef, thumb: Thumb },
    Dependents { reference: AssetRevisionRef, result: Result<DependentsDto, String> },
    Stopped,
}

struct Shared {
    stop: AtomicBool,
    query: AtomicU64,
}

pub struct Catalog {
    jobs: Option<SyncSender<CatalogJob>>,
    replies: Receiver<CatalogReply>,
    shared: Arc<Shared>,
    pub error: Option<String>,
}

impl Catalog {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel(QUEUE);
        let (reply_tx, reply_rx) = sync_channel(QUEUE);
        let shared = Arc::new(Shared { stop: AtomicBool::new(false), query: AtomicU64::new(0) });
        let worker_shared = shared.clone();
        let options = ThreadOptions { name: Some(Arc::from("library-catalog")), stack_size: None, priority: CxThreadPriority::Utility };
        let mut catalog = Self { jobs: None, replies: reply_rx, shared, error: None };
        match cx.thread_spawner().spawn_worker(options, move || worker(job_rx, reply_tx, worker_shared)) {
            Ok(handle) => {
                handle.detach();
                catalog.jobs = Some(job_tx);
            }
            Err(error) => catalog.error = Some(format!("library catalog worker unavailable: {error:?}")),
        }
        catalog
    }

    /// The catalogue generation wanted now; older queries end early.
    pub fn set_query_serial(&self, serial: u64) {
        self.shared.query.store(serial, Ordering::Release);
    }

    /// `Err(job)` when the queue is full (retry later) or the worker is gone.
    pub fn try_send(&self, job: CatalogJob) -> Result<(), CatalogJob> {
        let Some(jobs) = &self.jobs else { return Err(job) };
        match jobs.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => Err(job),
        }
    }

    pub fn try_recv(&self) -> Option<CatalogReply> {
        self.replies.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.jobs.take();
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        self.stop();
    }
}

struct StopGuard(SyncSender<CatalogReply>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        let _ = self.0.try_send(CatalogReply::Stopped);
    }
}

fn worker(jobs: Receiver<CatalogJob>, replies: SyncSender<CatalogReply>, shared: Arc<Shared>) {
    let _guard = StopGuard(replies.clone());
    // Declared first so the client (a lane clone of its owner) drops before it.
    let origin = asset_connection::origin();
    let mut client: Option<AssetClient> = None;
    let mut thumbs: Vec<(u64, AssetRevisionRef)> = Vec::new();
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let job = if thumbs.is_empty() {
            match jobs.recv_timeout(WORKER_TICK) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match jobs.try_recv() {
                Ok(job) => Some(job),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        };
        let reply = match job {
            None => {
                // Idle: the next queued thumbnail, while its query is current.
                let Some((serial, reference)) = (!thumbs.is_empty()).then(|| thumbs.remove(0)) else { continue };
                if serial != shared.query.load(Ordering::Acquire) {
                    continue;
                }
                match asset_connection::connected(&mut client, &origin).ok().and_then(|client| load_thumb(client, &reference)) {
                    Some(thumb) => CatalogReply::Thumb { reference, thumb },
                    None => continue,
                }
            }
            Some(CatalogJob::Query { serial, lane, text, tag, az }) => {
                if serial != shared.query.load(Ordering::Acquire) {
                    continue;
                }
                let current = || serial == shared.query.load(Ordering::Acquire) && !shared.stop.load(Ordering::Acquire);
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| catalogue(client, lane, &text, tag.as_deref(), az, &current, None));
                if result.is_err() {
                    client = None;
                }
                if let Ok((rows, _)) = &result {
                    thumbs.clear();
                    thumbs.extend(rows.iter().filter(|row| !row.poster_pending).map(|row| (serial, row.reference)));
                }
                CatalogReply::Rows { serial, lane, result }
            }
            Some(CatalogJob::Dependents { reference }) => {
                let result = asset_connection::connected(&mut client, &origin).and_then(|client| client.dependents(&reference.asset_id, None).map_err(redact));
                CatalogReply::Dependents { reference, result }
            }
        };
        if !publish(&replies, &shared, reply) {
            break;
        }
    }
    while jobs.try_recv().is_ok() {}
}

fn publish(replies: &SyncSender<CatalogReply>, shared: &Shared, mut reply: CatalogReply) -> bool {
    loop {
        match replies.try_send(reply) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(back)) => {
                if shared.stop.load(Ordering::Acquire) {
                    return false;
                }
                reply = back;
                std::thread::sleep(PUBLISH_WAIT);
            }
        }
    }
}
