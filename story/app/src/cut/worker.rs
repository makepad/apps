//! The cut worker: every write and load of a cut's Splash runs here, off
//! the UI thread (each evaluates the flow several times). One long-lived
//! thread fed by a bounded channel; one job in flight at a time, so edits
//! apply in the order they were made.

use super::ops::{apply, CutOp};
use super::patch::{diagnostics, Patcher};
use makepad_story_cut::CutDoc;
use makepad_asset_data::tree::TreeDraft;
use super::super::document_tree;
use makepad_app_shell::tool_host::Change;
use makepad_widgets::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

pub enum Job {
    Apply { serial: u64, source: String, op: CutOp, tree: Option<(Arc<TreeDraft>, String)> },
    Document { serial: u64, tree: Arc<TreeDraft>, path: String, changes: Vec<Change> },
    Load { serial: u64, source: String, tree: Option<(Arc<TreeDraft>, String)> },
}

pub struct Done {
    pub serial: u64,
    pub result: Result<(String, Arc<CutDoc>, Option<Arc<TreeDraft>>), String>,
}

pub struct CutWorker {
    jobs: Option<SyncSender<Job>>,
    done: Receiver<Done>,
    pub error: Option<String>,
}

impl CutWorker {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel::<Job>(2);
        let (done_tx, done_rx) = sync_channel::<Done>(2);
        let options = ThreadOptions { name: Some(Arc::from("stage-edit-writer")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::UserInitiated };
        let mut worker = Self { jobs: None, done: done_rx, error: None };
        match cx.thread_spawner().spawn_worker(options, move || run(job_rx, done_tx)) {
            Ok(handle) => {
                handle.detach();
                worker.jobs = Some(job_tx);
            }
            Err(error) => worker.error = Some(format!("edit worker unavailable: {error:?}")),
        }
        worker
    }

    pub fn try_send(&self, job: Job) -> Result<(), Job> {
        let Some(jobs) = &self.jobs else { return Err(job) };
        jobs.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
        })
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.jobs.take();
    }
}

fn run(jobs: Receiver<Job>, done: SyncSender<Done>) {
    let mut patcher = Patcher::new();
    while let Ok(job) = jobs.recv() {
        let reply = match job {
            Job::Apply { serial, source, op, tree } => Done {
                serial,
                result: match &mut patcher {
                    Ok(patcher) => match tree {
                        Some((tree, path)) => document_tree::apply_cut(patcher, &tree, &path, &op).map(|(source, doc, tree)| (source, doc, Some(tree))),
                        None => { patcher.tree = None; apply(patcher, &source, &op).map(|(source, doc)| (source, Arc::new(doc), None)) }
                    },
                    Err(error) => Err(error.clone()),
                },
            },
            Job::Load { serial, source, tree } => Done {
                serial,
                result: match &tree {
                    Some((tree, path)) => CutDoc::load_tree(&source, tree.context(path)),
                    None => CutDoc::load(&source),
                }.map(|doc| (source, Arc::new(doc), tree.map(|(tree, _)| tree))).map_err(diagnostics),
            },
            Job::Document { serial, tree, mut path, changes } => Done {
                serial,
                result: (|| {
                    for change in &changes { if let Change::Rename { from, to } = change { if *from == path { path = to.clone(); } } }
                    let tree = document_tree::apply_all(&tree, &changes)?;
                    let doc = document_tree::load(&tree, &path)?.into_cut()?;
                    Ok((tree.text(&path)?.into(), doc, Some(tree)))
                })(),
            },
        };
        // The UI takes one reply per pump; waiting here is this thread's
        // own time, never the UI's.
        if done.send(reply).is_err() {
            break;
        }
    }
}
