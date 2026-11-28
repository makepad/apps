//! Visual issues in one project. Drafts keep their destination and note through a window link,
//! a grab, and the store receipt, even when another project or issue is opened.
use super::marked_picture::{MarkedPicture, MarkedPictureAction, MarkTool};
use crate::{
    app_link::{report::{Report, Recording}, AppLinkEvent, AppLinks, IssueStill},
    call::{ShareChoice, ShareRow},
    lists::{Page, Pane, ViewData},
    marks::{self, Head, Mark},
    model::{issues::{IssueEdit, IssueProject}, Action as ModelAction, Model, Origin},
    tweak::overlay::seat_colour,
    record::capture::{Purpose, StillCapture, WindowPicker, WindowTarget},
    store_worker::{IssueMedia, IssueReceipt, StoredFile},
    view::LocalClock,
};
use makepad_town_core::{blob, entity::{field, kind, Entity}, event::{Id, Key, SessionId, Value}, key::PublicKey};
use makepad_widgets::*;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, mpsc::{Receiver, TryRecvError}}};

pub(super) fn page_template(vm: &mut ScriptVm) -> ScriptValue {
    let page = IssuePage::register_widget(vm);
    let row = IssueRow::register_widget(vm);
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*

        #(page){
            width: Fill height: Fill flow: Down spacing: 6 padding: 10
            report_setting := BodyNote{visible: false width: Fill
                text: "Reports from apps need the developer setting (MAKEPAD_REMOTE=local)"}
            index := View{
                width: Fill height: Fill flow: Down spacing: 6
                share_issue := PanelButton{text: "Share visual issue"}
                choices := ScrollYView{
                    visible: false width: Fill height: Fit{max: FitBound.Rel{base: Base.Unused factor: 1.0}} flow: Down
                    FlatList{width: Fill height: Fit
                        Choice := PanelButton{width: Fill height: 26}
                        Empty := BodyNote{width: Fill}
                    }
                }
                issue_list := PortalList{width: Fill height: Fill
                    Row := #(row){
                        width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                        poster := Image{width: 80 height: 56 fit: ImageFit.Smallest}
                        body := View{width: Fill height: Fit flow: Down
                            open := PanelButton{width: Fill}
                            meta := BodyNote{width: Fill}
                        }
                    }
                }
            }
            detail := View{
                visible: false width: Fill height: Fill flow: Down spacing: 6
                issue_back := PanelButton{text: "Issues"}
                picture_box := View{width: Fill height: Fill flow: Overlay
                    picture := MarkedPicture{}
                    issue_player := VideoPlayer{visible: false width: Fill height: Fill}
                    placeholder := BodyNote{text: "Waiting for picture…"}
                }
                issue_play := PanelButton{visible: false text: "Play recording"}
                window_label := BodyNote{width: Fill}
                issue_input := PanelInput{width: Fill height: 80 is_multiline: true submit_on_enter: false
                    empty_text: "Add a note…" a11y: {label: "Issue note"}}
                read_note := ScrollYView{width: Fill height: Fit{max: FitBound.Rel{base: Base.Unused factor: 0.5}} flow: Down
                    issue_note := BodyNote{width: Fill}
                }
                problem := BodyNote{visible: false width: Fill}
                controls := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 6
                    draw_mark := Toggle{text: "Draw"}
                    arrow_mark := Toggle{text: "Arrow"}
                    undo_mark := PanelButton{text: "Undo"}
                    issue_pin := PanelButton{text: "Pin to board"}
                    issue_status := PanelButton{text: "Resolve"}
                    issue_delete := PanelButton{text: "Delete"}
                }
            }
        }
    })
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
enum Open { Draft(u64), Filed(Id) }

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
struct WindowChoice(u64);

struct Line {
    open: Open,
    first: String,
    meta: String,
    poster: Option<PathBuf>,
}

enum Capture {
    Linking(WindowPicker),
    Still(StillCapture),
    Storing,
    Failed(String),
}

enum RecordingState {
    Waiting(Receiver<Option<Recording>>),
    Storing,
    Ready(Option<(StoredFile, i64)>),
}

struct Draft {
    project: IssueProject,
    session: Option<SessionId>,
    source_peer: Option<PublicKey>,
    note: String,
    label: String,
    size: [u32; 2],
    capture: Capture,
    pictures: Option<[StoredFile; 2]>,
    recording: RecordingState,
    marks: Vec<(i32, Head, Vec<[i32; 2]>)>,
}

