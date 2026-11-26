//! The model strand owns the active job. Switching the bed installs a sink for
//! the new document while keeping that job's original call bound.
use super::*;
use fab_elec_design::By;
use fab_model::{FabModel, JournalEvent, State};
use makepad_widgets::makepad_platform::thread::ThreadSpawner;

/// Moves from a panel job to its eventual UI reply, including reads delegated to
/// the pool. Dropping the last owner without delivering the reply cancels it.
pub(crate) struct PersonReply(Arc<Call>);

impl std::fmt::Debug for PersonReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("PersonReply") }
}

impl PersonReply {
    pub(crate) fn begin(plate: &Plate, tool: &str, args: Value) -> Work {
        let call = Call::begin(plate, By::Person, tool, args);
        let mut work = Work::new(By::Person, Some(&call));
        work.person = Some(Arc::new(Self(call)));
        work
    }

    pub(crate) fn receive(&self, ok: bool, text: &str) {
        let outcome = if ok { "ok" } else if crate::model_host::reason(text) == "cancelled" { "cancelled" } else { "refused" };
        self.0.done(outcome, text.into());
    }
}

impl Drop for PersonReply {
    fn drop(&mut self) { self.0.done("cancelled", "cancelled".into()); }
}

#[derive(Default)]
pub(super) struct Saved {
    manifest: String,
    files: Files,
    first: bool,
}

impl Saved {
    pub(super) fn opened(manifest: &str, files: &Files, state: &State) -> Self {
        let saved = files.iter().filter(|(name, _)| name.as_str() == manifest || state.scripts.contains_key(*name) || state.strokes.contains_key(*name))
            .map(|(name, bytes)| (name.clone(), bytes.clone())).collect();
        Self { manifest: manifest.to_owned(), files: saved, first: true }
    }

    fn update(&mut self, name: &str, bytes: &[u8], text: bool, changes: &mut Vec<Change>) {
        let before = self.files.get(name);
        if before.is_some_and(|old| old.as_ref() == bytes) { return }
        let after: Arc<[u8]> = Arc::from(bytes);
        let prior = before.map(AsRef::as_ref);
        let change = match (text, prior) {
            (true, Some(prior)) => match (std::str::from_utf8(prior), std::str::from_utf8(bytes)) {
                (Ok(old), Ok(new)) => Change::text(name.into(), old, new),
                _ => Change::whole(name.into(), Some(prior), Some(after.clone())),
            },
            _ => Change::whole(name.into(), prior, Some(after.clone())),
        };
        changes.push(change);
        self.files.insert(name.into(), after);
    }

    pub(super) fn changes(&mut self, old: &State, model: &FabModel) -> Vec<Change> {
        let mut changes = Vec::new();
        let new = model.state();
        self.files.retain(|name, bytes| {
            if *name == self.manifest || new.scripts.contains_key(name) || new.strokes.contains_key(name) { return true }
            changes.push(Change::whole(name.clone(), Some(bytes), None));
            false
        });
        self.update(&self.manifest.clone(), model.project_text().as_bytes(), true, &mut changes);
        for (name, text) in &new.scripts {
            if self.first || old.scripts.get(name) != Some(text) { self.update(name, text.as_bytes(), true, &mut changes); }
        }
        for (name, log) in new.strokes.iter() {
            if self.first || !old.strokes.get(name).is_some_and(|prior| Arc::ptr_eq(prior, log)) {
                self.update(name, &log.to_bytes(), false, &mut changes);
            }
        }
        self.first = false;
        changes
    }
}

#[derive(Clone, Default)]
pub(crate) struct ModelJournal(Arc<Mutex<Option<Work>>>);

impl ModelJournal {
    pub(crate) fn install(&self, session: &mut fab_ai::Session, spawner: Option<ThreadSpawner>) {
        let context = self.clone();
        let opened = move |session: &mut fab_ai::Session| {
            let sink = Plate::opened(session).map(|plate| {
                if let Some(spawner) = &spawner { plate.attach(spawner); }
                Arc::new(Sink { context: context.clone(), plate }) as Arc<dyn fab_model::JournalSink>
            });
            session.model.set_journal(sink);
        };
        opened(session);
        session.on_model_open = Some(Arc::new(opened));
    }

    pub(crate) fn during<T>(&self, work: Option<Work>, run: impl FnOnce() -> T) -> T {
        struct Restore<'a> { current: &'a Mutex<Option<Work>>, previous: Option<Work> }
        impl Drop for Restore<'_> {
            fn drop(&mut self) { *self.current.lock().unwrap() = self.previous.take(); }
        }
        let previous = std::mem::replace(&mut *self.0.lock().unwrap(), work);
        let _restore = Restore { current: &self.0, previous };
        run()
    }

    pub(crate) fn person(&self) -> Option<Arc<PersonReply>> {
        self.0.lock().unwrap().as_ref().and_then(|work| work.person.clone())
    }
}

struct Sink {
    context: ModelJournal,
    plate: Plate,
}

impl fab_model::JournalSink for Sink {
    fn receive(&self, event: JournalEvent<'_>, old: &State, model: &FabModel) {
        let changes = self.plate.mechanical_changes(old, model);
        match event {
            JournalEvent::Commit { seq, label } => {
                let work = self.context.0.lock().unwrap().clone();
                let call = work.as_ref().and_then(|w| w.journal.as_ref()).map(|binding| binding.0.as_ref());
                self.plate.effect(call, seq, work.as_ref().map_or(By::Person, |w| w.by), label, changes);
            }
            JournalEvent::History { undo, before, after } => {
                self.plate.send(Record::History { undo, before, after, changes });
            }
        }
    }
}
