//! Local window state. Shared model actions and replay never enter this module.
use super::{
    config::Config, sftp::{Progress, Published, Upload}, store::{Author, Entry, Store},
};
use crate::ai_out::SafeToolError;
use makepad_town_core::trust::{AiJson, AiOut, HomeGate};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool};
use std::{io, path::{Path, PathBuf}, sync::{Arc, mpsc::{self, Receiver, Sender}}};

#[derive(Default, Clone, PartialEq, Eq)]
pub struct Draft {
    pub ids: Vec<String>,
    pub title: String,
    pub text: String,
    pub target: usize,
}

pub enum Selection { Path(PathBuf), Bytes(String, Arc<[u8]>) }

struct Intake {
    entries: Vec<Entry>,
    problems: Vec<String>,
}

type Outcome = (Option<Arc<Upload>>, Result<Published, String>);
struct Sending {
    progress: Receiver<Progress>,
    task: TaskHandle<Outcome>,
}

pub struct LocalPublish {
    store: Store,
    config: Config,
    pub config_problem: Option<String>,
    pub entries: Vec<Entry>,
    pub draft: Draft,
    pub status: String,
    pub url: Option<String>,
    inventory: Option<TaskHandle<io::Result<Vec<Entry>>>>,
    imports: Vec<TaskHandle<Intake>>,
    sending: Option<Sending>,
    retry: Option<(Draft, Arc<Upload>)>,
}

impl LocalPublish {
    pub fn open(home: &Path) -> Self {
        let (config, config_problem) = match Config::load(home) {
            Ok(config) => (config, None),
            Err(e) => (Config::default(), Some(e.to_string())),
        };
        Self { store: Store::new(home), config, config_problem, entries: Vec::new(), draft: Draft::default(),
            status: String::new(), url: None, inventory: None, imports: Vec::new(), sending: None, retry: None }
    }

    pub fn enabled(&self) -> bool { self.config.has_publish_action() }
    pub fn targets(&self) -> Vec<String> { self.config.targets.iter().map(|t| t.name.clone()).collect() }
    pub fn sending(&self) -> bool { self.sending.is_some() }
    pub fn pending(&self) -> bool { self.inventory.is_some() || !self.imports.is_empty() || self.sending() }

    pub fn load_entries(&mut self, pool: &TaskPool) {
        if !self.enabled() { return }
        let store = self.store.clone();
        match pool.submit_named(Class::Background, "read publish media", move || store.offered()) {
            Ok(task) => self.inventory = Some(task),
            Err(e) => self.status = e.to_string(),
        }
    }

    pub fn catalog(&self, gate: &HomeGate) -> Result<AiOut, String> {
        let withheld = |why| SafeToolError::withheld(why).as_str().to_owned();
        gate.check_outside().map_err(withheld)?;
        if !self.enabled() { return Err("publish is unavailable".into()) }
        if self.inventory.is_some() || !self.imports.is_empty() { return Err("local media is loading; try again".into()) }
        let mut entries = Vec::new();
        for e in self.entries.iter().filter(|e| e.author == Author::Local) {
            entries.push(AiJson::object(vec![
                ("id", AiJson::text(gate.local_captured(e.id.clone()).map_err(withheld)?)),
                ("file", AiJson::text(gate.local_captured(e.media.file.clone()).map_err(withheld)?)),
                ("bytes", AiJson::int(e.media.bytes as i64)),
            ]));
        }
        gate.finish_json(AiJson::array(entries)).map_err(withheld)
    }

    /// The local AI stops at this boundary. It cannot replace a draft under review.
    pub fn review(&mut self, ids: Vec<String>, title: String, text: String) -> Result<(), String> {
        if !self.enabled() { return Err("publish is unavailable".into()) }
        if self.pending() || !self.draft.title.is_empty() || !self.draft.text.is_empty() { return Err("finish or discard the current draft first".into()) }
        if title.trim().is_empty() || ids.is_empty() { return Err("a draft needs a title and media".into()) }
        if ids.iter().any(|id| !self.entries.iter().any(|e| e.author == Author::Local && &e.id == id)) { return Err("unknown local media entry".into()) }
        let target = self.draft.target;
        self.clear();
        self.draft = Draft { ids, title, text, target };
        Ok(())
    }