impl Draft {
    fn poll_recording(&mut self, id: u64, model: &mut Model, closing: bool) -> Result<bool, String> {
        let RecordingState::Waiting(inbox) = &self.recording else { return Ok(false) };
        self.recording = match inbox.try_recv() {
            Ok(Some(video)) => {
                model.store_issue_video(id, self.project.town, video)?;
                RecordingState::Storing
            }
            Err(TryRecvError::Empty) if !closing => return Ok(false),
            _ => RecordingState::Ready(None),
        };
        Ok(true)
    }

    fn advance(&mut self, id: u64, links: &mut AppLinks, events: &[AppLinkEvent], model: &mut Model) -> Result<bool, String> {
        let still = match &mut self.capture {
            Capture::Linking(picker) => {
                let (_, picked) = picker.poll(links, events, &mut None);
                let Some((_, target)) = picked else { return Ok(false) };
                let WindowTarget::Linked(link, window) = target? else { return Err("Choose a running app window.".into()) };
                self.capture = Capture::Still(StillCapture::grab(links, link, window, &mut self.label)?);
                return Ok(true)
            }
            Capture::Still(capture) => {
                let Some(result) = capture.poll() else { return Ok(false) };
                result?
            }
            _ => return Ok(false),
        };
        self.size = still.size;
        model.store_issue(id, self.project.town, still)?;
        self.capture = Capture::Storing;
        Ok(true)
    }
}

#[derive(Default)]
struct ShownMarks {
    target: Option<Open>,
    stamp: (usize, u64),
    coloured: Arc<[(Mark, Vec4f)]>,
    newest_own: Option<Id>,
}

impl ShownMarks {
    fn sync(&mut self, model: &Model, open: Open, draft: Option<&Draft>) {
        let stamp = model.issue_fold_stamp();
        if self.target == Some(open) && self.stamp == stamp { return }
        let me = model.me();
        let changed_target = self.target != Some(open);
        let marks = match open {
            Open::Filed(id) if changed_target || model.issue_marks_touched(self.stamp, id, |id|
                self.coloured.iter().any(|(m, _)| m.id == *id)) => Some(marks::marks_on(&model.state, id)),
            Open::Draft(_) if changed_target => draft.map(|d| d.marks.iter().map(|(width, head, pts)|
                Mark { id: [0; 32], author: me, width: *width, head: *head, pts: pts.as_slice().into() }).collect()),
            _ => None,
        };
        let colour = |m: &Mark| seat_colour(&model.state, &m.author);
        if let Some(marks) = marks {
            self.coloured = marks.into_iter().map(|m| { let ink = colour(&m); (m, ink) }).collect();
        } else if self.coloured.iter().any(|(m, ink)| colour(m) != *ink) {
            for (m, ink) in Arc::make_mut(&mut self.coloured) { *ink = colour(m); }
        }
        let seat = model.state.seat_of(&me);
        self.newest_own = self.coloured.iter().rev().find(|(m, _)| seat.is_some() && model.state.seat_of(&m.author) == seat)
            .map(|(m, _)| m.id);
        self.target = Some(open);
        self.stamp = stamp;
    }
}

#[derive(Default)]
pub struct Issues {
    project: Option<IssueProject>,
    /// Keep this share's subscription and preview while its Issues page is open.
    source_share: Option<SessionId>,
    open: Option<Open>,
    picker: WindowPicker,
    picker_changed: bool,
    choices: Option<Arc<[ShareRow]>>,
    next: u64,
    drafts: BTreeMap<u64, Draft>,
    lines: Vec<Line>,
    list_revision: Option<usize>,
    edits: BTreeMap<Id, String>,
    timer: Timer,
    focus: bool,
    marks: ShownMarks,
    tool: MarkTool,
    error: Option<String>,
    pending_pin: Option<(Id, Id)>,
    play_requested: Option<Id>,
    playing: Option<MovieSource>,
    preparing: Option<TaskHandle<Result<MovieSource, String>>>,
    closing: bool,
}

/// Some native players need the MP4 suffix. Like the chat player, use a hard link to the
/// verified blob. Both creating and retiring the link belong to the pool.
struct MovieSource {
    path: PathBuf,
    cleanup: Option<TaskPool>,
}

