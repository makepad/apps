//! The page's side of the drafts store (`drafts.rs`): which draft the open
//! edit is, writing it a second after each change, reopening the last one at
//! start exactly where it was, and the drafts listed under Edits.

use makepad_library_browser::drafts::{self, Draft, DraftJob, DraftMeta, DraftReply, Drafts};
use makepad_library_browser::list::{CardKind, ListRow};
use makepad_library_browser::lanes::LibLane;
use super::timeline::CutTimeline;
use super::{PageLane, VideoCut};
use makepad_widgets::*;

#[derive(Default)]
pub struct Autosave {
    drafts: Option<Drafts>,
    /// The open cut's draft.
    pub id: Option<String>,
    /// The id the next opened document takes (a draft reopened).
    pub next_id: Option<String>,
    /// Where to put the page when the next document opens.
    restore: Option<DraftMeta>,
    /// What the last write held: revision and page state.
    written: Option<(u64, String)>,
    /// When the unwritten change was first seen.
    since: Option<f64>,
    reported: Option<usize>,
    /// Every draft on disk, newest first.
    pub list: Vec<DraftMeta>,
    /// A draft card asked to be deleted once; a second Delete does it.
    armed_delete: Option<String>,
    loaded: bool,
    /// When the open cut's draft was last on disk.
    pub saved_at: Option<f64>,
    /// The page state the last pump saw, for the write at quit.
    latest: Option<DraftMeta>,
}

fn lane_name(lane: Option<PageLane>) -> &'static str {
    match lane {
        Some(PageLane::Store(LibLane::Video)) | None => "video",
        Some(PageLane::Store(LibLane::Image)) => "images",
        Some(PageLane::Store(LibLane::Audio)) => "audio",
        Some(PageLane::Store(LibLane::Cuts)) => "edits",
        Some(PageLane::Store(LibLane::AppRuns)) => "app_runs",
        Some(PageLane::Store(LibLane::Decks)) => "decks",
        Some(PageLane::Store(LibLane::Scores)) => "scores",
        Some(PageLane::Store(LibLane::Samples)) => "samples",
        Some(PageLane::Store(LibLane::SoundFonts)) => "soundfonts",
        Some(PageLane::Store(LibLane::Voices)) => "voices",
        Some(PageLane::Web) => "web",
        Some(PageLane::Effects) => "effects",
        Some(PageLane::Transitions) => "transitions",
    }
}

fn lane_of(name: &str) -> Option<PageLane> {
    Some(match name {
        "video" => PageLane::Store(LibLane::Video),
        "images" => PageLane::Store(LibLane::Image),
        "audio" => PageLane::Store(LibLane::Audio),
        "edits" => PageLane::Store(LibLane::Cuts),
        "app_runs" => PageLane::Store(LibLane::AppRuns),
        "decks" => PageLane::Store(LibLane::Decks),
        "scores" => PageLane::Store(LibLane::Scores),
        "web" => PageLane::Web,
        "effects" => PageLane::Effects,
        "transitions" => PageLane::Transitions,
        _ => return None,
    })
}

impl VideoCut {
    pub(super) fn start_autosave(&mut self, cx: &mut Cx) {
        match Drafts::new(cx) {
            Ok(store) => {
                store.send(DraftJob::Load);
                self.autosave.drafts = Some(store);
            }
            Err(error) => log!("edit: drafts unavailable, edits are not kept across restarts: {error}"),
        }
    }

    /// Stage is closing (or quit outright): the open cut is written here,
    /// on this thread, since nothing may run after this. A few KB, once.
    pub(super) fn stop_autosave(&mut self) {
        let Some(store) = &mut self.autosave.drafts else { return };
        store.stop();
        self.autosave.drafts = None;
        if self.folder.is_some() { return; }
        let revision = self.session.as_ref().map_or(0, |session| session.revision);
        let (Some(mut meta), Some(session)) = (self.autosave.latest.clone(), self.session.as_ref()) else { return };
        meta.edit_stamp = super::motion_edit::draft_stamp(self);
        meta.dirty = Self::unsaved(session) || super::motion_edit::has_drafts(self);
        meta.head = session.head;
        if let Some(tree) = &mut meta.tree {
            if let Some(current) = &session.tree { tree.draft = current.clone(); }
            tree.path = session.path.clone();
            tree.saved_path = session.saved_path.clone();
            super::motion_edit::capture_draft(self, tree);
        }
        let key = Self::draft_key(&meta);
        let dir = drafts::dir();
        if !self.autosave.written.as_ref().is_some_and(|(written, written_key)| *written == revision && *written_key == key) {
            meta.updated = drafts::now_ms();
            if let Err(error) = drafts::write(&dir, &Draft { meta: meta.clone(), source: session.source.clone() }) {
                log!("edit: draft not written at quit: {error}");
                return;
            }
        }
        log!("edit: unsaved changes: 0 (draft {} in {})", meta.id, dir.display());
    }