    pub fn clear(&mut self) {
        if !self.sending() {
            self.retry = None;
            self.draft = Draft::default();
            self.url = None;
            self.status.clear();
        }
    }

    pub fn import(&mut self, pool: &TaskPool, files: Vec<Selection>) {
        if !self.enabled() || self.sending() || files.is_empty() { return }
        let store = self.store.clone();
        match pool.submit_named(Class::Background, "import publish media", move || {
            let mut result = Intake { entries: Vec::new(), problems: Vec::new() };
            for file in files {
                let (name, entry) = match file {
                    Selection::Path(path) => (path.file_name().unwrap_or_default().to_string_lossy().into_owned(), store.import(&path, Author::Local)),
                    Selection::Bytes(name, bytes) => { let entry = store.import_bytes(&name, &bytes, Author::Local); (name, entry) }
                };
                match entry {
                    Ok(entry) => result.entries.push(entry),
                    Err(e) => result.problems.push(format!("{name}: {e}")),
                }
            }
            result
        }) {
            Ok(task) => { self.imports.push(task); self.url = None; self.status = "Adding media…".into(); }
            Err(e) => self.status = e.to_string(),
        }
    }

    pub fn can_confirm(&self) -> bool {
        self.enabled() && !self.pending() && !self.draft.title.trim().is_empty() && !self.draft.ids.is_empty()
    }

    pub fn confirm(&mut self, pool: &TaskPool) -> Result<(), String> {
        self.confirm_with(pool, |upload, tx| upload.run(Path::new("sftp"), tx))
    }

    pub(super) fn confirm_with(&mut self, pool: &TaskPool, send: impl FnOnce(&Upload, &Sender<Progress>) -> io::Result<Published> + Send + 'static) -> Result<(), String> {
        if !self.can_confirm() { return Err("review a title and at least one file before confirming".into()) }
        let draft = self.draft.clone();
        let target = self.config.targets.get(draft.target).ok_or("choose a target")?.clone();
        let retry = self.retry.as_ref().filter(|(old,_)| old == &draft).map(|(_,job)| job.clone());
        let store = self.store.clone();
        let (tx, progress) = mpsc::channel();
        let task = pool.submit_named(Class::Background, "publish confirmed media", move || {
            let job = match retry {
                Some(job) => job,
                None => match Upload::prepare(store, target, draft.ids, draft.title, draft.text) {
                    Ok(job) => Arc::new(job),
                    Err(e) => return (None, Err(e.to_string())),
                },
            };
            let result = send(&job, &tx).map_err(|e| e.to_string());
            (Some(job), result)
        }).map_err(|e| e.to_string())?;
        self.sending = Some(Sending { progress, task });
        self.url = None;
        self.status = "Preparing upload…".into();
        Ok(())
    }

    /// Called by the window timer only while work is pending.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(result) = self.inventory.as_mut().and_then(TaskHandle::try_take) {
            self.inventory = None;
            match result {
                Ok(Ok(entries)) => for e in entries { if !self.entries.iter().any(|old| old.id == e.id) { self.entries.push(e); } },
                Ok(Err(e)) => self.status = e.to_string(),
                Err(e) => self.status = e.to_string(),
            }
            changed = true;
        }
        self.imports.retain_mut(|task| {
            let Some(result) = task.try_take() else { return true };
            match result {
                Ok(result) => {
                    for e in result.entries {
                        self.draft.ids.push(e.id.clone());
                        if !self.entries.iter().any(|old| old.id == e.id) { self.entries.push(e); }
                    }
                    self.status = result.problems.join("\n");
                }
                Err(e) => self.status = e.to_string(),
            }
            changed = true;
            false
        });
        if let Some(sending) = &mut self.sending {
            if let Some(p) = sending.progress.try_iter().last() {
                self.status = format!("{} · {} of {} bytes", p.file, p.bytes, p.total);
                changed = true;
            }
            if let Some(result) = sending.task.try_take() {
                self.sending = None;
                match result {
                    Ok((_, Ok(post))) => {
                        self.clear();
                        self.url = post.url;
                        self.status = format!("Published {}", post.folder);
                    }
                    Ok((job, Err(e))) => { self.retry = job.map(|job| (self.draft.clone(), job)); self.status = e; }
                    Err(e) => self.status = e.to_string(),
                }
                changed = true;
            }
        }
        changed
    }
}
