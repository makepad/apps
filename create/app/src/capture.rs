//! The window list is shared by Share, Clip and Snap. Only its location and the selection's intent differ.
use crate::{app_link::{AppKey, AppLinkEvent, AppLinks, IssueStill, LinkId, LinkPhase, ThumbnailLinks, ThumbnailReply, WindowId},
    call::{Press, ShareChoice, ShareRole, ShareRow, ShareShown}, model::Model};
use makepad_widgets::*;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use makepad_call::driver::codec::Picture;
use makepad_town_core::{event::{Id, SessionId, TownId}, key::PublicKey};
use std::{cell::OnceCell, rc::{Rc, Weak}, sync::{Arc, mpsc::{Receiver, TryRecvError}}, time::Instant};

pub(crate) enum StillCapture {
    Grabbing(Receiver<Result<IssueStill, String>>),
    Converting(TaskHandle<Result<IssueStill, String>>),
}

impl StillCapture {
    pub(crate) fn grab(links: &mut AppLinks, link: LinkId, window: WindowId, label: &mut String) -> Result<Self, String> {
        let app = links.state(link).ok_or("The app has closed.")?;
        let w = app.windows.iter().find(|w| w.id == window && w.visible).ok_or("The window has closed.")?;
        *label = format!("{} · {} · {} × {} pt · {} dpi", app.app.name, w.title, w.size.0, w.size.1, w.dpi)
            .replace(['\n', '\r'], " ");
        Ok(Self::Grabbing(links.issue_still(link, window)?))
    }

    pub(crate) fn poll(&mut self) -> Option<Result<IssueStill, String>> {
        match self {
            Self::Grabbing(receipt) => match receipt.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("The app closed before the picture arrived.".into())),
            },
            Self::Converting(task) => Some(task.try_take()?.unwrap_or_else(|_| Err("The picture conversion stopped.".into()))),
        }
    }
}

pub(crate) struct SharedStill {
    pub label: String,
    pub source_peer: Option<PublicKey>,
    pub capture: Result<StillCapture, String>,
}

impl ShareShown {
    pub(crate) fn capture_still(&self, cx: &Cx, links: &mut AppLinks, session: SessionId) -> Option<SharedStill> {
        let offer = self.offer(session)?;
        let mut label = offer.title.replace(['\r', '\n'], " ");
        let (source_peer, capture) = match offer.role {
            ShareRole::Sharer { link, window } => (None, StillCapture::grab(links, link, window, &mut label)),
            ShareRole::Viewer { sharer } => {
                let picture = self.picture_for(session)?.clone();
                let (w, h) = offer.window_size;
                label = format!("{label} · {w} × {h} pt · as received");
                let capture = cx.task_pool().submit_named(Class::Background, "Town shared picture", move || IssueStill::from_picture(&picture))
                    .map(StillCapture::Converting).map_err(|e| e.to_string());
                (Some(sharer), capture)
            }
        };
        Some(SharedStill { label, source_peer, capture })
    }
}

struct BoardSnap {
    town: TownId,
    board: Id,
    session: SessionId,
    source_peer: Option<PublicKey>,
    title: String,
    capture: StillCapture,
}

#[derive(Default)]
pub struct BoardSnaps {
    pending: Vec<BoardSnap>,
}

impl BoardSnaps {
    pub fn start(&mut self, cx: &Cx, links: &mut AppLinks, share: &ShareShown, session: SessionId, town: TownId, board: Id) -> Result<(), String> {
        let still = share.capture_still(cx, links, session).ok_or("The shared picture is no longer available.")?;
        self.pending.push(BoardSnap { town, board, session, source_peer: still.source_peer, title: still.label, capture: still.capture? });
        Ok(())
    }

    pub fn busy(&self) -> bool { !self.pending.is_empty() }

    pub fn poll(&mut self, model: &mut Model) {
        self.pending.retain_mut(|snap| {
            let Some(result) = snap.capture.poll() else { return true };
            if let Err(why) = result.and_then(|still| model.store_board_snap(snap.town, snap.board,
                std::mem::take(&mut snap.title), snap.session, snap.source_peer, still)) {
                model.record_clip_problem(format!("Snap to board: {why}"));
            }
            false
        });
    }
}

