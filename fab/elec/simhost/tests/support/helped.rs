//! A test's threads with two helper cores for a run in rounds: shared by the host's tests that
//! need a run able to go in rounds (`#[path]`).

use fab_elec_sim::rounds::{Helpers, Post};
use fab_elec_simhost::Spawn;
use std::sync::{mpsc, Arc, Mutex};

/// A test's threads with two helper cores for a run in rounds: long-lived threads fed by a
/// channel, as the app's task pool is (they end with the run that holds their sender).
pub struct Helped;

impl Spawn for Helped {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn helpers(&self) -> Option<Helpers> {
        let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..2 {
            let rx = rx.clone();
            std::thread::spawn(move || while let Ok(job) = rx.lock().unwrap_or_else(|e| e.into_inner()).recv() {
                job();
            });
        }
        let tx = Mutex::new(tx);
        let post: Post = Arc::new(move |job| {
            let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(job);
        });
        Some(Helpers { post, count: 2 })
    }
}