    /// The page's state as a draft of the open cut (its meta; the source
    /// is copied only to write it).
    fn draft_meta(&self, cx: &mut Cx, ui: &WidgetRef) -> Option<DraftMeta> {
        // A folder cut's files are its work: no draft beside them.
        if self.folder.is_some() { return None; }
        let session = self.session.as_ref()?;
        let id = self.autosave.id.clone()?;
        let (px_per_sec, scroll) = ui.widget(cx, ids!(cut_timeline)).borrow::<CutTimeline>().map_or((0.0, 0.0), |timeline| timeline.view());
        Some(DraftMeta {
                id,
                label: session.doc.label.clone(),
                head: session.head,
                tree: session.tree.as_ref().zip(session.alias.as_ref()).map(|(tree, alias)| drafts::DraftTree { draft: tree.clone(), path: session.path.clone(), saved_path: session.saved_path.clone(), alias: alias.clone(), pending: Vec::new(), selected: super::motion_edit::selected_file(self).into() }),
                edit_stamp: super::motion_edit::draft_stamp(self),
                dirty: Self::unsaved(session) || (session.tree.is_some() && super::motion_edit::has_drafts(self)),
                playhead: (self.playhead * 1000.0).round() / 1000.0,
                selected: self.selected.clone(),
                lane: lane_name(self.lib.lane).into(),
                px_per_sec: (px_per_sec * 100.0).round() / 100.0,
                scroll: (scroll * 1000.0).round() / 1000.0,
                updated: 0,
        })
    }

    /// Work the library does not have: changes since the last save, or a
    /// new cut with something in it (an untouched new cut is not work).
    fn unsaved(session: &super::Session) -> bool {
        match session.head {
            Some(_) => session.dirty(),
            None => session.doc.nodes.len() > 2,
        }
    }

    fn draft_key(m: &DraftMeta) -> String {
        format!("{}|{}|{}|{:?}|{}|{}|{}|{:?}|{}|{:?}", m.id, m.dirty, m.playhead, m.selected, m.lane, m.px_per_sec, m.scroll, m.head, m.edit_stamp, m.tree.as_ref().map(|tree| (&tree.path, &tree.saved_path, &tree.selected)))
    }

    /// Write the open cut's draft now if anything changed since the last.
    pub(super) fn write_draft(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.write_draft_logged(cx, ui, false);
    }

    /// True when a write was sent.
    fn write_draft_logged(&mut self, cx: &mut Cx, ui: &WidgetRef, log: bool) -> bool {
        let Some(meta) = self.draft_meta(cx, ui) else { return false };
        self.write_meta(meta, log)
    }

    fn write_meta(&mut self, mut meta: DraftMeta, log: bool) -> bool {
        let revision = self.session.as_ref().map_or(0, |session| session.revision);
        let key = Self::draft_key(&meta);
        if self.autosave.written.as_ref().is_some_and(|(written, written_key)| *written == revision && *written_key == key) {
            return false;
        }
        let Some(store) = &self.autosave.drafts else { return false };
        let Some(source) = self.session.as_ref().map(|session| session.source.clone()) else { return false };
        meta.updated = drafts::now_ms();
        if let Some(tree) = &mut meta.tree { super::motion_edit::capture_draft(self, tree); }
        store.send(DraftJob::Write { draft: Draft { meta, source }, log });
        self.autosave.written = Some((revision, key));
        self.autosave.since = None;
        self.autosave.reported = Some(0);
        true
    }