impl MovieSource {
    fn prepare(path: PathBuf, pool: TaskPool) -> Result<Self, String> {
        use std::{fs, io::Read, sync::atomic::{AtomicUsize, Ordering}};
        let mut head = [0; 12];
        fs::File::open(&path).and_then(|mut f| f.read_exact(&mut head)).map_err(|_| "Cannot read the issue recording.")?;
        if crate::store_worker::CaptureFormat::sniff(&head) != Some(crate::store_worker::CaptureFormat::Mp4) {
            return Err("This recording is not an MP4.".into());
        }
        let dir = path.parent().ok_or("Missing recording folder.")?.join("playback");
        if fs::create_dir_all(&dir).is_ok() {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            loop {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let link = dir.join(format!("issue-{}-{id}.mp4", std::process::id()));
                match fs::hard_link(&path, &link) {
                    Ok(()) => return Ok(Self { path: link, cleanup: Some(pool) }),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(_) => break,
                }
            }
        }
        Ok(Self { path, cleanup: None })
    }
}

impl Drop for MovieSource {
    fn drop(&mut self) {
        if let Some(pool) = self.cleanup.take() {
            let link = std::mem::take(&mut self.path);
            if let Ok(job) = pool.submit_named(Class::Background, "Town close issue recording", move || {
                if let Err(e) = std::fs::remove_file(link) { log!("town: removing issue playback link: {e}"); }
            }) { job.detach(); }
        }
    }
}

fn blob_id(e: &Entity, field: Key) -> Option<Id> {
    match e.value(field)? { Value::Blob { size, root } => Some(blob::id(*size, root)), _ => None }
}

fn local_picture(model: &Model, id: Id) -> Option<PathBuf> {
    let town = model.open_id();
    model.complete_blobs(&town)?.contains(&id).then(|| blob::data_path(&model.home.blobs_dir().join(makepad_town_core::hex(&town)), &id))
}

fn image_path(cx: &mut Cx, image: ImageRef, shown: &mut Option<PathBuf>, path: Option<&Path>) {
    if shown.as_deref() == path { return }
    image.set_texture(cx, None);
    if let Some(p) = path { let _ = image.load_image_file_by_path_async(cx, p); }
    *shown = path.map(Path::to_owned);
}

impl Issues {
    fn start_draft(&mut self, project: IssueProject, label: String, session: Option<SessionId>, source_peer: Option<PublicKey>,
        capture: Capture, done: Option<Receiver<Option<Recording>>>) {
        self.next += 1;
        self.project = Some(project);
        self.source_share = session;
        self.drafts.insert(self.next, Draft { project, session, source_peer, label, note: String::new(), size: [0, 0], capture, pictures: None, marks: Vec::new(),
            recording: done.map(RecordingState::Waiting).unwrap_or(RecordingState::Ready(None)) });
        self.list_revision = None;
        self.choices = None;
        self.open = Some(Open::Draft(self.next));
        self.tool = MarkTool::Off;
        self.error = None;
        self.focus = true;
    }

    pub fn closing(&mut self, cx: &mut Cx, page: &WidgetRef) {
        self.closing = true;
        self.play_requested = None;
        self.preparing = None;
        if self.playing.is_some() { page.video_player(cx, ids!(issue_player)).stop(cx); }
        self.playing = None;
    }

    pub fn busy(&self) -> bool { self.drafts.values().any(|d| !matches!(d.capture, Capture::Failed(_))) }

    fn select(&mut self, cx: &mut Cx, model: &mut Model, open: Option<Open>) {
        self.flush(cx, model);
        self.list_revision = None;
        self.picker.close();
        self.choices = None;
        self.open = open;
        self.tool = MarkTool::Off;
        self.marks = ShownMarks::default();
        self.focus = false;
        self.error = None;
    }

    pub fn show_index(&mut self, cx: &mut Cx, model: &mut Model) { self.select(cx, model, None); }

    pub fn holdings_changed(&mut self) { self.list_revision = None; }

    pub fn flush(&mut self, cx: &mut Cx, model: &mut Model) {
        cx.stop_timer(std::mem::take(&mut self.timer));
        let (Some(project), Some(Open::Filed(id))) = (self.project, self.open) else { return };
        let Some(note) = self.edits.get(&id) else { return };
        match model.change_issue(project, id, IssueEdit::Note(note.clone())) {
            Ok(()) => { self.edits.remove(&id); self.error = None; }
            Err(why) => self.error = Some(why),
        }
    }