pub(crate) enum ThumbnailPicture { Still(Arc<ImageBuffer>), Shared(Arc<Picture>) }

#[derive(Default)]
pub struct WindowThumbnail {
    shared: bool,
    pub(crate) picture: OnceCell<Result<ThumbnailPicture, ()>>,
    pub(crate) texture: OnceCell<Texture>,
}

enum ThumbnailJob { Still(Receiver<ThumbnailReply>), Shared(LinkId, WindowId) }

// A shared window can be waiting for its first local preview.
type SharedPreview<'a> = dyn Fn(LinkId, WindowId) -> Option<Option<Arc<Picture>>> + 'a;

impl std::fmt::Debug for WindowThumbnail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowThumbnail").field("shared", &self.shared)
            .field("ready", &self.picture.get().is_some_and(Result::is_ok)).finish()
    }
}

#[derive(Default)]
struct Thumbnails {
    pending: Vec<(Weak<WindowThumbnail>, ThumbnailJob)>,
    links: ThumbnailLinks,
}


impl Thumbnails {
    fn for_window(&mut self, apps: &mut AppLinks, key: &AppKey, window: WindowId,
        shared: &SharedPreview<'_>, previous: Option<&Rc<WindowThumbnail>>) -> Rc<WindowThumbnail> {
        let link = apps.linked().find(|l| &l.app.key == key).map(|l| l.id);
        let live = link.and_then(|id| shared(id, window).map(|picture| (id, picture)));
        if let Some(old) = previous.filter(|p| p.shared == live.is_some()) { return old.clone() }
        let row = Rc::new(WindowThumbnail { shared: live.is_some(), ..Default::default() });
        match live {
            Some((_, Some(picture))) => { let _ = row.picture.set(Ok(ThumbnailPicture::Shared(picture))); }
            Some((id, None)) => self.pending.push((Rc::downgrade(&row), ThumbnailJob::Shared(id, window))),
            None => match apps.thumbnail(&mut self.links, link, key, window) {
                Some(rx) => self.pending.push((Rc::downgrade(&row), ThumbnailJob::Still(rx))),
                None => { let _ = row.picture.set(Err(())); }
            },
        }
        row
    }