    /// Every pump: replies, then the debounced write.
    pub(super) fn pump_autosave(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        while let Some(reply) = self.autosave.drafts.as_ref().and_then(|store| store.try_recv()) {
            match reply {
                DraftReply::Loaded { last, drafts } => {
                    self.autosave.loaded = true;
                    self.autosave.list = drafts;
                    // Launch: the last open cut, where it was.
                    if let (Some(last), None) = (last, &self.session) {
                        self.status = format!("Reopened “{}” where you left it", last.meta.label);
                        self.reopen_draft(last);
                    }
                    self.repaint_library = true;
                }
                DraftReply::Written { id, drafts } => {
                    if self.autosave.id.as_deref() == Some(id.as_str()) {
                        self.autosave.saved_at = Some(Cx::time_now());
                    }
                    let changed = drafts.iter().map(|meta| (&meta.id, meta.dirty)).ne(self.autosave.list.iter().map(|meta| (&meta.id, meta.dirty)));
                    self.autosave.list = drafts;
                    if changed {
                        self.repaint_library = true;
                    }
                }
                DraftReply::Deleted { id, drafts } => {
                    let label = self.autosave.list.iter().find(|meta| meta.id == id).map_or(id, |meta| meta.label.clone());
                    self.autosave.list = drafts;
                    self.lib.list_selected = None;
                    self.status = format!("Draft “{label}” deleted");
                    self.repaint_library = true;
                    self.repaint = true;
                }
                DraftReply::Failed(error) => {
                    log!("edit: {error}");
                    self.status = format!("Autosave: {error}");
                    self.repaint = true;
                }
            }
        }
        if !self.autosave.loaded {
            return;
        }
        let Some(meta) = self.draft_meta(cx, ui) else { return };
        let revision = self.session.as_ref().map_or(0, |session| session.revision);
        let key = Self::draft_key(&meta);
        self.autosave.latest = Some(meta.clone());
        let unchanged = self.autosave.written.as_ref().is_some_and(|(written, written_key)| *written == revision && *written_key == key);
        if unchanged {
            return;
        }
        let now = Cx::time_now();
        let since = *self.autosave.since.get_or_insert(now);
        let unsaved = self.autosave.written.as_ref().map_or(1, |(written, _)| (revision.saturating_sub(*written)) as usize);
        if unsaved > 0 && self.autosave.reported != Some(unsaved) {
            // What a restart checks first (state.json, the log).
            if let Some(store) = &self.autosave.drafts {
                store.send(DraftJob::Pending { unsaved, log: false });
            }
            self.autosave.reported = Some(unsaved);
        }
        if now - since >= drafts::DEBOUNCE {
            self.write_meta(meta, false);
        }
    }

    /// Open a draft, the page to follow it once it loads.
    pub(super) fn reopen_draft(&mut self, draft: Draft) {
        self.autosave.next_id = Some(draft.meta.id.clone());
        let saved = !draft.meta.dirty && draft.meta.head.is_some();
        self.crumbs.clear();
        if let Some(tree) = draft.meta.tree.clone() {
            let Some(head) = draft.meta.head else { self.status = "The tree draft is missing its saved head".into(); return; };
            self.send(super::store::StoreJob::RestoreTree { head, tree });
        } else {
            self.open_source(draft.source, draft.meta.head, saved);
        }
        self.autosave.restore = Some(draft.meta);
    }

    /// A document opened: it is the draft named for it (a new id for a new
    /// edit), and a reopened draft puts the page back. False when the
    /// timeline should fit the cut as usual.
    pub(super) fn opened_draft(&mut self, cx: &mut Cx, ui: &WidgetRef) -> bool {
        let head = self.session.as_ref().and_then(|session| session.head);
        let id = self.autosave.next_id.take().unwrap_or_else(|| head.map_or_else(drafts::new_id, |head| drafts::id_of(&head)));
        self.autosave.id = Some(id.clone());
        self.autosave.written = None;
        self.autosave.since = None;
        let Some(meta) = self.autosave.restore.take().filter(|meta| meta.id == id) else { return false };
        if let Some(tree) = meta.tree.clone() { super::motion_edit::restore_draft(self, cx, ui, tree); }
        // Written as it was: nothing to write until something changes.
        self.playhead = meta.playhead.clamp(0.0, self.duration());
        self.selected = meta.selected.filter(|selected| self.session.as_ref().is_some_and(|session| session.doc.node(super::split_selection(selected).0).is_some()));
        if let Some(lane) = lane_of(&meta.lane) {
            self.set_lane(cx, ui, lane);
        }
        let restored = meta.px_per_sec > 0.0;
        if restored {
            if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<CutTimeline>() {
                timeline.set_view(cx, meta.px_per_sec, meta.scroll, self.playhead);
            }
            ui.slider(cx, ids!(cut_zoom)).set_value(cx, super::zoom_to_slider(meta.px_per_sec));
        }
        self.repaint = true;
        self.repaint_preview = true;
        restored
    }