    pub fn timeout(&mut self, cx: &mut Cx, event: &Event, model: &mut Model) -> bool {
        if self.timer.is_event(event).is_none() { return false }
        self.flush(cx, model);
        true
    }

    pub fn sync(&mut self, cx: &mut Cx, ui: &WidgetRef, project: Option<Id>, model: &mut Model, clock: &LocalClock) {
        let issue_project = project.map(|project| IssueProject { town: model.open_id(), project });
        if issue_project != self.project {
            self.select(cx, model, None);
            self.project = issue_project;
            self.source_share = None;
            self.lines.clear();
        }
        let page = ui.widget(cx, ids!(issues_page));
        let writable = model.state.active_member(&model.me());
        let entity = match self.open {
            Some(Open::Filed(id)) => model.state.entities.get(&id).filter(|e| e.kind == kind::ISSUE && e.project == project && !e.flag(field::DELETED)),
            _ => None,
        };
        if matches!(self.open, Some(Open::Filed(_))) && entity.is_none() { self.open = None; }
        let current = match self.open { Some(Open::Filed(id)) => Some(id), _ => None };
        if self.play_requested != current { self.play_requested = None; }
        if self.play_requested.is_none() {
            self.preparing = None;
        }
        if self.playing.is_some() && self.play_requested.is_none() {
            page.video_player(cx, ids!(issue_player)).stop(cx);
            self.playing = None;
        }
        let Some(issue_project) = issue_project else { return };
        page.widget(cx, ids!(report_setting)).set_visible(cx, !makepad_widgets::makepad_platform::remote::is_active());
        let draft = match self.open { Some(Open::Draft(id)) => self.drafts.get(&id), _ => None };
        let detail = self.open.is_some();
        page.widget(cx, ids!(index)).set_visible(cx, !detail);
        page.widget(cx, ids!(detail)).set_visible(cx, detail);
        page.widget(cx, ids!(choices)).set_visible(cx, !detail && self.picker.purpose.is_some());
        page.button(cx, ids!(share_issue)).set_enabled(cx, writable);
        if !detail && self.list_revision != Some(model.open_event_count()) {
            self.list_revision = Some(model.open_event_count());
            if let Some(mut picture) = page.widget(cx, ids!(picture)).borrow_mut::<MarkedPicture>() {
                picture.set_tool(cx, MarkTool::Off);
                picture.set_picture(cx, None);
                picture.set_marks(cx, self.marks.coloured.clone());
            }
            self.lines.clear();
            for (&id, draft) in self.drafts.iter().rev().filter(|(_, d)| d.project == issue_project) {
                self.lines.push(Line { open: Open::Draft(id), first: draft.note.lines().next().unwrap_or("").into(),
                    meta: match &draft.capture { Capture::Failed(why) => why.clone(),
                        _ if draft.pictures.is_some() => "Waiting for recording…".into(), _ => "Waiting for picture…".into() },
                    poster: draft.pictures.and_then(|files| local_picture(model, files[1].id)) });
            }
            for e in model.project_issues(issue_project.project) {
                let status = if e.text(field::STATUS) == "resolved" { "Resolved" } else { "Open" };
                let time = model.event(&e.id).map(|v| clock.hm(v.header.wall_ms)).unwrap_or_default();
                self.lines.push(Line { open: Open::Filed(e.id), first: e.text(field::TEXT).lines().next().unwrap_or("").into(),
                    meta: format!("{} · {time} · {status}", model.device_label(&e.author)),
                    poster: blob_id(e, field::POSTER).and_then(|id| local_picture(model, id)) });
            }
        }
        if let Some(open) = self.open {
            let own = draft.is_some() || entity.is_some_and(|e| model.state.seat_of(&e.author) == model.state.seat_of(&model.me()));
            let note = draft.map(|d| d.note.as_str()).or_else(|| entity.map(|e|
                self.edits.get(&e.id).map(String::as_str).unwrap_or(e.text(field::TEXT)))).unwrap_or("");
            let input = page.text_input(cx, ids!(issue_input));
            input.set_visible(cx, own && writable);
            if own && writable && input.text() != note { input.set_text(cx, note); }
            let plain = page.label(cx, ids!(issue_note));
            page.widget(cx, ids!(read_note)).set_visible(cx, !own || !writable);
            if !own || !writable { plain.set_text(cx, note); }
            let path = draft.and_then(|d| local_picture(model, d.pictures?[0].id)).or_else(|| entity.and_then(|e| [field::BLOB, field::POSTER].into_iter()
                .find_map(|field| local_picture(model, blob_id(e, field)?))));
            let video = entity.and_then(|e| blob_id(e, field::VIDEO));
            let button = page.button(cx, ids!(issue_play));
            button.set_visible(cx, video.is_some());
            if self.play_requested.is_some() && self.playing.is_none() {
                if self.preparing.is_none() {
                    if let Some(path) = video.and_then(|blob| local_picture(model, blob)) {
                        let pool = cx.task_pool();
                        match cx.task_pool().submit_named(Class::Background, "Town open issue recording", move || MovieSource::prepare(path, pool)) {
                            Ok(job) => self.preparing = Some(job),
                            Err(e) => { self.play_requested = None; self.error = Some(e.to_string()); }
                        }
                    }
                }
                if let Some(result) = self.preparing.as_mut().and_then(TaskHandle::try_take) {
                    self.preparing = None;
                    match result.map_err(|e| e.to_string()).and_then(|r| r) {
                        Ok(source) => {
                            page.video_player(cx, ids!(issue_player)).play_source(cx, VideoDataSource::Filesystem { path: source.path.to_string_lossy().into_owned() });
                            self.playing = Some(source);
                        }
                        Err(e) => { self.play_requested = None; self.error = Some(e); }
                    }
                }
            }
            let why = draft.and_then(|d| match &d.capture { Capture::Failed(why) => Some(why.as_str()), _ => None }).or(self.error.as_deref());
            let problem = page.label(cx, ids!(problem));
            problem.set_visible(cx, why.is_some());
            problem.set_text(cx, why.unwrap_or(""));
            button.set_text(cx, if self.playing.is_some() { "Close recording" }
                else if self.play_requested.is_some() { "Waiting for recording…" } else { "Play recording" });
            page.widget(cx, ids!(issue_player)).set_visible(cx, self.playing.is_some());
            page.widget(cx, ids!(picture)).set_visible(cx, self.playing.is_none());
            page.widget(cx, ids!(placeholder)).set_visible(cx, self.playing.is_none() && path.is_none() && why.is_none());
            let can_draw = writable && self.playing.is_none();
            self.marks.sync(model, open, draft);
            if let Some(mut picture) = page.widget(cx, ids!(picture)).borrow_mut::<MarkedPicture>() {
                picture.set_picture(cx, path.as_deref());
                picture.set_marks(cx, self.marks.coloured.clone());
                picture.ink = seat_colour(&model.state, &model.me());
                picture.set_tool(cx, if can_draw { self.tool } else { MarkTool::Off });
            }
            for (id, tool) in [(ids!(draw_mark), MarkTool::Pen), (ids!(arrow_mark), MarkTool::Arrow)] {
                let button = page.check_box(cx, id);
                button.set_visible(cx, can_draw);
                let active = self.tool == tool;
                if button.active(cx) != active { button.set_active(cx, active, Animate::No); }
            }
            let undo = page.button(cx, ids!(undo_mark));
            undo.set_visible(cx, can_draw);
            undo.set_enabled(cx, self.marks.newest_own.is_some());
            page.label(cx, ids!(window_label)).set_text(cx, draft.map(|d| d.label.as_str())
                .or_else(|| entity.map(|e| e.text(field::LABEL))).unwrap_or(""));
            page.widget(cx, ids!(controls)).set_visible(cx, writable);
            page.button(cx, ids!(issue_pin)).set_visible(cx, entity.and_then(|e| blob_id(e, field::BLOB)).is_some());
            page.widget(cx, ids!(issue_status)).set_visible(cx, entity.is_some());
            let resolved = entity.is_some_and(|e| e.text(field::STATUS) == "resolved");
            page.button(cx, ids!(issue_status)).set_text(cx, if resolved { "Reopen" } else { "Resolve" });
            page.widget(cx, ids!(issue_delete)).set_visible(cx, own && entity.is_some());
        }
        page.redraw(cx);
    }

