//! Asynchronous loader registry owned by the frontend.

use crate::api::*;
use crate::model::Loader;
use makepad_widgets_core::*;
use std::path::{Path, PathBuf};
use std::io::Read;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use makepad_widgets_core::jobs::{self, JobHandle, JobId, JobSpec};

/// Registered format loaders plus cancellation state for the active load.
pub struct LoadCoordinator {
    seq: Arc<AtomicU64>,
    loaders: Vec<Arc<dyn Loader>>,
    /// Loads the caption's job strip can retry: their files.
    retry: HashMap<JobId, PathBuf>,
}

impl Default for LoadCoordinator {
    fn default() -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(0)),
            loaders: Vec::new(),
            retry: HashMap::new(),
        }
    }
}

impl LoadCoordinator {
    pub fn register<L: Loader + 'static>(&mut self, loader: L) {
        self.loaders.push(Arc::new(loader));
    }

    pub fn extensions(&self) -> Vec<&str> {
        self.loaders
            .iter()
            .flat_map(|loader| loader.extensions().iter().copied())
            .collect()
    }

    fn loader_for(&self, path: &Path) -> Option<Arc<dyn Loader>> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        if let Some(loader) = self.loaders
            .iter()
            .find(|loader| {
                extension.as_deref().is_some_and(|extension| {
                    loader.extensions().iter().any(|candidate| *candidate == extension)
                })
            })
            .cloned()
        {
            return Some(loader);
        }
        let mut prefix = [0_u8; 4096];
        let count = std::fs::File::open(path).ok()?.read(&mut prefix).ok()?;
        self.loaders
            .iter()
            .find(|loader| loader.probe(&prefix[..count]))
            .cloned()
    }

    pub fn open(&mut self, cx: &mut Cx, path: PathBuf) {
        let Some(loader) = self.loader_for(&path) else {
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();
            Cx::post_action(ShellAction::LoadFailed {
                path,
                error: format!("no loader registered for .{extension}"),
            });
            return;
        };

        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let seq_ref = self.seq.clone();
        // The caption's job: a cancel moves the load sequence on, which is what a newer
        // open does too; the loader checks it between its steps.
        let cancel_seq = self.seq.clone();
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string());
        let job = JobSpec::new(format!("Opening {name}")).stage("reading").on_cancel(move || {
            let _ = cancel_seq.compare_exchange(seq, seq + 1, Ordering::SeqCst, Ordering::SeqCst);
        }).start();
        let held: Vec<JobId> = jobs::snapshot().iter().map(|j| j.id).collect();
        self.retry.retain(|id, _| held.contains(id));
        self.retry.insert(job.id(), path.clone());
        Cx::post_action(ShellAction::LoadStarted(path.clone()));
        if let Ok(task) = cx.task_pool().submit(makepad_widgets_core::makepad_platform::thread::Class::Background, move || {
            let guard_path = path.clone();
            let guard_seq = seq_ref.clone();
            // A loader that panics unwinds the job's handle with it: the job ends as crashed.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                Self::load_on_worker(loader, path, seq, seq_ref, job)
            }));
            if let Err(payload) = result {
                let error = format!("loader crashed: {}", panic_message(&payload));
                if guard_seq.load(Ordering::SeqCst) == seq {
                    Cx::post_action(ShellAction::LoadFailed {
                        path: guard_path,
                        error,
                    });
                }
            }
        }) {
            task.detach();
        }
    }

    /// Retry on the caption's job strip of a failed load: open the file again.
    pub fn retry(&mut self, cx: &mut Cx, job: JobId) -> bool {
        let Some(path) = self.retry.remove(&job) else { return false };
        self.open(cx, path);
        true
    }

    fn load_on_worker(
        loader: Arc<dyn Loader>,
        path: PathBuf,
        seq: u64,
        seq_ref: Arc<AtomicU64>,
        job: JobHandle,
    ) {
        let still_current = || seq_ref.load(Ordering::SeqCst) == seq;
        let cancelled = || !still_current();
        let mut last = String::new();
        let mut progress = |value: LoadProgress| {
            if !still_current() {
                return;
            }
            match &value {
                LoadProgress::Opening => job.stage("reading", None, None),
                LoadProgress::Parsing(f) => job.stage("parsing", None, Some(*f as f32)),
                LoadProgress::Meshing { done, total } => job.stage("meshes", Some((*done as u32, *total as u32)), Some(*done as f32 / (*total).max(1) as f32)),
                LoadProgress::Building { stage, fraction } => job.stage(*stage, None, Some(*fraction as f32)),
                LoadProgress::Done => {}
            }
            let key = format!("{value:?}");
            if key != last {
                last = key;
                Cx::post_action(ShellAction::LoadProgress(value));
            }
        };
        let document = match loader.load_cancellable(&path, &mut progress, &cancelled) {
            Ok(document) => document,
            Err(error) => {
                if still_current() && error != LoadError::Cancelled {
                    job.fail(error.to_string(), true);
                    Cx::post_action(ShellAction::LoadFailed {
                        path,
                        error: error.to_string(),
                    });
                } else {
                    job.cancelled();
                }
                return;
            }
        };
        if !still_current() {
            job.cancelled();
            return;
        }
        let mut last_stage = "";
        let mut last_step = -1i32;
        let scene = Scene::from_document_with(document, &mut |stage, fraction| {
            if !still_current() {
                return;
            }
            job.stage(stage, None, Some(fraction as f32));
            let step = (fraction * 20.0) as i32;
            if stage != last_stage || step != last_step {
                last_stage = stage;
                last_step = step;
                Cx::post_action(ShellAction::LoadProgress(LoadProgress::Building {
                    stage,
                    fraction,
                }));
            }
        });
        if !still_current() {
            job.cancelled();
            return;
        }
        let scene = Arc::new(scene);
        job.done();
        Cx::post_action(ShellAction::LoadProgress(LoadProgress::Done));
        Cx::post_action(ShellAction::Loaded(scene));
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

pub fn apply(
    cx: &mut Cx,
    loader: &mut LoadCoordinator,
    _state: &mut AppState,
    action: &ShellAction,
) -> bool {
    match action {
        ShellAction::OpenFile(path) => {
            loader.open(cx, path.clone());
            true
        }
        // A load that worked says nothing: the plate shows it (no success notices).
        _ => false,
    }
}