    /// The open cut's name when it has unsaved work (a New or an open
    /// keeps it in Drafts).
    pub(super) fn kept_label(&self) -> Option<String> {
        self.session.as_ref().filter(|session| self.folder.is_none() && Self::unsaved(session)).map(|session| session.doc.label.clone())
    }

    /// Before another document replaces the open one: its draft is written
    /// (nothing is discarded), and the status says where it is.
    pub(super) fn keep_draft(&mut self, cx: &mut Cx, ui: &WidgetRef) -> Option<String> {
        let label = self.session.as_ref().filter(|session| session.dirty() || session.head.is_none()).map(|session| session.doc.label.clone());
        self.write_draft(cx, ui);
        label
    }

    /// Drafts listed under Edits: unsaved work, newest first.
    pub(super) fn draft_rows(&self) -> Vec<(DraftMeta, ListRow)> {
        if self.lib.lane != Some(PageLane::Store(LibLane::Cuts)) {
            return Vec::new();
        }
        // Only drafts of cuts never saved: a saved cut's unsaved changes
        // mark its own row (EDITED), never add or move one. Oldest first by
        // creation, so editing one does not reorder the list.
        let mut drafts: Vec<&DraftMeta> = self.autosave.list.iter().filter(|meta| meta.dirty && meta.head.is_none()).collect();
        drafts.sort_by(|a, b| a.id.cmp(&b.id));
        drafts.into_iter().map(|meta| {
            let open = self.autosave.id.as_deref() == Some(meta.id.as_str());
            let row = ListRow {
                title: meta.label.clone(),
                detail: if open { "Draft · open now".into() } else { format!("Draft · {}", super::modified(meta.updated)) },
                kind: CardKind::Cut,
                badge: Some(("DRAFT".into(), 0x5a4312ff, 0xffd58aff)),
                ..ListRow::default()
            };
            (meta.clone(), row)
        }).collect()
    }

    /// A saved cut with unsaved changes in its draft (its row wears EDITED).
    pub(super) fn edited(&self, asset: &str) -> bool {
        self.autosave.list.iter().any(|meta| meta.dirty && meta.head.is_some_and(|head| head.asset_id.to_string() == asset))
    }

    /// The draft behind list card `index`, when it is one (drafts follow
    /// the library's edits).
    pub(super) fn draft_at(&self, index: usize) -> Option<DraftMeta> {
        let first = self.lib.rows.len();
        index.checked_sub(first).and_then(|at| self.draft_rows().into_iter().nth(at)).map(|(meta, _)| meta)
    }

    pub(super) fn click_draft(&mut self, meta: DraftMeta) {
        self.status = format!("Draft “{}” · double-click opens it · Delete (twice) deletes it", meta.label);
        self.repaint = true;
    }

    pub(super) fn open_draft(&mut self, cx: &mut Cx, ui: &WidgetRef, meta: DraftMeta) {
        if self.autosave.id.as_deref() == Some(meta.id.as_str()) {
            self.status = format!("“{}” is open", meta.label);
            self.repaint = true;
            return;
        }
        let kept = self.keep_draft(cx, ui);
        match drafts::read(&drafts::dir(), &meta.id) {
            Some(draft) => {
                self.status = match kept {
                    Some(label) => format!("Opened draft “{}” · “{label}” is kept in Drafts", meta.label),
                    None => format!("Opened draft “{}”", meta.label),
                };
                self.reopen_draft(draft);
            }
            None => self.status = format!("Draft “{}” is gone", meta.label),
        }
        self.repaint = true;
    }

    /// Delete on a draft card: once arms, twice deletes (it cannot be undone).
    pub(super) fn delete_draft(&mut self, meta: DraftMeta) {
        if self.autosave.id.as_deref() == Some(meta.id.as_str()) {
            self.status = "This draft is open: open or start another cut first".into();
        } else if self.autosave.armed_delete.as_deref() == Some(meta.id.as_str()) {
            self.autosave.armed_delete = None;
            if let Some(store) = &self.autosave.drafts {
                store.send(DraftJob::Delete(meta.id));
            }
        } else {
            self.status = format!("Delete draft “{}”? Its unsaved changes are lost · press Delete again", meta.label);
            self.autosave.armed_delete = Some(meta.id);
        }
        self.repaint = true;
    }
}