    /// Called on the originating local input, like the window-share control.
    pub fn picker_input(&mut self, cx: &mut Cx, page: &WidgetRef, actions: &Actions, model: &Model) -> bool {
        let Some(project) = self.project else { return false };
        if self.open.is_some() || !model.state.active_member(&model.me()) { return false }
        if page.button(cx, ids!(share_issue)).clicked(actions) {
            self.picker.toggle(Purpose::Share);
            self.picker_changed = true;
            self.choices = None;
            return true
        }
        for a in actions.iter().filter_map(|a| a.as_widget_action()) {
            if !matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { continue }
            let Some(WindowChoice(key)) = a.data.as_ref().and_then(|d| d.downcast_ref()) else { continue };
            if !self.picker.choose(*key) { continue }
            self.picker_changed = true;
            let picker = std::mem::take(&mut self.picker);
            self.start_draft(project, String::new(), None, None, Capture::Linking(picker), None);
            return true
        }
        false
    }

    pub fn link_events(&mut self, links: &mut AppLinks, events: &[AppLinkEvent], signal: bool, model: &mut Model) -> bool {
        if !std::mem::take(&mut self.picker_changed) && events.is_empty() && !signal && !self.closing { return false }
        let (mut changed, _) = self.picker.poll(links, events, &mut self.choices);
        changed |= self.preparing.as_ref().is_some_and(TaskHandle::is_finished);
        let mut finished = Vec::new();
        for (&id, d) in &mut self.drafts {
            if matches!(d.capture, Capture::Failed(_)) { continue }
            match d.poll_recording(id, model, self.closing) {
                Ok(true) => { finished.push(id); changed = true; }
                Ok(false) => {}
                Err(why) => { d.capture = Capture::Failed(why); changed = true; continue }
            }
            match d.advance(id, links, events, model) {
                Ok(updated) => changed |= updated,
                Err(why) => { d.capture = Capture::Failed(why.replace(['\r', '\n'], " ")); self.list_revision = None; changed = true; }
            }
        }
        for id in finished { self.file_ready(model, id); }
        changed
    }