    fn poll(&mut self, shared: Option<&SharedPreview<'_>>) -> bool {
        if self.pending.is_empty() { return false }
        self.links.flush();
        let mut arrived = false;
        self.pending.retain(|(row, job)| {
            let Some(row) = row.upgrade() else { return false };
            let picture = match job {
                ThumbnailJob::Still(rx) => match rx.try_recv() {
                    Ok(result) => result.map(ThumbnailPicture::Still).map_err(|_| ()),
                    Err(TryRecvError::Empty) => return true,
                    Err(TryRecvError::Disconnected) => Err(()),
                },
                ThumbnailJob::Shared(link, window) => match shared.and_then(|f| f(*link, *window)) {
                    Some(Some(p)) => Ok(ThumbnailPicture::Shared(p)),
                    Some(None) => return true,
                    None => Err(()),
                },
            };
            let _ = row.picture.set(picture);
            arrived = true;
            false
        });
        if !self.pending.iter().any(|(_, job)| matches!(job, ThumbnailJob::Still(_))) {
            self.links = ThumbnailLinks::default();
        }
        arrived
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowTarget { Town, Linked(LinkId, WindowId) }

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose { Share, Clip, Snap }

#[derive(Clone, PartialEq, Eq)]
enum Target { Town, Window(AppKey, usize, WindowId), Empty }

impl Target {
    // One registry entry per local process. Keep the widget path independent of the
    // title and list position; the press still checks this particular app instance.
    fn widget_id(&self) -> LiveId {
        match self {
            Self::Window(app, _, window) => LiveId::from_str_with_intern(
                &format!("window_{}_{}", app.pid(), window.0), InternLiveId::Yes),
            Self::Town => LiveId::from_str_with_intern("town_window", InternLiveId::Yes),
            Self::Empty => id!(no_windows),
        }
    }
}

/// Why the Share list holds no window, so it never stands empty without a word: still looking,
/// no app link, apps Town cannot reach (with the reason), or apps without a visible window.
pub(super) fn empty_reason(configured: bool, scanning: bool, unreachable: &[(String, String)], windowless: Option<&str>) -> String {
    if !configured { return "Town's app link is not running, so it cannot list apps.".into() }
    if scanning { return "Looking for running Makepad apps…".into() }
    if let Some((name, why)) = unreachable.first() {
        let more = match unreachable.len() - 1 { 0 => String::new(), n => format!(" (and {n} more)") };
        return format!("No apps to share: Town can't reach {name}{more}: {why}.");
    }
    if let Some(name) = windowless { return format!("No apps to share: {name} has no visible window."); }
    "No apps to share: no other Makepad app is running on this computer.".into()
}

fn window_name(app: &str, title: &str) -> String {
    let name = app.strip_prefix("makepad-app-").or_else(|| app.strip_prefix("makepad-")).unwrap_or(app);
    let mut label = name.replace(['-', '_'], " ");
    if let Some(first) = label.chars().next() {
        label.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    }
    let title = title.trim();
    if !title.is_empty() && !title.eq_ignore_ascii_case("Makepad") && title.to_lowercase() != label.to_lowercase() {
        label.push_str(" · ");
        label.push_str(title);
    }
    label
}

#[test]
fn picker_window_name_keeps_only_a_distinct_title() {
    assert_eq!(window_name("makepad-example-counter", "  mAkEpAd  "), "Example counter");
    assert_eq!(window_name("makepad-app-browser", ""), "Browser");
    assert_eq!(window_name("makepad-app-note_editor", "  Meeting notes  "), "Note editor · Meeting notes");
    assert_eq!(window_name("note_editor", " NOTE EDITOR "), "Note editor");
}

/// The picker and the resulting recording or share use the same labels. Assign duplicate
/// suffixes in process/window order, before moving the last choice to the front.
fn window_rows(apps: &AppLinks) -> Vec<(Target, String)> {
    let mut rows = Vec::new();
    let mut used = std::collections::HashSet::new();
    let mut counts = std::collections::HashMap::new();
    for app in apps.apps() {
        let mut windows: Vec<_> = match apps.linked().find(|l| l.app.key == app.key && l.phase == LinkPhase::Live) {
            Some(link) => link.windows.iter().filter(|w| w.visible).map(|w| (w.id, w.title.as_str())).collect(),
            None => app.visible_windows.iter().map(|(id, t)| (*id, t.as_str())).collect(),
        };
        windows.sort_by_key(|(id, _)| id.0);
        for (pos, (id, title)) in windows.into_iter().enumerate() {
            let base = window_name(&app.name, title);
            let count = counts.entry(base.clone()).or_insert(1);
            let label = loop {
                let label = if *count == 1 { base.clone() } else { format!("{base} · {count}") };
                *count += 1;
                if used.insert(label.clone()) { break label; }
            };
            rows.push((Target::Window(app.key.clone(), pos, id), label));
        }
    }
    rows
}

pub(crate) fn window_label(apps: &AppLinks, link: LinkId, window: WindowId) -> Option<String> {
    let key = &apps.state(link)?.app.key;
    window_rows(apps).into_iter().find_map(|(target, name)|
        matches!(target, Target::Window(ref app, _, id) if app == key && id == window).then_some(name))
}

#[derive(Clone, Default)]
pub struct CaptureShown {
    pub picking: bool,
    pub line: Option<Arc<str>>,
    /// The recording `line` shows, as fields for readers that are not the screen.
    pub recording: Option<RecordingShown>,
    pub problem: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordingShown {
    pub title: Arc<str>,
    pub started: Instant,
}

impl CaptureShown {
    pub fn show_recording(&self, cx: &mut Cx, ui: &WidgetRef) {
        let row = ui.widget(cx, ids!(title_content.recording));
        row.set_visible(cx, self.recording.is_some());
        if let Some(line) = &self.line { row.label(cx, ids!(line)).set_text(cx, line); }
    }
}

pub fn recording_press(cx: &Cx, ui: &WidgetRef, actions: &Actions) -> Option<Press> {
    ui.button(cx, ids!(title_content.recording.stop_recording)).clicked(actions).then_some(Press::StopRecording)
}

#[derive(Default)]
pub(crate) struct WindowPicker {
    pub purpose: Option<Purpose>,
    rows: Vec<(u64, Target, Option<Rc<WindowThumbnail>>)>,
    thumbnails: Thumbnails,
    serial: u64,
    selection: Option<Target>,
    linking: Option<(LinkId, usize, WindowId)>,
    scan: bool,
    // None is the local window. Linked windows are remembered by app identity and position.
    last: Option<Option<(AppKey, usize)>>,
}

impl WindowPicker {
    pub fn close(&mut self) {
        self.rows.clear();
        self.thumbnails = Thumbnails::default();
        self.selection = None;
        self.linking = None;
        self.purpose = None;
        self.scan = false;
    }

    pub fn toggle(&mut self, purpose: Purpose) {
        let closing = self.purpose == Some(purpose);
        self.close();
        if !closing { self.purpose = Some(purpose); self.scan = true; }
    }

    pub fn choose(&mut self, key: u64) -> bool {
        let Some((_, target, _)) = self.rows.iter().find(|(id, ..)| *id == key) else { return false };
        if matches!(target, Target::Empty) { return false; }
        self.selection = Some(target.clone());
        true
    }

    pub fn choose_first(&mut self) -> bool {
        let Some((_, target, _)) = self.rows.iter().find(|(_, target, _)| !matches!(target, Target::Empty)) else { return false };
        self.selection = Some(target.clone());
        true
    }

    pub fn poll(&mut self, apps: &mut AppLinks, events: &[AppLinkEvent], rows: &mut Option<Arc<[ShareRow]>>)
        -> (bool, Option<(Purpose, Result<WindowTarget, String>)>) {
        self.poll_rows(apps, events, rows, false, None)
    }

    pub(crate) fn poll_rows(&mut self, apps: &mut AppLinks, events: &[AppLinkEvent], rows: &mut Option<Arc<[ShareRow]>>,
        shares_changed: bool, shared: Option<&SharedPreview<'_>>)
        -> (bool, Option<(Purpose, Result<WindowTarget, String>)>) {
        let Some(purpose) = self.purpose else { return (false, None) };
        let scan = std::mem::take(&mut self.scan);
        if scan { apps.scan(); }
        let mut picked = None;
        match self.selection.take() {
            Some(Target::Window(key, pos, window)) => {
                let link = apps.linked().find(|l| l.app.key == key).map(|l| l.id);
                self.linking = Some((link.unwrap_or_else(|| apps.link(&key)), pos, window));
            }
            Some(Target::Town) => { self.last = Some(None); picked = Some(Ok(WindowTarget::Town)); }
            _ => {}
        }
        if let Some((id, pos, window)) = self.linking {
            if let Some(reason) = events.iter().find_map(|e| match e {
                AppLinkEvent::Ended { id: ended, why } if *ended == id => Some(why.as_str()),
                AppLinkEvent::Restarted { ended, .. } if *ended == id => Some("The selected app restarted."),
                _ => None,
            }) {
                picked = Some(Err(reason.to_owned()));
            } else if let Some(link) = apps.state(id) {
                match &link.phase {
                    LinkPhase::Live if link.windows.iter().any(|w| w.visible && w.id == window) => {
                        self.last = Some(Some((link.app.key.clone(), pos)));
                        picked = Some(Ok(WindowTarget::Linked(id, window)));
                    }
                    LinkPhase::Live => picked = Some(Err("The selected window is no longer visible.".into())),
                    LinkPhase::Ended(why) => picked = Some(Err(why.clone())),
                    LinkPhase::Connecting => {}
                }
            }
        }
        if let Some(result) = picked {
            self.close();
            *rows = None;
            return (true, Some((purpose, result)));
        }
        let arrived = self.thumbnails.poll(shared);
        if !scan && !shares_changed && !events.iter().any(|e| matches!(e, AppLinkEvent::Apps | AppLinkEvent::Linked(_) |
            AppLinkEvent::Windows(_) | AppLinkEvent::Ended { .. } | AppLinkEvent::Restarted { .. })) { return (arrived, None); }
        let mut choices = window_rows(apps);
        if purpose != Purpose::Share { choices.push((Target::Town, "Town".into())); }
        if let Some(last) = &self.last {
            if let Some(i) = choices.iter().position(|(target, _)| match (last, target) {
                (None, Target::Town) => true,
                (Some((k, pos)), Target::Window(key, index, ..)) => k == key && pos == index,
                _ => false,
            }) { choices[..=i].rotate_right(1); }
        }
        if choices.is_empty() {
            let windowless = apps.apps().first().map(|a| a.title.as_deref().unwrap_or(&a.name));
            let reason = empty_reason(apps.configured(), apps.scanning(), apps.unreachable(), windowless);
            choices.push((Target::Empty, reason));
        }
        let old = std::mem::take(&mut self.rows);
        let mut shown: Vec<_> = choices.into_iter().map(|(target, label)| {
            let previous = old.iter().find(|(_, previous, _)| match (previous, &target) {
                (Target::Window(a, _, w), Target::Window(b, _, v)) => a == b && w == v,
                _ => *previous == target,
            });
            let id = previous.map(|(id, ..)| *id).unwrap_or_else(|| { self.serial += 1; self.serial });
            let mut selectable = !matches!(target, Target::Empty);
            let mut thumbnail = None;
            if let (Target::Window(key, _, window), Some(shared)) = (&target, shared) {
                let preview = self.thumbnails.for_window(apps, key, *window, shared, previous.and_then(|(_, _, p)| p.as_ref()));
                selectable = purpose != Purpose::Share || !preview.shared;
                thumbnail = Some(preview);
            }
            let widget_id = target.widget_id();
            self.rows.push((id, target, thumbnail.clone()));
            ShareRow { widget_id, choice: ShareChoice::Row(id), label, selectable, thumbnail }
        }).collect();
        if shown.iter().all(|r| !r.selectable && r.thumbnail.is_some()) {
            shown.push(ShareRow { widget_id: id!(no_windows), choice: ShareChoice::Menu,
                label: "All available windows are already shared.".into(), selectable: false, thumbnail: None });
        }
        *rows = Some(shown.into());
        (true, None)
    }
}

#[derive(Default)]
pub(crate) struct RecordingClock {
    timer: Timer,
    second: Option<(Instant, u64)>,
}

pub(super) fn recording_line(title: &str, seconds: u64) -> String {
    format!("Recording {title} · {}:{:02}", seconds / 60, seconds % 60)
}

impl RecordingClock {
    pub fn update(&mut self, cx: &mut Cx, recording: Option<(Instant, &str)>, now: Instant, shown: &mut CaptureShown) -> bool {
        let stamp = recording.map(|(start, _)| (start, now.saturating_duration_since(start).as_secs()));
        if stamp == self.second { return false; }
        self.second = stamp;
        shown.line = recording.zip(stamp).map(|((_, title), (_, s))| recording_line(title, s).into());
        if shown.recording.as_ref().map(|r| (r.started, &*r.title)) != recording {
            shown.recording = recording.map(|(started, title)| RecordingShown { title: title.into(), started });
        }
        if stamp.is_some() && self.timer.is_empty() { self.timer = cx.start_interval(1.); }
        if stamp.is_none() { cx.stop_timer(std::mem::take(&mut self.timer)); }
        true
    }
}

pub fn shortcut(event: &Event) -> Option<Press> {
    let Event::KeyDown(k) = event else { return None };
    if k.is_repeat || !k.modifiers.is_primary() || !k.modifiers.shift || k.modifiers.alt { return None; }
    match k.key_code { KeyCode::Key1 => Some(Press::Snap), KeyCode::Key2 => Some(Press::Clip), _ => None }
}

use super::{Destination, Recorder, RecordedClip};
use makepad_widgets::makepad_platform::screen_capture::FrameCapture;
use std::path::{Path, PathBuf};

fn new_file(dir: &Path, extension: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    dir.join(format!("capture-{}-{time}-{}.{extension}", std::process::id(), SEQUENCE.fetch_add(1, Ordering::Relaxed)))
}

enum StillJob { Frame(FrameCapture), Writing(TaskHandle<Result<(u32, u32), String>>) }
enum StillFor { Snap(Destination), Poster(Instant) }

pub(super) struct LocalStill {
    purpose: StillFor,
    path: PathBuf,
    job: StillJob,
}

impl LocalStill {
    pub(super) fn is_snap(&self) -> bool { matches!(self.purpose, StillFor::Snap(_)) }

    fn poll(&mut self, cx: &mut Cx) -> Option<Result<(u32, u32), String>> {
        match &mut self.job {
            StillJob::Frame(frame) => {
                let png = match frame.take()? { Ok(bytes) => bytes, Err(why) => return Some(Err(why)) };
                let path = self.path.clone();
                let poster = !self.is_snap();
                let submit = cx.task_pool().submit_named(Class::Background, "save window picture", move || {
                    let result = write_still(&path, &png, poster);
                    SignalToUI::set_ui_signal();
                    result
                });
                match submit {
                    Ok(job) => { self.job = StillJob::Writing(job); None }
                    Err(e) => Some(Err(e.to_string())),
                }
            }
            StillJob::Writing(task) => task.try_take().map(|r| r.map_err(|e| e.to_string()).and_then(|r| r)),
        }
    }
}

fn write_still(path: &Path, png: &[u8], poster: bool) -> Result<(u32, u32), String> {
    use std::io::Write;
    let ((w, h), bytes);
    let jpeg;
    if poster {
        let image = ImageBuffer::from_png(png).map_err(|e| format!("Could not decode the poster: {e:?}"))?;
        let scale = (640. / image.width.max(image.height) as f64).min(1.);
        let preview = image.area_downscaled((image.width as f64 * scale).round() as usize, (image.height as f64 * scale).round() as usize);
        (w, h) = (preview.width as u32, preview.height as u32);
        let mut rgb = Vec::with_capacity(preview.data.len() * 3);
        for pixel in preview.data { rgb.extend_from_slice(&pixel.to_be_bytes()[1..]); }
        let mut encoded = Vec::new();
        jpeg_encoder::Encoder::new(&mut encoded, 80).encode(&rgb, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| e.to_string())?;
        jpeg = encoded;
        bytes = jpeg.as_slice();
    } else {
        let size = makepad_widgets::makepad_draw::image_cache::image_size_by_data(png, path).map_err(|e| format!("Invalid captured PNG: {e:?}"))?;
        (w, h) = (size.0 as u32, size.1 as u32);
        bytes = png;
    }
    let mut f = std::fs::File::create_new(path).map_err(|e| e.to_string())?;
    if let Err(e) = f.write_all(bytes) {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(e.to_string());
    }
    Ok((w, h))
}

impl Recorder {
    /// Poll the current window and worker receipts. All encoding and file writes run on workers.
    pub fn capture_event(&mut self, cx: &mut Cx, window: &WindowRef, links: &mut AppLinks, model: &mut Model) {
        let folder = model.recordings_dir();
        if !self.snaps.is_empty() || self.local.as_ref().is_some_and(|m| m.file.is_none()) {
            match folder {
                Some(Ok(dir)) => {
                    for (target, dest) in self.snaps.drain(..) {
                        match target {
                            WindowTarget::Town => self.own_stills.push(LocalStill { purpose: StillFor::Snap(dest),
                                path: new_file(dir, "png"), job: StillJob::Frame(cx.capture_next_frame()) }),
                            WindowTarget::Linked(link, win) => {
                                let Some(title) = window_label(links, link, win) else {
                                    self.problem = Some("The selected window is no longer visible.".into());
                                    continue;
                                };
                                let ticket = links.still(link, win, &new_file(dir, "jpg"), crate::app_link::StillSize::Full);
                                self.grabbing.insert((link, ticket), (dest, title));
                            }
                        }
                    }
                    if let Some(local) = self.local.as_mut().filter(|m| m.file.is_none()) {
                        let result = window.borrow_mut().ok_or_else(|| "The window is closed.".to_string())
                            .and_then(|mut w| w.record_to(cx, new_file(dir, "mp4")));
                        match result {
                            Ok(file) => {
                                local.file = Some(file);
                                self.own_stills.push(LocalStill { purpose: StillFor::Poster(local.started), path: new_file(dir, "jpg"),
                                    job: StillJob::Frame(cx.capture_next_frame()) });
                            }
                            Err(why) => { self.problem = Some(why); self.local = None; }
                        }
                    }
                }
                Some(Err(why)) => {
                    self.snaps.clear();
                    if self.local.as_ref().is_some_and(|m| m.file.is_none()) { self.local = None; }
                    self.problem = Some(why.into());
                }
                None => {}
            }
        }

        self.own_stills.retain_mut(|still| {
            if matches!(&still.purpose, StillFor::Poster(start) if self.local.as_ref().is_none_or(|l| l.started != *start)) {
                if let StillJob::Frame(frame) = &still.job { cx.cancel_frame_capture(frame.request_id()); return false; }
            }
            let Some(result) = still.poll(cx) else { return true; };
            match (&still.purpose, result) {
                (StillFor::Snap(d), Ok((w,h))) => {
                    if let Err(why) = model.add_picture(d.town, d.project, d.session, still.path.clone(), "Town".into(), w, h) { self.problem = Some(why); }
                }
                (StillFor::Snap(_), Err(why)) => self.problem = Some(why),
                (StillFor::Poster(start), Ok(_)) => match self.local.as_mut().filter(|l| l.started == *start) {
                    Some(l) => l.poster = Some(still.path.clone()), None => self.unused_posters.push(still.path.clone()),
                },
                (StillFor::Poster(_), Err(why)) => log!("Recording poster unavailable: {why}"),
            }
            false
        });
        // A still waits for a presented frame: ask for one only when none is pending, since this
        // runs on every event and a Cx that draws no frame (a test) would otherwise grow its set.
        if !self.own_stills.is_empty() && !self.still_frame.is_some_and(|f| cx.next_frame_is_pending(f)) {
            self.still_frame = Some(cx.new_next_frame());
        }
        let Some(local) = self.local.as_mut() else { return; };
        if local.stopping && local.file.is_some() && !local.stop_sent {
            if let Some(mut w) = window.borrow_mut() { w.stop_file_recording(cx); }
            local.stop_sent = true;
        }
        let Some(result) = local.file.as_ref().and_then(WindowRecording::try_finish) else { return; };
        let local = self.local.take().unwrap();
        match result {
            Ok(file) => self.finished.push(RecordedClip { destination: local.destination, path: file.path,
                title: "Town".into(), duration_ms: file.duration_ms, poster: local.poster, width: file.width, height: file.height }),
            Err(why) => {
                self.problem = Some(why);
                if let Some(poster) = local.poster { self.unused_posters.push(poster); }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CaptureBar {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] choices: Option<WidgetRef>,
}

pub(crate) fn bar_template(vm: &mut ScriptVm) -> ScriptValue {
    let base = CaptureBar::register_widget(vm);
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        #(base){
            width: Fill height: Fill flow: Down
            body := View{width: Fill height: Fill flow: Down}
            problem := RailText{visible: false width: Fill a11y: {live: @Polite}}
        }
    })
}

impl CaptureBar {
    pub fn show(&mut self, cx: &mut Cx, shown: &CaptureShown) {
        // The capture bar precedes the call widgets in registration order. Instantiate
        // the shared window list on its first opening and keep it for subsequent presses.
        if shown.picking && self.choices.is_none() {
            let list = cx.with_vm(|vm| {
                let v = script_eval!(vm, {
                    use mod.prelude.workspace.*
                    mod.widgets.ShareChoices{
                        list +: {height: Fit{max: FitBound.Rel{base: Base.Unused factor: 1.0}}}
                    }
                });
                WidgetRef::script_from_value(vm, v)
            });
            if let Some(mut body) = self.view.view(cx, ids!(body)).borrow_mut() {
                body.children.push((id!(capture_choices), list.clone()));
                cx.widget_tree_mark_dirty(body.widget_uid());
                self.choices = Some(list);
            }
        }
        if let Some(list) = &self.choices { list.set_visible(cx, shown.picking); }
        let problem = self.view.label(cx, ids!(problem));
        problem.set_visible(cx, shown.problem.is_some());
        if let Some(why) = &shown.problem { problem.set_text(cx, why); }
        self.view.redraw(cx);
    }
}

impl Widget for CaptureBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) { self.view.handle_event(cx, event, scope); }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
}