    pub fn stored(&mut self, model: &mut Model, (draft, result): IssueReceipt) {
        let Some(d) = self.drafts.get_mut(&draft) else { return };
        self.list_revision = None;
        match result {
            Ok(IssueMedia::Picture(files)) => d.pictures = Some(files),
            Ok(IssueMedia::Video { file, duration_ms }) => d.recording = RecordingState::Ready(Some((file, duration_ms))),
            Err(why) => d.capture = Capture::Failed(why),
        }
        self.file_ready(model, draft);
    }

    fn file_ready(&mut self, model: &mut Model, draft: u64) {
        let Some(d) = self.drafts.get_mut(&draft) else { return };
        if matches!(d.capture, Capture::Failed(_)) { return }
        let (Some(files), RecordingState::Ready(video)) = (d.pictures, &d.recording) else { return };
        self.list_revision = None;
        match model.file_issue(d.project, files, d.size, &d.label, &d.note, d.session, *video, d.source_peer, &d.marks) {
            Ok(id) => {
                self.drafts.remove(&draft);
                if self.open == Some(Open::Draft(draft)) { self.open = Some(Open::Filed(id)); }
            }
            Err(why) => d.capture = Capture::Failed(why.replace(['\r', '\n'], " ")),
        }
    }

    pub fn take_pin(&mut self) -> Option<(Id, Id)> { self.pending_pin.take() }

    pub fn set_error(&mut self, why: String) { self.error = Some(why); }

    pub fn actions(&mut self, cx: &mut Cx, page: &WidgetRef, actions: &Actions, model: &mut Model) -> bool {
        let Some(project) = self.project else { return false };
        let input = page.text_input(cx, ids!(issue_input));
        let mut changed = false;
        if let Some(note) = input.changed(actions) {
            match self.open {
                Some(Open::Draft(id)) => { if let Some(d) = self.drafts.get_mut(&id) { d.note = note; } }
                Some(Open::Filed(id)) => {
                    self.edits.insert(id, note);
                    cx.stop_timer(std::mem::take(&mut self.timer));
                    self.timer = cx.start_timeout(1.);
                }
                None => {}
            }
        }
        if input.key_focus_lost(actions) { self.flush(cx, model); changed = true; }
        if page.button(cx, ids!(issue_back)).clicked(actions) { self.select(cx, model, None); return true }
        if let Some(open) = self.open.filter(|_| project.town == model.open_id() && self.playing.is_none()
            && model.state.active_member(&model.me())) {
            for (id, tool) in [(ids!(draw_mark), MarkTool::Pen), (ids!(arrow_mark), MarkTool::Arrow)] {
                if let Some(on) = page.check_box(cx, id).changed(actions) {
                    self.tool = if on { tool } else { MarkTool::Off };
                    changed = true;
                }
            }
            let picture = page.widget(cx, ids!(picture));
            for action in actions.filter_widget_actions(picture.widget_uid()) {
                let Some(MarkedPictureAction::Add { width, head, pts }) = action.action.downcast_ref::<MarkedPictureAction>() else { continue };
                let (width, head) = (*width, *head);
                match open {
                    Open::Draft(id) => if let Some(draft) = self.drafts.get_mut(&id) {
                        let chunks = marks::chunks(pts);
                        let n = chunks.len();
                        draft.marks.extend(chunks.into_iter().enumerate().map(|(i, pts)|
                            (width, if i + 1 == n { head } else { Head::None }, pts)));
                        self.marks.target = None;
                    }
                    Open::Filed(target) => self.error = model.act(ModelAction::AddMark { target, width, head, pts: pts.clone() }, Origin::Ui).err(),
                }
                changed = true;
            }
            if page.button(cx, ids!(undo_mark)).clicked(actions) {
                match open {
                    Open::Draft(id) => if let Some(draft) = self.drafts.get_mut(&id) {
                        draft.marks.pop();
                        self.marks.target = None;
                    }
                    Open::Filed(target) => {
                        self.marks.sync(model, Open::Filed(target), None);
                        if let Some(mark) = self.marks.newest_own {
                            self.error = model.act(ModelAction::DeleteMark { mark }, Origin::Ui).err();
                        }
                    }
                }
                changed = true;
            }
        }
        if let Some(Open::Filed(id)) = self.open {
            if page.button(cx, ids!(issue_pin)).clicked(actions) && project.town == model.open_id()
                && model.state.active_member(&model.me()) && model.state.entities.get(&id).is_some_and(|e|
                    e.project == Some(project.project) && !e.flag(field::DELETED) && blob_id(e, field::BLOB).is_some()) {
                self.error = None;
                self.pending_pin = Some((project.project, id));
                changed = true;
            }
            if page.button(cx, ids!(issue_play)).clicked(actions) {
                if self.play_requested.take().is_none() {
                    if let Some(blob) = model.state.entities.get(&id).and_then(|e| blob_id(e, field::VIDEO)) {
                        self.error = model.get_clip(project.town, blob).err();
                        if self.error.is_none() { self.play_requested = Some(id); }
                    }
                }
                changed = true;
            }
            if page.button(cx, ids!(issue_status)).clicked(actions) {
                let resolved = model.state.entities.get(&id).is_some_and(|e| e.text(field::STATUS) == "resolved");
                self.error = model.change_issue(project, id, IssueEdit::Resolved(!resolved)).err();
                changed = true;
            }
            if page.button(cx, ids!(issue_delete)).clicked(actions) {
                self.flush(cx, model);
                self.error = model.change_issue(project, id, IssueEdit::Delete).err();
                if self.error.is_none() { self.select(cx, model, None); }
                changed = true;
            }
        }
        if self.open.is_none() {
            for a in actions.iter().filter_map(|a| a.as_widget_action()) {
                if !matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { continue }
                let Some(&open) = a.data.as_ref().and_then(|v| v.downcast_ref::<Open>()) else { continue };
                if !self.lines.iter().any(|r| r.open == open) { continue }
                self.select(cx, model, Some(open));
                if let Open::Filed(id) = open {
                    if let Some(blob) = model.state.entities.get(&id).and_then(|e| blob_id(e, field::BLOB)) {
                        self.error = model.get_clip(project.town, blob).err();
                    }
                }
                return true
            }
        }
        changed
    }
}

impl ViewData {
    pub fn issue_share(&self) -> Option<SessionId> {
        if self.page == Page::Issues { self.issues.source_share } else { None }
    }

    pub fn select_project(&mut self, model: &mut Model, id: Id) {
        model.select_project(id);
        let current = model.current_project().map(|e| e.id);
        self.workspace.current = current;
        self.workspace.removable = current.filter(|id| model.can_remove_project(*id));
    }

    pub fn issue_destination(&self, model: &Model) -> Option<IssueProject> {
        let tree = self.tree.snapshot();
        if tree.town != Some(model.open_id()) || !model.state.active_member(&model.me()) { return None }
        let project = self.workspace.current.and_then(|id| tree.project(id)).or_else(|| tree.projects.first())?;
        Some(IssueProject { town: model.open_id(), project: project.id })
    }

    fn open_issue_draft(&mut self, model: &mut Model, project: IssueProject, label: String, session: Option<SessionId>,
        source_peer: Option<PublicKey>, capture: Capture, done: Option<Receiver<Option<Recording>>>) {
        self.issues.start_draft(project, label, session, source_peer, capture, done);
        if project.town == model.open_id() {
            self.select_project(model, project.project);
            self.page = Page::Issues;
            self.show_pane(Pane::Page);
        }
    }

    pub fn report_issue(&mut self, cx: &mut Cx, model: &mut Model, report: Report) {
        let Report { project, size, still, label, done } = report;
        self.issues.flush(cx, model);
        let capture = cx.task_pool().submit_named(Class::Background, "Town reported picture", move || IssueStill::from_report(still, size))
            .map(|task| Capture::Still(StillCapture::Converting(task))).unwrap_or_else(|e| Capture::Failed(e.to_string()));
        self.open_issue_draft(model, project, label, None, None, capture, Some(done));
    }

    /// Called on the original input with the share page's actions.
    pub fn share_issue_input(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions, links: &mut AppLinks, model: &mut Model) -> bool {
        let Some(session) = self.share_page() else { return false };
        if self.narrow() && self.pane != Pane::Page || !ui.button(cx, ids!(live_share.share_issue)).clicked(actions) { return false }
        let Some(project) = self.issue_destination(model) else { return false };
        let Some(still) = self.call.share.capture_still(cx, links, session) else { return false };
        self.open_issue_draft(model, project, still.label, Some(session), still.source_peer,
            still.capture.map(Capture::Still).unwrap_or_else(Capture::Failed), None);
        true
    }
}

#[derive(Script, ScriptHook, Widget)]
struct IssueRow {
    #[deref] view: View,
    #[rust] poster: Option<PathBuf>,
}
impl Widget for IssueRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
}

#[derive(Script, ScriptHook, Widget)]
struct IssuePage { #[deref] view: View }
impl Widget for IssuePage {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(data) = scope.data.get::<ViewData>() else { continue };
            if let Some(mut list) = step.borrow_mut::<FlatList>() {
                for row in data.issues.choices.iter().flat_map(|rows| rows.iter()) {
                    let template = if row.selectable { id!(Choice) } else { id!(Empty) };
                    let Some(item) = list.item(cx, row.widget_id, template) else { continue };
                    item.set_text(cx, &row.label);
                    if let ShareChoice::Row(key) = row.choice { item.set_action_data(WindowChoice(key)); }
                    item.draw_all(cx, &mut Scope::empty());
                }
                list.items.retain_visible();
            } else if let Some(mut list) = step.borrow_mut::<PortalList>() {
                let rows = &data.issues.lines;
                list.set_item_range(cx, 0, rows.len());
                while let Some(i) = list.next_visible_item(cx) {
                    let Some(row) = rows.get(i) else { continue };
                    let item = list.item(cx, i, id!(Row));
                    let button = item.button(cx, ids!(open));
                    button.set_text(cx, if row.first.is_empty() { "Visual issue" } else { &row.first });
                    button.set_action_data(row.open);
                    item.label(cx, ids!(meta)).set_text(cx, &row.meta);
                    if let Some(mut r) = item.borrow_mut::<IssueRow>() {
                        let image = r.view.image(cx, ids!(poster));
                        image_path(cx, image, &mut r.poster, row.poster.as_deref());
                    }
                    item.draw_all(cx, &mut Scope::empty());
                }
            }
        }
        if let Some(data) = scope.data.get_mut::<ViewData>() {
            if std::mem::take(&mut data.issues.focus) { self.view.text_input(cx, ids!(issue_input)).take_key_focus(cx); }
        }
        DrawStep::done()
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
}
