//! The graphic desk: an opened Motion document on the CUT page, in place
//! of the edit's own panes (like a compound clip opened): its viewer, its
//! inspector, and below them its layer timeline, graph or source (the
//! project bar's view toggle chooses, as for the edit).
//!
//! The session (`session.rs`) holds the document; every gesture here is a
//! `MotionOp` on it, one undo step each. The document is autosaved two
//! seconds after the last change. A project plate writes its file and
//! changed modules into the CUT tree draft, then uses the shared tree save.
//! A standalone library graphic publishes a Motion revision.

use makepad_motion::plain::PrimitiveFrame;
use super::super::ops::CutOp;
use super::super::patch::Value;
use super::super::VideoCut;
use super::checker::{Check, DocKind};
use super::inspector::{Field, FieldKind, InspectorAction, MotionInspector, Section};
use super::session::{is_color, layer_hue, parent_path, LayerSelection, MotionOp, MotionSession};
use super::store::{MotionJob, MotionReply, MotionStore};
use super::viewer::{MotionViewer, ViewItem, ViewerAction};
use super::ViewMode;
use makepad_asset_data::AssetRevisionRef;
use makepad_flow::Literal;
use makepad_motion::patch::{self, PropForm};
use makepad_motion::schema::{KeyPart, Owner, Site, ValueKind};
use makepad_motion::{ItemKind, MotionFrame};
use makepad_story_edit_render::sound::CutSound;
use makepad_edit_values::Edit;
use makepad_widgets::*;

/// Idle time before a changed graphic is autosaved.
const AUTOSAVE: f64 = 2.0;
/// The published thumbnail's size.
const THUMB: (usize, usize) = (512, 288);

/// A graphic being opened, saved or shown.
#[derive(Default)]
pub struct Desk {
    pub session: Option<MotionSession>,
    pub store: Option<MotionStore>,
    held: Vec<MotionJob>,
    /// The Graphic node and revision being fetched.
    opening: Option<(Option<String>, AssetRevisionRef)>,
    tree_opening: Option<(String, Option<String>)>,
    tree_write: Option<Vec<makepad_app_shell::tool_host::Change>>,
    tree_source: Option<String>,
    tree_write_revision: Option<u64>,
    tree_seen: Option<u64>,
    closing: bool,
    /// A model load in flight (checker serial) and whether another revision
    /// waits for one.
    doc_sent: Option<u64>,
    /// When the pending load was asked for (`MAKEPAD_TRACE=motion.reload`
    /// logs how long it took to come back).
    doc_sent_at: Option<std::time::Instant>,
    /// The session revision that load is of.
    doc_rev: u64,
    /// The check whose film samples (its second answer) are awaited.
    activity_sent: Option<u64>,
    /// Where call sites are in the code (the picture's way back to it).
    sites: super::picking::Sites,
    /// When each code block draws over the film, sampled a few frames at a
    /// time after each load: (model, next sample, per file per region).
    activity: (u64, usize, std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<Vec<f32>>>),
    /// What the code's `let`s held over the film (the same samples), per
    /// file: (byte, value per sample, NaN where it was not bound).
    series: std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, Vec<f32>)>>,
    /// The same for vectors and colours: (byte, components per sample
    /// (NaN unbound), 2..4 components or 5 a colour).
    paths: std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, Vec<[f32; 4]>, u8)>>,
    /// How much each block draws at the playhead.
    activity_now: std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<f32>>,
    /// What the code's `let`s hold at the paused playhead, per file:
    /// (byte, value shown short).
    lets_now: std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, String)>>,
    /// The frame at the playhead and the layers' boxes (what a click picks
    /// from), and what it picks, made when first asked: (model, time).
    picture: Option<(u64, f64, makepad_motion::MotionFrame, Vec<(String, [[f64; 2]; 4])>)>,
    picks: Option<(u64, f64, Vec<super::picking::Pickable>)>,
    /// What the renderer says drew each pixel of that picture (model,
    /// time; `None` when it could not say: picks go by shape), and a click
    /// waiting for it (where, alt, shift).
    pick_image: Option<(u64, f64, Option<PickImage>)>,
    pick_wait: Option<(PickTicket, [f64; 2], bool, bool)>,
    /// The last click: where, and how deep down the pile it went.
    pick_cycle: Option<([f64; 2], usize)>,
    /// Where the last click on the picture was (document units).
    pick_point: [f64; 2],
    /// What was picked, by what it is (the code that drew it and its
    /// label): found again in a new frame to follow it, and whether its
    /// outline is on the picture now.
    /// The selection: what was picked on the picture (a click; shift-click
    /// adds or takes away), and its revision (bumped on every change).
    pub(super) selection: Vec<Selected>,
    pub(super) selection_rev: u64,
    /// The curve lanes open, per scene (its title): paths of keyed values;
    /// and what the lanes were last built for (revision, lanes).
    pub(super) lanes: std::collections::HashMap<String, Vec<String>>,
    lanes_of: Option<(u64, Vec<String>)>,
    pick_outlined: bool,
    /// The code block the inspector shows (document, region), when code
    /// was picked or selected.
    pub(super) code_selection: Option<(usize, usize)>,
    /// The value picked in it (its byte): first in the inspector.
    pub(super) code_focus: Option<usize>,
    /// The code view's documents as last given (by revision and activity).
    code_docs_of: Option<(u64, u64, u64)>,
    /// Bumped each time the checker's samples arrive (ribbons, curves).
    activity_rev: u64,
    /// Numbers being dragged in one gesture, set in the running document
    /// as they move (see [`LiveDrag`]), and whether the running document
    /// took them (then it is not loaded again until the drag ends).
    live_drag: Option<LiveDrag>,
    /// An inspector scrub of a code value: (doc, literal, gesture, its
    /// bytes now), and the gestures' counter.
    insp_drag: Option<(usize, usize, u64, std::ops::Range<usize>)>,
    insp_gesture: u64,
    /// Bumped by each number set in the running document (the frame
    /// changes without a new revision).
    live_rev: u64,
    live_patched: bool,
    /// The open graphic's place in the open edit (how the edit loads it),
    /// and for which edit revision and node: the desk loads its document
    /// the same way, so it is the film's.
    film: Option<((u64, String), makepad_story_edit_render::graphic::FilmLoad)>,
    /// Whether the check in flight, and the document the session holds,
    /// were loaded in the film.
    doc_sent_film: bool,
    doc_in_film: bool,
    /// The document text whose literal-only change from the running
    /// document's was last patched in (see [`patch_literal_edits`]).
    literal_patched: Option<std::sync::Arc<str>>,
    /// A paused moment to read more of once the playhead rests there:
    /// (model, playhead, since).
    settle: Option<(u64, u64, f64)>,
    /// The revision the scrubber's filmstrip was asked for.
    film_of: Option<u64>,
    /// The revision and panel paths the code view's bindings were resolved for.
    bindings_of: Option<(u64, usize)>,
    /// The film's sound played with the graphic: the edit's mix (decoded
    /// once per audio plan, not per revision: a graphic's save changes
    /// none of it), and the edit revision it was checked against.
    sound: Option<(makepad_story_cut::AudioPlan, CutSound)>,
    sound_rev: Option<u64>,
    /// A scrub's touch of sound: the graphic time asked for, and until when
    /// (real time) the touch sounds once started.
    scrub: Option<(f64, Option<f64>)>,
    /// When the document was last changed (for the autosave).
    changed_at: Option<f64>,
    publishing: Option<u64>,
    /// The revision the desk's own save published, while the edit op that
    /// points the clip at it is on its way: the clip still naming the
    /// revision before it is not someone else's (nothing reopens).
    repointing: Option<AssetRevisionRef>,
    /// A module being published (its key), under `publishing`'s serial.
    module_publishing: Option<String>,
    publish_serial: u64,
    save_error: Option<String>,
    saved_at: Option<String>,
    /// Who makes the next change (the Source view's history).
    pub author: Option<String>,
    /// The Source view of the graphic.
    pub source: Option<super::source_pane::SourcePane>,
    seen: Option<u64>,
    frame_next: NextFrame,
    /// Library media asked for the viewer, each once.
    media_asked: std::collections::HashSet<makepad_motion::Media>,
    /// Markup of the library SVGs `Character{src: asset(…)}` layers are
    /// built from (lane CH1).
    character_svgs: std::collections::HashMap<makepad_motion::Media, String>,
    pub(super) status: String,
    pub repaint: bool,
    /// A live gesture's latest step, applied on the next frame, and whether
    /// it ends the gesture.
    live: Option<(String, MotionOp, bool)>,
    live_wake: bool,
    /// A drag's layer, where it began, and how its position is written.
    drag_from: Option<(String, Vec<f64>, Option<String>)>,
}

impl Desk {
    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    fn send(&mut self, job: MotionJob) {
        self.held.push(job);
        self.flush();
    }

    fn flush(&mut self) {
        let Some(store) = &self.store else { return };
        while !self.held.is_empty() {
            let job = self.held.remove(0);
            if let Err(job) = store.try_send(job) {
                self.held.insert(0, job);
                break;
            }
        }
    }
}

/// The Motion revision a Graphic node of the open edit shows.
fn graphic_src(page: &VideoCut, node: &str) -> Option<AssetRevisionRef> {
    let session = page.session.as_ref()?;
    let graphic = session.doc.graphics.iter().find(|graphic| graphic.id == node)?;
    let makepad_story_cut::GraphicSrc::Asset(asset) = &graphic.src else { return None };
    Some(AssetRevisionRef { asset_id: asset.id.parse().ok()?, revision: asset.rev.parse().ok()? })
}

/// Open the graphic a Graphic clip shows; false when `node` is not one.
pub fn open_graphic(page: &mut VideoCut, node: &str) -> bool {
    if let Some(path) = page.session.as_ref().and_then(|session| session.doc.graphic(node)).and_then(|graphic| match &graphic.src {
        makepad_story_cut::GraphicSrc::Tree(tree) => Some(tree.document.clone()), _ => None,
    }) {
        return open_tree_file(page, &path, Some(node.into())).is_ok();
    }
    let Some(asset) = graphic_src(page, node) else { return false };
    page.motion.desk.opening = Some((Some(node.to_string()), asset));
    page.motion.desk.send(MotionJob::Open { asset });
    page.status = "Opening the graphic…".into();
    page.repaint = true;
    true
}

pub fn open_tree_file(page: &mut VideoCut, path: &str, node: Option<String>) -> Result<(), String> {
    let tree = page.session.as_ref().and_then(|session| session.tree.as_ref()).ok_or("no cut tree is open")?;
    let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`"))?;
    if file.kind != makepad_asset_data::AssetKind::Motion { return Err(format!("{path}: not a Motion plate")); }
    page.motion.desk.tree_opening = Some((path.into(), node));
    page.repaint = true;
    Ok(())
}

/// Open a Motion document from its text (a file, or the AI's).
pub fn open_source(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, source: String, title: String, node: Option<String>, asset: Option<AssetRevisionRef>) {
    // The desk covers the edit's viewer: the edit stops, and its sound
    // with it (the desk plays the film's sound with the graphic).
    if page.playing.is_some() {
        page.shuttle(0.0);
    }
    if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<super::super::preview::CutPreview>() {
        preview.set_playing(cx, None);
    }
    let desk = &mut page.motion.desk;
    desk.session = Some(MotionSession::new(source, title, node, asset));
    desk.opening = None;
    desk.publishing = None;
    desk.module_publishing = None;
    desk.tree_seen = None;
    desk.doc_sent = None;
    desk.activity_sent = None;
    desk.closing = false;
    desk.seen = None;
    desk.changed_at = None;
    desk.save_error = None;
    desk.saved_at = None;
    let pane = desk.source.get_or_insert_with(|| super::SourcePane::new(DocKind::Motion, live_id!(m_source_pane), "motion.splash"));
    pane.reset(cx, ui, "motion.splash");
    page.motion.desk.repaint = true;
    page.repaint = true;
}

/// Leave the graphic: a change not saved yet is saved first.
pub fn close(page: &mut VideoCut) {
    publish_now(page);
    if page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.is_some()) && !settled(page) {
        page.motion.desk.closing = true;
        return;
    }
    page.motion.desk.session = None;
    page.motion.desk.source = None;
    page.motion.desk.opening = None;
    page.repaint = true;
    page.repaint_preview = true;
}

pub(super) fn reset(page: &mut VideoCut) {
    let desk = &mut page.motion.desk;
    desk.session = None;
    desk.source = None;
    desk.opening = None;
    desk.tree_opening = None;
    desk.tree_write = None;
    desk.tree_source = None;
    desk.tree_write_revision = None;
    desk.tree_seen = None;
    desk.closing = false;
    desk.doc_sent = None;
    desk.activity_sent = None;
    desk.publishing = None;
    desk.module_publishing = None;
    desk.seen = None;
}

/// The value a control writes (`0.25`, `-3`, `#ff8800`, `#ff880080`) as a
/// literal the running document can take.
fn literal_of(text: &str) -> Option<makepad_motion::schema::Literal> {
    use makepad_motion::schema::Literal;
    let text = text.trim();
    // A number in its unit, set as the unit's base (seconds, radians, px).
    if let Some((v, unit)) = super::code_timeline::written_number(text) {
        return Some(Literal::Number(match unit {
            "ms" => v / 1000.0,
            "deg" => v.to_radians(),
            _ => v,
        }));
    }
    let hex = text.strip_prefix('#')?;
    let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let byte = |i: usize| (digits[i * 2] * 16 + digits[i * 2 + 1]) as f32 / 255.0;
    match digits.len() {
        6 => Some(Literal::Color([byte(0), byte(1), byte(2), 1.0])),
        8 => Some(Literal::Color([byte(0), byte(1), byte(2), byte(3)])),
        _ => None,
    }
}

/// One gesture's values in the running document: the document's text as
/// the gesture began and each value it moved ([`makepad_motion::edit::Held`]).
/// The text is not written while the gesture goes on: each step is set in
/// the running document where it lands there, else the held text is loaded
/// on the preview worker and shown as it arrives; the release writes it, as
/// one step.
pub struct LiveDrag {
    gesture: u64,
    doc: usize,
    held: makepad_motion::edit::Held,
    /// A step was shown by loading the held text: every later step is too
    /// (the preview is what the viewer shows).
    previewed: bool,
}

/// Where bytes `then` of document `doc`'s text (as the session holds it)
/// are in the text shown now: within a held gesture the values it moved
/// have changed length and the session's text is the gesture's start;
/// otherwise the same bytes.
pub fn now_of(page: &VideoCut, doc: usize, then: std::ops::Range<usize>) -> std::ops::Range<usize> {
    match &page.motion.desk.live_drag {
        Some(drag) if drag.doc == doc => drag.held.now_of(then),
        _ => then,
    }
}

/// Document `doc`'s text as it reads now: a held gesture's (not written
/// yet), else the session's.
pub fn text_now(page: &VideoCut, doc: usize) -> Option<String> {
    if let Some(drag) = page.motion.desk.live_drag.as_ref().filter(|d| d.doc == doc) {
        return Some(drag.held.text());
    }
    let session = page.motion.desk.session.as_ref()?;
    if doc == 0 { Some(session.source.clone()) } else { session.modules.get(doc - 1).map(|m| m.source.clone()) }
}

/// What a dragged value's step does to the document's text.
enum DragStep {
    /// Held: set in the running document or loaded as a preview; the text
    /// is written at the release.
    Held,
    /// Written now, as any edit.
    Write,
    /// The release of a held gesture: its text as the values left it,
    /// written as one step.
    WriteAll(String),
}

/// A held gesture that will not be released (another gesture began, an
/// undo): what it held written now, as one step.
fn finish_drag(page: &mut VideoCut, drag: LiveDrag) {
    let all = drag.held.text();
    page.motion.desk.live_patched = false;
    if all == drag.held.base() {
        if drag.previewed {
            super::value_pick::unpreview(page);
        }
        return;
    }
    let key = format!("code:{}:{}", drag.doc, drag.gesture);
    let Some(session) = &mut page.motion.desk.session else { return };
    let written = if drag.doc == 0 {
        session.replace_burst(all.clone(), "set value", &key)
    } else {
        match session.modules.get(drag.doc - 1).map(|m| m.key.clone()) {
            Some(module) => session.replace_module(&module, all.clone(), "set value", Some(&key)),
            None => false,
        }
    };
    session.end_burst();
    super::value_pick::adopt(page, &[(drag.doc, all)]);
    if written {
        changed(page);
    }
}

/// A value dragged in the code (a number, a time on a lane, a key, a
/// swatch, words typed): set everywhere it landed at once
/// (`Cx::patch_literal`: the running document's VM and what its load stored
/// it in, shader slots, kernel parameters), so the next frame shows it
/// without a reload or a recompile. A value the running document cannot
/// take (it fed something only a load makes, it is words) is shown by
/// loading the held text on the preview worker (the newest step only, off
/// the main thread). The text is written at the release, one step.
fn live_number(page: &mut VideoCut, cx: &mut Cx, doc: usize, start: usize, end: usize, text: &str, gesture: u64, done: bool) -> DragStep {
    // Another held gesture that did not end: written first.
    if let Some(other) = page.motion.desk.live_drag.take() {
        if other.gesture == gesture && other.doc == doc {
            page.motion.desk.live_drag = Some(other);
        } else {
            finish_drag(page, other);
        }
    }
    let Some(session) = &page.motion.desk.session else { return DragStep::Write };
    // (A document that does not load now is held too: its steps are
    // previewed, the release writes once.)
    let (file, now) = if doc == 0 {
        (makepad_motion::schema::CodeFile::Doc, session.source.clone())
    } else {
        match session.modules.get(doc - 1) {
            Some(m) => (makepad_motion::schema::CodeFile::Module(m.key.clone()), m.source.clone()),
            None => return DragStep::Write,
        }
    };
    // The text the running document was made from: as the gesture began.
    let mut drag = page.motion.desk.live_drag.take().unwrap_or_else(|| LiveDrag { gesture, doc, held: makepad_motion::edit::Held::new(now), previewed: false });
    // This value's bytes then (in the text the running document was
    // loaded from): a value moved before by where it starts or any
    // overlap, whatever end the writer gives (a writer that read the text
    // before the last step gives the old length; a second spot over the
    // same bytes would write them twice, eating what follows them).
    let then = drag.held.then_of(start..end);
    // Every other literal keeps its site while this one grows.
    let reach = makepad_value_pickers::panel::host::timed("edit.patch_literal", || literal_of(text).and_then(|value| makepad_motion::schema::site_value(&file, drag.held.base(), then.clone(), value)).map(|(site, value)| cx.patch_literal(&site, value)));
    let patched = reach.is_some_and(|reach| reach.places() > 0 && !reach.needs_reload());
    drag.held.set(then, text);
    if patched {
        makepad_value_pickers::panel::host::timed("edit.reread_running", || reread_running(page));
        page.motion.desk.live_patched = true;
        page.motion.desk.live_rev += 1;
        page.motion.desk.repaint = true;
    }
    if done {
        // The release: the text as the values left it, one step.
        page.motion.desk.live_patched = false;
        return DragStep::WriteAll(drag.held.text());
    }
    if !patched || drag.previewed {
        drag.previewed = true;
        let texts = vec![(doc, drag.held.text())];
        page.motion.desk.live_drag = Some(drag);
        makepad_value_pickers::panel::host::timed("edit.preview", || super::value_pick::preview(page, cx, texts));
        return DragStep::Held;
    }
    page.motion.desk.live_drag = Some(drag);
    DragStep::Held
}

/// How the open graphic loads in the open edit, when it is placed in it.
pub(super) fn film(page: &VideoCut) -> Option<makepad_story_edit_render::graphic::FilmLoad> {
    page.motion.desk.film.as_ref().map(|(_, f)| f.clone())
}

/// The open graphic's place in the open edit, from the edit's renderer
/// (asked again when the edit or the graphic changes): once it is known
/// the document is loaded again as the film loads it. A graphic outside
/// any edit stays on its own.
fn refresh_film(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    let key = session.node.clone().zip(page.session.as_ref().map(|e| e.revision)).map(|(node, rev)| (rev, node));
    if page.motion.desk.film.as_ref().map(|(k, _)| k) == key.as_ref() {
        return;
    }
    let Some((rev, node)) = key else {
        if page.motion.desk.film.take().is_some() {
            force_reload(page);
        }
        return;
    };
    let Some(film) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<super::super::preview::CutPreview>().and_then(|mut p| p.film_load(&node)) else { return };
    // (An edit revision that leaves the graphic's place as it was, the
    // desk's own save re-pointing its clip: the same document.)
    let same = page.motion.desk.film.as_ref().is_some_and(|((_, n), f)| *n == node && f.fingerprint == film.fingerprint);
    page.motion.desk.film = Some(((rev, node), film));
    if !same {
        force_reload(page);
    }
}

/// The document loaded again from the same text (how it loads changed).
fn force_reload(page: &mut VideoCut) {
    if let Some(session) = &mut page.motion.desk.session {
        if session.doc_of == session.revision {
            session.doc_of = session.revision.wrapping_sub(1);
        }
    }
}

/// Ask the checker for the model of the session's source.
/// Any edit of the document's or a module's text (typed, an inspector's,
/// the AI's, a palette's) that changed only literals from the text the running
/// document was loaded from shows at once: each changed literal is set
/// everywhere it landed (`Cx::patch_literal`). The edit loads as usual
/// behind it (the code stays the truth: the load draws what the patch
/// drew); a change of anything else, or a literal that fed something only
/// a load makes again, waits for that load.
fn patch_literal_edits(page: &mut VideoCut, cx: &mut Cx) {
    let desk = &mut page.motion.desk;
    if desk.live_drag.is_some() {
        return;
    }
    let Some(session) = &desk.session else { return };
    let Some(running) = &session.doc else { return };
    if session.doc_of == session.revision {
        return;
    }
    // The texts now: the document's and each module's (one string, to know
    // whether this state was patched already).
    let mut now = session.source.clone();
    for m in &session.modules {
        now.push('\u{0}');
        now.push_str(&m.source);
    }
    if desk.literal_patched.as_deref() == Some(now.as_str()) {
        return;
    }
    desk.literal_patched = Some(now.as_str().into());
    // (file, text the running document loaded, text now)
    let mut files: Vec<(String, std::sync::Arc<str>, &str)> = vec![(makepad_motion::DOC_CODE_FILE.to_string(), running.source.text_arc(), session.source.as_str())];
    for m in &session.modules {
        if let Some(loaded) = running.loaded_module_text(&m.key) {
            files.push((format!("module:{}", m.key), loaded, m.source.as_str()));
        }
    }
    let mut shown = false;
    for (file, loaded, text) in files {
        if loaded.as_ref() == text {
            continue;
        }
        let Some(edits) = makepad_widgets::makepad_script::literal::literal_edits(&loaded, text) else { continue };
        for e in edits {
            let site = LiteralSite::new(&file, e.line(), e.col());
            shown |= cx.patch_literal(&site, e.value()).places() > 0;
        }
    }
    if shown {
        desk.live_rev += 1;
        desk.repaint = true;
        reread_running(page);
    }
}

/// After a patch reached the running document: its model read again from
/// the same VM (a layer's static field copied from a patched value at load
/// follows; nothing is evaluated again).
fn reread_running(page: &mut VideoCut) {
    let Some(session) = &mut page.motion.desk.session else { return };
    let Some(doc) = &session.doc else { return };
    if let Ok(doc) = doc.reread() {
        session.doc = Some(std::sync::Arc::new(doc));
    }
}

fn request_doc(page: &mut VideoCut) {
    // The running document holds a dragged value already: loaded when the
    // drag ends.
    if page.motion.desk.live_drag.is_some() && page.motion.desk.live_patched {
        return;
    }
    let motion = &mut page.motion;
    if motion.desk.doc_sent.is_some() {
        return;
    }
    let Some(session) = &motion.desk.session else { return };
    if session.doc_of == session.revision {
        return;
    }
    let Some(checker) = &motion.checker else { return };
    let film = motion.desk.film.as_ref().map(|(_, f)| f.clone());
    let in_film = film.is_some();
    let tree = session.tree_path.as_ref().and_then(|path| page.session.as_ref()?.tree.clone().map(|tree| (tree, path.clone())));
    let check = Check { serial: motion.serial + 1, kind: DocKind::Motion, source: session.source.clone(), modules: session.edited_modules(), activity: true, film, tree, diagnose: false };
    let revision = session.revision;
    if checker.try_send(check).is_ok() {
        motion.serial += 1;
        motion.desk.doc_sent = Some(motion.serial);
        motion.desk.doc_sent_at = Some(std::time::Instant::now());
        motion.desk.activity_sent = Some(motion.serial);
        motion.desk.doc_rev = revision;
        motion.desk.doc_sent_film = in_film;
    }
}

/// A checker answer for the desk (a model load); true when it was one.
pub fn checked(page: &mut VideoCut, checked: &super::checker::Checked) -> bool {
    // The film sampled (the check's second answer): the ribbons.
    if let Some(samples) = &checked.activity {
        if page.motion.desk.activity_sent != Some(checked.serial) {
            return true;
        }
        // The coarse look keeps the fine one coming.
        if !checked.coarse {
            page.motion.desk.activity_sent = None;
        }
        if let Some(session) = &page.motion.desk.session {
            if session.source == checked.source {
                page.motion.desk.activity = (session.doc_of, samples.len(), activity_rows(session, samples));
                page.motion.desk.series = value_series(session, samples);
                page.motion.desk.paths = value_paths(session, samples);
                page.motion.desk.activity_rev += 1;
                page.motion.desk.repaint = true;
            }
        }
        return true;
    }
    let desk = &mut page.motion.desk;
    if desk.doc_sent != Some(checked.serial) {
        return false;
    }
    desk.doc_sent = None;
    if let Some(at) = desk.doc_sent_at.take() {
        if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("motion.reload") {
            log!("motion.reload: the edited document loaded {:.1} ms after it was sent", at.elapsed().as_secs_f64() * 1000.0);
        }
    }
    if let Some(session) = &mut desk.session {
        if session.source == checked.source {
            session.doc_of = desk.doc_rev;
            desk.doc_in_film = desk.doc_sent_film;
            session.problems = checked.problems.clone();
            session.warnings = checked.warnings.clone();
            if let Some(doc) = &checked.motion {
                session.loaded_modules(&checked.modules);
                session.doc = Some(with_characters(doc, &desk.character_svgs));
            }
        }
    }
    desk.repaint = true;
    true
}

/// A thing selected on the picture: the code that drew it (innermost
/// first), what it is, where it is drawn, its words (a text), the paths
/// its drawing reads (top-level `let`s, `module.name` for a module's) and
/// where its text comes from. What the scene's editor panel and chat take
/// as the context of an ask.
#[derive(Clone, Debug, PartialEq)]
pub struct Selected {
    pub code: Vec<(makepad_motion::schema::CodeFile, usize)>,
    pub label: String,
    pub quad: [[f64; 2]; 4],
    pub words: Option<String>,
    pub paths: Vec<String>,
    /// Its text's origin: (doc, its words' bytes); doc 0 the graphic,
    /// `k + 1` module `k`.
    pub origin: Option<(usize, std::ops::Range<usize>)>,
    /// What that origin is, said (`sung word 3 of line “A red thread of”
    /// · song lyrics`): what the selected-text box edits.
    pub origin_said: Option<String>,
}

impl Desk {
    /// Which picture the picks are of (model, time), when made.
    pub(super) fn picks_key(&self) -> Option<(u64, f64)> {
        self.picks.as_ref().map(|p| (p.0, p.1))
    }
}

/// What the picture at the playhead drew that a click can pick, when it
/// was made.
pub(super) fn picked_things(page: &VideoCut) -> Option<&Vec<super::picking::Pickable>> {
    page.motion.desk.picks.as_ref().map(|p| &p.2)
}

/// The layers' boxes in the picture at the playhead (model paths), when
/// the picture was made.
pub(super) fn layer_boxes(page: &VideoCut) -> Option<&Vec<(String, [[f64; 2]; 4])>> {
    page.motion.desk.picture.as_ref().map(|p| &p.3)
}

/// What the code's `let`s held over the film, sampled after each load (a
/// file's: (byte, value per sample, NaN where not bound)), and how far the
/// sampling is: (the model, the next sample).
pub(super) fn series<'a>(page: &'a VideoCut, file: &makepad_motion::schema::CodeFile) -> Option<&'a Vec<(usize, Vec<f32>)>> {
    page.motion.desk.series.get(file)
}

/// When each code region draws over the film (per file, per region of its
/// outline, a row of amounts at the checker's samples), and the model.
pub(super) fn activity_of<'a>(page: &'a VideoCut, file: &makepad_motion::schema::CodeFile) -> Option<&'a Vec<Vec<f32>>> {
    page.motion.desk.activity.2.get(file)
}

/// What is drawn in the paused frame (each with the code that drew it).
pub(super) fn picks(page: &VideoCut) -> Option<&[super::picking::Pickable]> {
    page.motion.desk.picks.as_ref().map(|(_, _, list)| list.as_slice())
}

/// Let go of the picture's selection (the panel's root filter cleared).
pub(super) fn clear_selection(page: &mut VideoCut) {
    if !page.motion.desk.selection.is_empty() {
        page.motion.desk.selection.clear();
        page.motion.desk.selection_rev += 1;
        page.motion.desk.repaint = true;
    }
}

/// The picture's outlines: these quads (document units), or (None) the
/// selection's again.
pub(super) fn outline(page: &VideoCut, cx: &mut Cx, ui: &WidgetRef, quads: Option<Vec<[[f64; 2]; 4]>>) {
    let quads = quads.unwrap_or_else(|| page.motion.desk.selection.iter().map(|s| s.quad).collect());
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<MotionViewer>() {
        viewer.set_highlight(cx, quads, String::new());
    }
}

/// How much each code region draws at the paused playhead, per file (by
/// region index of the file's outline); empty while it plays.
pub(super) fn activity_now(page: &VideoCut) -> &std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<f32>> {
    &page.motion.desk.activity_now
}

pub(super) fn series_rev(page: &VideoCut) -> (u64, usize) {
    (page.motion.desk.activity.0, page.motion.desk.activity.1)
}

/// The desk's selection and its revision: a reader keeps the revision it
/// read and reads again when it changes.
pub fn selection(page: &VideoCut) -> (u64, &[Selected]) {
    (page.motion.desk.selection_rev, &page.motion.desk.selection)
}

/// The words of the thing picked on the picture (the font picker's
/// sample), empty when it shows none.
pub(super) fn picked_words(page: &VideoCut) -> String {
    page.motion.desk.selection.last().and_then(|s| s.words.clone()).unwrap_or_default()
}

/// The paths a picked thing's drawing reads: at each place in the code
/// that drew it, the `let`s its call reads (nearest first), the top-level
/// ones; a module's as `module.name`.
fn read_paths(session: &MotionSession, code: &[(makepad_motion::schema::CodeFile, usize)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (file, byte) in code {
        let (outline, text, prefix) = match file {
            makepad_motion::schema::CodeFile::Doc => (&session.code, session.source.as_str(), String::new()),
            makepad_motion::schema::CodeFile::Module(key) => match session.modules.iter().find(|m| m.key == *key) {
                Some(m) => (&m.code, m.source.as_str(), format!("{}.", m.name)),
                None => continue,
            },
        };
        for g in makepad_motion::schema::code::gather(outline, text, *byte, 16) {
            let top = outline.regions.iter().any(|r| r.parent.is_none() && r.range == g.range);
            let path = format!("{prefix}{}", g.name);
            if top && !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// The running document was changed in place (a picker's live recolour or
/// numbers): its model read again from the VM (what a layer, a post pass or
/// the motion blur copied at load follows), drawn on the next frame.
pub(super) fn live_shown(page: &mut VideoCut) {
    reread_running(page);
    page.motion.desk.live_rev += 1;
    page.motion.desk.repaint = true;
}

/// The running document loaded again from the source (a live gesture put
/// away unwritten: its in-place values are not the code's).
pub(super) fn reload_running(page: &mut VideoCut) {
    if let Some(session) = &mut page.motion.desk.session {
        session.doc_of = u64::MAX;
    }
    request_doc(page);
    page.motion.desk.repaint = true;
}

/// Say something in the desk's bar.
pub(super) fn set_status(page: &mut VideoCut, text: impl Into<String>) {
    page.motion.desk.status = text.into();
    page.motion.desk.repaint = true;
}

/// Apply an op to the open graphic as one undo step.
fn apply(page: &mut VideoCut, op: MotionOp) {
    flush_live(page);
    let Some(session) = &mut page.motion.desk.session else { return };
    match session.apply(&op) {
        Ok(true) => changed(page),
        Ok(false) => {}
        Err(error) => page.motion.desk.status = format!("Refused: {error}"),
    }
    page.motion.desk.repaint = true;
}

pub(super) fn changed(page: &mut VideoCut) {
    page.motion.desk.changed_at = Some(Cx::time_now());
    page.motion.desk.save_error = None;
    request_doc(page);
}

/// The open graphic's live source, and the Graphic node that shows it
/// (for the AI: what the person sees, including unsaved changes).
pub fn source_of(page: &VideoCut) -> Option<(Option<String>, String)> {
    let session = page.motion.desk.session.as_ref()?;
    Some((session.node.clone(), session.source.clone()))
}

/// Replace the open graphic's source (the AI revising it) as one undo step
/// by `author`; the Source view shows exactly what changed.
pub fn replace_source(page: &mut VideoCut, source: String, author: &str) -> bool {
    let Some(session) = &mut page.motion.desk.session else { return false };
    if !session.replace(source, author) {
        return false;
    }
    page.motion.desk.author = Some(author.to_string());
    changed(page);
    page.motion.desk.repaint = true;
    true
}

/// Nothing of the open graphic waits to be published (none open, or its
/// last change is in the library and the clip points at it).
pub fn settled(page: &VideoCut) -> bool {
    let desk = &page.motion.desk;
    desk.publishing.is_none() && desk.tree_write.is_none()
        && desk.session.as_ref().is_none_or(|session| !session.dirty())
        && !desk.source.as_ref().is_some_and(super::SourcePane::has_draft)
}

/// All pending file texts enter the tree together, so a plate and its modules
/// can change their interface in one save. The worker checks the complete tree.
pub(super) fn stage_tree_sources(page: &mut VideoCut) -> Result<(), String> {
    if page.motion.desk.tree_write.is_some() { return Ok(()); }
    let owner = page.session.as_ref().filter(|session| session.tree.is_some()).ok_or("no cut tree is open")?;
    let graphic = page.motion.desk.session.as_ref().filter(|session| session.tree_path.is_some());
    if graphic.is_some_and(|session| session.project != owner.head.map(|head| head.asset_id)) {
        return Err("Reopen the plate in its project before saving".into());
    }
    let changes = super::source_changes(page);
    if changes.is_empty() { return Ok(()); }
    if let Some(session) = graphic {
        page.motion.desk.tree_source = Some(session.source.clone());
        page.motion.desk.tree_write_revision = Some(owner.revision);
        page.motion.desk.tree_write = Some(changes.clone());
    }
    page.submit(super::super::Pending::Document(changes));
    page.save_requested = true;
    Ok(())
}

pub(super) fn publish_now(page: &mut VideoCut) {
    if page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.is_some()) {
        if let Err(error) = stage_tree_sources(page) { page.motion.desk.save_error = Some(error); }
        return;
    }
    let desk = &mut page.motion.desk;
    let Some(session) = &desk.session else { return };
    if (!session.dirty() && !desk.source.as_ref().is_some_and(super::SourcePane::has_draft)) || desk.publishing.is_some() || session.outline_error.is_some() {
        return;
    }
    // Edited modules first: each becomes its next revision and the imports
    // follow it; the graphic is published after (its source changed then).
    if let Some(module) = session.modules.iter().find(|m| m.source != m.saved) {
        let Ok(asset) = module.id.parse() else { return };
        desk.publish_serial += 1;
        desk.publishing = Some(desk.publish_serial);
        desk.module_publishing = Some(module.key.clone());
        let job = MotionJob::Publish { serial: desk.publish_serial, asset: Some(asset), title: module.name.clone(), source: module.source.clone(), thumbnail: (0, 0, Vec::new()) };
        desk.send(job);
        return;
    }
    let thumbnail = sketch_thumbnail(session);
    desk.publish_serial += 1;
    desk.publishing = Some(desk.publish_serial);
    let job = MotionJob::Publish {
        serial: desk.publish_serial,
        asset: session.asset.map(|asset| asset.asset_id),
        title: session.title.clone(),
        source: session.source.clone(),
        thumbnail,
    };
    desk.send(job);
}

/// The film's samples as ribbons: per file, per code region, how much it
/// drew at each sample (a region counts what its inner regions draw).
/// What each `let` held over the film, from the checker's samples: per
/// file, (byte, a value per sample, NaN where it was not bound then).
fn value_series(session: &MotionSession, samples: &[super::checker::ActivitySample]) -> std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, Vec<f32>)>> {
    use makepad_motion::schema::{CodeAt, CodeFile};
    use std::collections::{BTreeMap, HashMap};
    let n = samples.len();
    let mut places: HashMap<CodeAt, Option<(CodeFile, usize)>> = HashMap::new();
    let mut out: HashMap<CodeFile, BTreeMap<usize, Vec<f32>>> = HashMap::new();
    for (k, sample) in samples.iter().enumerate() {
        for (at, v) in &sample.values {
            let place = places.entry(at.clone()).or_insert_with(|| {
                let text = super::picking::text_of(session, &at.file)?;
                Some((at.file.clone(), at.byte_in(text)?))
            });
            if let Some((file, byte)) = place {
                out.entry(file.clone()).or_default().entry(*byte).or_insert_with(|| vec![f32::NAN; n])[k] = *v as f32;
            }
        }
    }
    out.into_iter().map(|(file, series)| (file, series.into_iter().collect())).collect()
}

/// Once the paused playhead has rested a moment: what the code's `let`s
/// hold there (the rows' values) and what each piece of code drew (their
/// thumbnails). Dragging the playhead skips both.
fn settle(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let Some((of, t, since)) = page.motion.desk.settle else { return };
    // (The page's pump comes back each frame of its timer.)
    if Cx::time_now() - since < 0.12 {
        return;
    }
    page.motion.desk.settle = None;
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    if session.doc_of != of || session.playhead.to_bits() != t || session.playing.is_some() {
        return;
    }
    let Some(doc) = session.doc.clone() else { return };
    let Some((_, _, _, boxes)) = page.motion.desk.picture.clone() else { return };
    let (mut frame, lets) = doc.eval_watched(session.playhead);
    super::session::without_hidden(&mut frame, &session.hidden);
    let mut sites = std::mem::take(&mut page.motion.desk.sites);
    let places = sites.resolve(session, &doc, &lets.iter().map(|(ip, _, _)| *ip).collect::<Vec<_>>());
    let mut lets_now: std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, String)>> = Default::default();
    for ((_, _, value), at) in lets.into_iter().zip(places) {
        if let Some((file, byte)) = at {
            // A loop's: its last value, and how many times it ran.
            let text = if value.binds > 1 { format!("{}  ×{}", value.text, value.binds) } else { value.text };
            lets_now.entry(file).or_default().push((byte, text));
        }
    }
    let list = super::picking::pickables(session, &doc, &frame, &boxes, &mut sites);
    let drawn: Vec<(usize, usize, [[f64; 2]; 4], Option<([f64; 2], [f64; 2], f64)>)> = list.iter().filter(|p| p.content).filter_map(|p| {
        let (file, byte) = p.code.first()?;
        Some((doc_index(session, file)?, *byte, p.quad, p.seg))
    }).collect();
    // The pick, in this frame: the thing drawn by the same code with the
    // same label, outlined again; gone, the outline stays off (the pane
    // keeps its trace).
    // (What is drawn, by the same code; the same label first, then the
    // one nearest where it was.)
    let follow: Vec<Option<[[f64; 2]; 4]>> = page.motion.desk.selection.iter().map(|s| {
        let centre = |q: &[[f64; 2]; 4]| [(q[0][0] + q[2][0]) * 0.5, (q[0][1] + q[2][1]) * 0.5];
        let w = centre(&s.quad);
        let near = |p: &&super::picking::Pickable| { let c = centre(&p.quad); (c[0] - w[0]).powi(2) + (c[1] - w[1]).powi(2) };
        let same: Vec<&super::picking::Pickable> = list.iter().filter(|p| p.content && p.code == s.code).collect();
        same.iter().filter(|p| p.label == s.label).min_by(|a, b| near(a).total_cmp(&near(b))).or_else(|| same.iter().min_by(|a, b| near(a).total_cmp(&near(b)))).map(|p| p.quad)
    }).collect();
    page.motion.desk.picks = Some((of, session.playhead, list));
    page.motion.desk.sites = sites;
    page.motion.desk.lets_now = lets_now;
    page.motion.desk.repaint = true;
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let _ = drawn;
    // Each selected thing outlined again where it is drawn in this frame.
    if follow.iter().any(Option::is_some) {
        page.motion.desk.pick_outlined = true;
        for (s, quad) in page.motion.desk.selection.iter_mut().zip(&follow) {
            if let Some(q) = quad {
                s.quad = *q;
            }
        }
        let quads: Vec<[[f64; 2]; 4]> = follow.iter().flatten().copied().collect();
        if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<MotionViewer>() {
            viewer.set_highlight(cx, quads, String::new());
        }
    }
}

/// What the scrubber's filmstrip is made from: the open graphic alone as
/// an edit (where its clip starts there, its length) with the desk's texts
/// served in place of the library's. `None` when the graphic is not placed
/// in the edit.
fn film_source(page: &VideoCut, key: u64) -> Option<super::filmstrip::FilmSource> {
    let session = page.motion.desk.session.as_ref()?;
    let node = session.node.as_deref()?;
    let edit = page.session.as_ref()?;
    let mini = super::super::clip_films::mini_edit(&edit.doc, &edit.source, node)?;
    let doc = std::sync::Arc::new(match &edit.tree {
        Some(tree) => makepad_story_cut::CutDoc::load_tree(&mini, tree.context(&edit.path)),
        None => makepad_story_cut::CutDoc::load(&mini),
    }.ok()?);
    let texts: std::collections::HashMap<String, String> = session.asset.as_ref().map(|asset| (asset.asset_id.to_string(), session.source.clone())).into_iter()
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| (m.id.clone(), m.source.clone())))
        .collect();
    let start = page.projection.block(node).map_or(0.0, |b| b.start);
    // The document the checker loaded for this revision (the strip waits
    // for it: `paint` makes the source only once it is this revision's).
    let motion = session.doc.clone().filter(|_| session.doc_of == session.revision && page.motion.desk.doc_in_film)?;
    Some(super::filmstrip::FilmSource { key, doc, start, duration: session.duration().max(1e-3), texts: std::sync::Arc::new(texts), graphic: (node.to_string(), motion) })
}

/// How long a scrub's touch of sound lasts at each new place.
const SCRUB_TOUCH: f64 = 0.12;

/// The film's sound under the open graphic, played with the desk: graphic
/// time `T` is edit time `block.start + T`, so
/// play, pause, the loop back to the start and every seek move the sound
/// with the playhead, and a scrub sounds a short touch at each new place.
/// The edit's own mix (`engine::sound_mix`, played as the edit's viewer
/// plays it); silent when the graphic is not placed in the edit or the
/// page is not shown.
fn play_sound(page: &mut VideoCut, cx: &mut Cx, shown: bool) {
    let start = page.motion.desk.session.as_ref().filter(|_| shown)
        .and_then(|session| session.node.as_deref())
        .and_then(|node| page.projection.block(node))
        .map(|block| block.start);
    let edit = page.session.as_ref().map(|edit| (edit.revision, edit.doc.clone()));
    let (Some(g0), Some((revision, doc))) = (start, edit) else {
        let desk = &mut page.motion.desk;
        desk.scrub = None;
        if desk.session.is_none() {
            desk.sound = None;
            desk.sound_rev = None;
        } else if let Some((_, sound)) = &mut desk.sound {
            sound.follow(None);
        }
        return;
    };
    // The mix is made again only when what sounds changed.
    if page.motion.desk.sound_rev != Some(revision) {
        page.motion.desk.sound_rev = Some(revision);
        let plan = page.eval(&doc).0.audio_plan(0.0, doc.duration);
        if page.motion.desk.sound.as_ref().is_none_or(|(of, _)| *of != plan) {
            let mut resolver = super::super::store::LazyStoreMedia::new();
            page.motion.desk.sound = match CutSound::new(cx, move || makepad_story_edit_render::engine::sound_mix(doc, &mut resolver)) {
                Ok(sound) => Some((plan, sound)),
                Err(error) => {
                    log!("graphic sound: {error}");
                    None
                }
            };
        }
    }
    let now = Cx::time_now();
    let desk = &mut page.motion.desk;
    let (Some(session), Some((_, sound))) = (&desk.session, &mut desk.sound) else { return };
    sound.pump();
    if session.playing.is_some() {
        desk.scrub = None;
        sound.follow(Some(g0 + session.playhead));
        return;
    }
    match desk.scrub {
        Some((t, None)) => {
            sound.play(g0 + t);            desk.scrub = Some((t, Some(now + SCRUB_TOUCH)));
            desk.frame_next = cx.new_next_frame();
        }
        Some((_, Some(until))) if now < until => desk.frame_next = cx.new_next_frame(),
        _ => {
            desk.scrub = None;
            sound.follow(None);
        }
    }
}

/// What each `let` holding a vector or a colour held over the film.
fn value_paths(session: &MotionSession, samples: &[super::checker::ActivitySample]) -> std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<(usize, Vec<[f32; 4]>, u8)>> {
    use makepad_motion::schema::{CodeAt, CodeFile};
    use std::collections::{BTreeMap, HashMap};
    let n = samples.len();
    let mut places: HashMap<CodeAt, Option<(CodeFile, usize)>> = HashMap::new();
    let mut out: HashMap<CodeFile, BTreeMap<usize, (Vec<[f32; 4]>, u8)>> = HashMap::new();
    for (k, sample) in samples.iter().enumerate() {
        for (at, v, kind) in &sample.vectors {
            let place = places.entry(at.clone()).or_insert_with(|| {
                let text = super::picking::text_of(session, &at.file)?;
                Some((at.file.clone(), at.byte_in(text)?))
            });
            if let Some((file, byte)) = place {
                let entry = out.entry(file.clone()).or_default().entry(*byte).or_insert_with(|| (vec![[f32::NAN; 4]; n], *kind));
                entry.0[k] = *v;
            }
        }
    }
    out.into_iter().map(|(file, paths)| (file, paths.into_iter().map(|(b, (v, k))| (b, v, k)).collect())).collect()
}

pub(super) fn activity_rows(session: &MotionSession, samples: &[super::checker::ActivitySample]) -> std::collections::HashMap<makepad_motion::schema::CodeFile, Vec<Vec<f32>>> {
    use makepad_motion::schema::{CodeAt, CodeFile};
    use std::collections::HashMap;
    let n = samples.len();
    let mut out: HashMap<CodeFile, Vec<Vec<f32>>> = HashMap::new();
    // Where each call site is: its file's region chain (asked once each).
    let mut chains: HashMap<CodeAt, Option<(CodeFile, Vec<usize>)>> = HashMap::new();
    let starts: HashMap<CodeFile, Vec<usize>> = std::iter::once((CodeFile::Doc, &session.source))
        .chain(session.modules.iter().map(|m| (CodeFile::Module(m.key.clone()), &m.source)))
        .map(|(f, text)| (f, std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect()))
        .collect();
    let code_of = |file: &CodeFile| -> Option<(&str, &makepad_motion::schema::code::CodeOutline)> {
        match file {
            CodeFile::Doc => Some((session.source.as_str(), &session.code)),
            CodeFile::Module(key) => session.modules.iter().find(|m| m.key == *key).map(|m| (m.source.as_str(), &m.code)),
        }
    };
    let chain_of = |code: &makepad_motion::schema::code::CodeOutline, byte: usize| -> Vec<usize> {
        let mut chain = Vec::new();
        let mut r = code.region_at(byte);
        while let Some(i) = r {
            chain.push(i);
            r = code.regions[i].parent;
        }
        chain
    };
    let add = |out: &mut HashMap<CodeFile, Vec<Vec<f32>>>, file: &CodeFile, chain: &[usize], count: usize, k: usize, amount: f32| {
        let rows = out.entry(file.clone()).or_insert_with(|| vec![vec![0.0; n]; count]);
        for &r in chain {
            if let Some(row) = rows.get_mut(r) {
                row[k] += amount;
            }
        }
    };
    let layers = session.outline.all();
    let mut layer_chains: HashMap<String, Vec<usize>> = HashMap::new();
    for (k, sample) in samples.iter().enumerate() {
        for (at, amount) in &sample.sites {
            let found = chains.entry(at.clone()).or_insert_with(|| {
                let (text, code) = code_of(&at.file)?;
                let lines = starts.get(&at.file)?;
                // The written line, when it reads like the evaluated one;
                // else found again by its text.
                let row = (at.line.max(1) as usize - 1).min(lines.len().saturating_sub(1));
                let line_end = text[lines[row]..].find('\n').map_or(text.len(), |e| lines[row] + e);
                let same = at.line_text.as_deref().is_none_or(|l| l.trim() == text[lines[row]..line_end].trim());
                let byte = if same {
                    let line = &text[lines[row]..line_end];
                    lines[row] + line.char_indices().nth(at.col.saturating_sub(1) as usize).map_or(line.len(), |(b, _)| b)
                } else {
                    at.byte_in(text)?
                };
                Some((at.file.clone(), chain_of(code, byte)))
            });
            if let Some((file, chain)) = found {
                let count = code_of(file).map_or(0, |(_, c)| c.regions.len());
                add(&mut out, file, chain, count, k, *amount);
            }
        }
        for path in &sample.layers {
            let chain = layer_chains.entry(path.clone()).or_insert_with(|| {
                layers.iter().filter(|l| *path == l.path || path.starts_with(&format!("{}.", l.path)) || path.starts_with(&format!("{}#", l.path)))
                    .max_by_key(|l| l.path.len()).map(|l| chain_of(&session.code, l.range.start)).unwrap_or_default()
            });
            add(&mut out, &CodeFile::Doc, chain, session.code.regions.len(), k, 1.0);
        }
    }
    out
}

/// The code view's documents: the graphic and its modules, with what their
/// `let`s computed and hold.
fn code_docs(desk: &Desk, session: &MotionSession) -> Vec<super::code_timeline::CodeDoc> {
    use makepad_motion::schema::CodeFile;
    let mut docs = Vec::new();
    // Animated layer fields' values at the playhead, at their lines' ends.
    let mut live = desk.lets_now.get(&CodeFile::Doc).cloned().unwrap_or_default();
    for layer in session.outline.all() {
        for prop in layer.props.iter().filter(|prop| prop.form.is_animated()) {
            if let Some(values) = session.value_now(&layer.path, &prop.name) {
                live.push((prop.range.start, super::session::show(&prop.name, &values)));
            }
        }
    }
    // What the `let`s computed as the graphic loaded, per document.
    let mut bound = vec![Vec::new(); 1 + session.modules.len()];
    if let Some(doc) = session.doc.as_ref() {
        // Only the top-level `let`s': a name one of the documents gives a `let`.
        let names: std::collections::HashSet<&str> = std::iter::once(&session.code).chain(session.modules.iter().map(|m| &m.code))
            .flat_map(|code| code.regions.iter().filter(|r| r.kind == makepad_motion::schema::code::RegionKind::Let).map(|r| r.name.as_str())).collect();
        let lets: Vec<_> = doc.bound_at_load().into_iter().filter(|(_, name, _)| names.contains(name.as_str())).collect();
        let places = doc.code_at(&lets.iter().map(|(ip, _, _)| *ip).collect::<Vec<_>>());
        for ((_, name, value), at) in lets.into_iter().zip(places) {
            let Some(at) = at else { continue };
            let (Some(i), Some(text)) = (doc_index(session, &at.file), super::picking::text_of(session, &at.file)) else { continue };
            if let Some(byte) = at.byte_in(text) {
                bound[i].push((byte, name, value));
            }
        }
    }
    let mut bound = bound.into_iter();
    let series = desk.series.get(&CodeFile::Doc).cloned().unwrap_or_default();
    let paths = desk.paths.get(&CodeFile::Doc).cloned().unwrap_or_default();
    docs.push(super::code_timeline::CodeDoc { title: session.title.clone(), file: CodeFile::Doc, text: session.source.clone(), code: session.code.clone(), live, bound: bound.next().unwrap_or_default(), series, paths, lines: Vec::new(), read_only: false });
    for module in &session.modules {
        let file = CodeFile::Module(module.key.clone());
        let live = desk.lets_now.get(&file).cloned().unwrap_or_default();
        let series = desk.series.get(&file).cloned().unwrap_or_default();
        let paths = desk.paths.get(&file).cloned().unwrap_or_default();
        docs.push(super::code_timeline::CodeDoc { title: module.name.clone(), file, text: module.source.clone(), code: module.code.clone(), live, bound: bound.next().unwrap_or_default(), series, paths, lines: Vec::new(), read_only: module.builtin() });
    }
    // A call into a module names its arguments by that module's function.
    let fns: Vec<(String, Vec<String>)> = docs.iter().flat_map(|doc| doc.code.fns.iter().cloned()).collect();
    for doc in &mut docs {
        doc.code.name_args(&fns);
    }
    docs
}

/// The live values per document, without the code.
fn code_live_now(desk: &Desk, session: &MotionSession) -> Vec<Vec<(usize, String)>> {
    use makepad_motion::schema::CodeFile;
    let files: Vec<CodeFile> = std::iter::once(CodeFile::Doc).chain(session.modules.iter().map(|m| CodeFile::Module(m.key.clone()))).collect();
    let mut live: Vec<Vec<(usize, String)>> = files.iter().map(|file| desk.lets_now.get(file).cloned().unwrap_or_default()).collect();
    for layer in session.outline.all() {
        for prop in layer.props.iter().filter(|prop| prop.form.is_animated()) {
            if let Some(values) = session.value_now(&layer.path, &prop.name) {
                live[0].push((prop.range.start, super::session::show(&prop.name, &values)));
            }
        }
    }
    live
}

/// The document index of a code file in the code view.
fn doc_index(session: &MotionSession, file: &makepad_motion::schema::CodeFile) -> Option<usize> {
    match file {
        makepad_motion::schema::CodeFile::Doc => Some(0),
        makepad_motion::schema::CodeFile::Module(key) => session.modules.iter().position(|m| m.key == *key).map(|i| i + 1),
    }
}

/// Store replies, the model, the autosave and playback, then the desk's
/// widgets.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    if let Some(changes) = &page.motion.desk.tree_write {
        let applied = page.session.as_ref().filter(|session| Some(session.revision) != page.motion.desk.tree_write_revision).and_then(|session| session.tree.as_ref()).is_some_and(|tree| changes.iter().all(|change| match change {
            makepad_app_shell::tool_host::Change::Write { path, text, .. } => tree.text(path).ok() == Some(text.as_str()), _ => false,
        }));
        if applied {
            let changes = page.motion.desk.tree_write.take().unwrap();
            let submitted_source = page.motion.desk.tree_source.take();
            page.motion.desk.tree_write_revision = None;
            if let Some(session) = &mut page.motion.desk.session {
                for change in changes {
                    if let makepad_app_shell::tool_host::Change::Write { path, text, .. } = change {
                        if session.tree_path.as_deref() == Some(path.as_str()) {
                            if submitted_source.as_deref() == Some(session.source.as_str()) {
                                session.replace(text.clone(), "Source");
                            }
                            session.saved = text;
                            if let Some(pane) = &mut page.motion.desk.source { pane.sync(cx, ui, &session.source, "Source"); }
                        }
                        else if let Some(index) = session.modules.iter().position(|module| module.id == path || module.key == path) {
                            if session.modules[index].source == session.modules[index].saved {
                                let key = session.modules[index].key.clone();
                                session.replace_module(&key, text.clone(), "Source", None);
                            }
                            session.modules[index].saved = text;
                        }
                    }
                }
            }
            page.motion.desk.tree_write = None;
            page.motion.desk.save_error = None;
        } else if page.in_flight.is_none() && page.queue.is_empty() {
            page.motion.desk.tree_write = None;
            page.motion.desk.tree_source = None;
            page.motion.desk.tree_write_revision = None;
            page.motion.desk.tree_opening = None;
            page.motion.desk.closing = false;
            page.motion.desk.save_error = Some(page.status.clone());
        }
    }
    if page.motion.desk.closing && settled(page) {
        page.motion.desk.closing = false;
        page.motion.desk.session = None;
        page.motion.desk.source = None;
        page.repaint = true;
    }
    if page.motion.desk.tree_opening.is_some() {
        if !settled(page) { publish_now(page); }
        else if let Some((path, node)) = page.motion.desk.tree_opening.take() {
            let opened = page.session.as_ref().and_then(|session| Some((session.tree.as_ref()?.text(&path).ok()?.to_string(), session.head.map(|head| head.asset_id))));
            if let Some((source, project)) = opened {
                let title = patch::label_of(&source).unwrap_or_else(|| path.clone());
                open_source(page, cx, ui, source, title, node, None);
                if let Some(session) = &mut page.motion.desk.session { session.tree_path = Some(path.clone()); session.project = project; }
                if let Some(pane) = &mut page.motion.desk.source {
                    pane.reset(cx, ui, &path);
                    pane.tree = page.session.as_ref().and_then(|session| session.tree.clone());
                    if let Some(index) = page.motion.recovered.iter().position(|source| source.path == path) {
                        let recovered = page.motion.recovered.remove(index);
                        if let Some(session) = &page.motion.desk.session { pane.sync_detached(&session.source, "Recovered draft"); }
                        pane.restore_draft(recovered.text, recovered.base);
                        pane.resume(cx, ui);
                    }
                }
                force_reload(page);
            }
        }
    }
    // A click waiting for the renderer's pick: picked once it is back.
    if let Some((ticket, at, alt, shift)) = page.motion.desk.pick_wait {
        match cx.take_pick(ticket) {
            Some(result) => {
                if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("motion.pick") {
                    log!("motion.pick: the renderer answered {:?}", result.as_ref().map(|i| (i.width, i.height)));
                }
                page.motion.desk.pick_wait = None;
                if let Some((of, t, ..)) = &page.motion.desk.picture {
                    page.motion.desk.pick_image = Some((*of, *t, result.ok()));
                }
                let desk_ref = ui.widget(cx, ids!(cut_motion));
                point_action(page, cx, &desk_ref, at, true, alt, shift);
            }
            None => {
                cx.new_next_frame();
            }
        }
    }
    if page.motion.desk.store.is_none() {
        // Clicks on the picture ask the renderer what drew there: every
        // shader compiled from now on has its pick variant.
        cx.enable_pick_variants();
        let store = MotionStore::new(cx);
        if let Some(error) = &store.error {
            page.status = error.clone();
        }
        page.motion.desk.store = Some(store);
        page.motion.desk.source.get_or_insert_with(|| super::SourcePane::new(DocKind::Motion, live_id!(m_source_pane), "motion.splash"));
        if let Ok(path) = std::env::var("STAGE_OPEN_MOTION") {
            // A document from disk (an agent's or a person's): opened as a
            // new graphic, saved to the library on its first change.
            match std::fs::read_to_string(&path) {
                Ok(source) => {
                    let title = patch::outline(&source).ok().and_then(|_| patch::label_of(&source))
                        .unwrap_or_else(|| std::path::Path::new(&path).file_stem().map(|stem| stem.to_string_lossy().to_string()).unwrap_or_default());
                    open_source(page, cx, ui, source, title, None, None);
                }
                Err(error) => page.status = format!("{path}: {error}"),
            }
        }
    }
    // Edit mode's live shader literals while a document is open: its
    // shaders compile with their literals as slots a drag patches.
    // (No graphic open: the edit's panel's documents, when it has one.)
    let edited = match &page.motion.desk.session {
        Some(session) => std::iter::once(makepad_motion::DOC_CODE_FILE.to_string()).chain(session.modules.iter().map(|m| format!("module:{}", m.key))).collect(),
        None => super::super::cut_panel::live_files(page),
    };
    cx.set_live_literals(edited);
    makepad_value_pickers::panel::host::timed("desk.patch_literal_edits", || patch_literal_edits(page, cx));
    makepad_value_pickers::panel::host::timed("desk.flush", || page.motion.desk.flush());
    while let Some(reply) = page.motion.desk.store.as_ref().and_then(MotionStore::try_recv) {
        match reply {
            MotionReply::Opened { asset, result } => {
                let Some((node, wanted)) = page.motion.desk.opening.clone() else { continue };
                if wanted != asset {
                    continue;
                }
                page.motion.desk.opening = None;
                match result {
                    Ok((source, title)) => {
                        // Reopened on a revision someone else wrote: the view
                        // keeps its history and shows what changed.
                        let same = page.motion.desk.session.as_ref().is_some_and(|session| session.node == node && node.is_some());
                        if same {
                            // Whoever re-pointed the clip (the AI, undo in
                            // the edit) wrote this revision.
                            let author = page.motion.desk.author.take()
                                .or_else(|| (!page.motion.last_author.is_empty()).then(|| page.motion.last_author.clone()))
                                .unwrap_or_else(|| "Edit".into());
                            if let Some(session) = &mut page.motion.desk.session {
                                session.replace(source.clone(), &author);
                                session.asset = Some(asset);
                                session.saved = source;
                            }
                            page.motion.desk.author = Some(author);
                            request_doc(page);
                        } else {
                            open_source(page, cx, ui, source, title, node, Some(asset));
                        }
                        page.status.clear();
                    }
                    Err(error) => page.status = format!("The graphic does not open: {error}"),
                }
                page.repaint = true;
            }
            MotionReply::Media { media, result } => {
                let viewer = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_viewer));
                // A character's model SVG: the document is built again with it.
                if let (Ok((bytes, "svg")), makepad_motion::Media::Asset(asset)) = (&result, &media) {
                    let desk = &mut page.motion.desk;
                    if let Some(session) = &mut desk.session {
                        if let Some(doc) = session.doc.as_ref().filter(|doc| doc.character_models().contains(asset)) {
                            desk.character_svgs.insert(media.clone(), String::from_utf8_lossy(bytes).into_owned());
                            session.doc = Some(with_characters(doc, &desk.character_svgs));
                        }
                    }
                }
                let outcome = match result {
                    Ok((bytes, kind)) => viewer.borrow_mut::<MotionViewer>().map_or(Ok(()), |mut viewer| viewer.set_media(cx, media.clone(), &bytes, kind)),
                    Err(error) => Err(error),
                };
                if let Err(error) = outcome {
                    page.motion.desk.status = format!("An image the graphic shows did not load: {error}");
                }
                page.motion.desk.repaint = true;
            }
            MotionReply::Published { serial, source, result } => {
                if page.motion.desk.publishing != Some(serial) {
                    continue;
                }
                page.motion.desk.publishing = None;
                if let Some(key) = page.motion.desk.module_publishing.take() {
                    match result {
                        Ok(published) => {
                            if let Some(session) = &mut page.motion.desk.session {
                                session.module_published(&key, &source, &published.revision.to_string());
                                changed(page);
                            }
                            log!("module saved: {key} -> {}", published.revision);
                        }
                        Err(error) => {
                            log!("module save failed: {key}: {error}");
                            page.motion.desk.save_error = Some(error);
                        }
                    }
                    page.motion.desk.repaint = true;
                    continue;
                }
                match result {
                    Ok(published) => {
                        let mut repoint = None;
                        if let Some(session) = &mut page.motion.desk.session {
                            session.saved = source;
                            session.asset = Some(published);
                            repoint = session.node.clone();
                        }
                        page.motion.desk.saved_at = Some(clock_text());
                        log!("graphic saved: asset(\"{}\", \"{}\")", published.asset_id, published.revision);
                        if let Some(node) = repoint {
                            // The clip shows the new revision: an edit op, so
                            // the edit's undo and autosave have it too.
                            let (id, rev) = (published.asset_id.to_string(), published.revision.to_string());
                            let value = Value {
                                text: format!("asset(\"{id}\", \"{rev}\")"),
                                literal: Literal::Obj(vec![("ref".into(), Literal::Id("asset".into())), ("id".into(), Literal::Str(id)), ("rev".into(), Literal::Str(rev))]),
                            };
                            page.motion.author = Some("You · graphic".into());
                            page.motion.desk.repointing = Some(published);
                            page.op(CutOp::Param { node, key: "src".into(), value });
                        }
                    }
                    Err(error) => {
                        log!("graphic save failed: {error}");
                        page.motion.desk.save_error = Some(error);
                    }
                }
                page.motion.desk.repaint = true;
            }
        }
    }
    // The Graphic's revision changed under the open graphic (the AI wrote
    // a new one, or undo in the edit): follow it.
    let revision = page.session.as_ref().map(|session| session.revision);
    let tree_follow = if settled(page) && page.motion.desk.tree_seen != revision {
        page.motion.desk.tree_seen = revision;
        page.motion.desk.session.as_ref().and_then(|graphic| {
            let path = graphic.tree_path.as_ref()?;
            let owner = page.session.as_ref()?;
            let tree = owner.tree.as_ref()?;
            if graphic.project != owner.head.map(|head| head.asset_id) { return None; }
            let linked = graphic.node.as_deref().and_then(|node| owner.doc.graphic(node)).and_then(|graphic| match &graphic.src {
                makepad_story_cut::GraphicSrc::Tree(context) => Some(context.document.clone()), _ => None,
            });
            let next_path = linked.or_else(|| tree.files.contains_key(path).then(|| path.clone())).or_else(|| {
                let previous = page.motion.desk.source.as_ref()?.tree.as_ref()?.files.get(path)?;
                let mut candidates = tree.files.values().filter(|file| file.kind == previous.kind && match previous.reference.or(previous.base) {
                    Some(reference) => file.reference == Some(reference) || file.base == Some(reference),
                    None => file.bytes == previous.bytes,
                });
                let next = candidates.next()?;
                candidates.next().is_none().then(|| next.path.clone())
            });
            if next_path.as_ref() != Some(path) || tree.text(path).ok() != Some(graphic.source.as_str())
                || graphic.modules.iter().any(|module| !module.builtin() && tree.text(&module.id).ok() != Some(module.source.as_str())) {
                Some((next_path, graphic.node.clone()))
            } else { None }
        })
    } else { None };
    if let Some((path, node)) = tree_follow {
        if let Some(path) = path { page.motion.desk.tree_opening = Some((path, node)); }
        else {
            page.motion.desk.session = None;
            page.motion.desk.source = None;
            page.repaint = true;
        }
    }
    if let Some(pane) = &mut page.motion.desk.source {
        if page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.is_some()) { pane.tree = page.session.as_ref().and_then(|session| session.tree.clone()); }
    }
    // Our own save's re-point: done when the clip names it, or when no edit
    // op is left to bring it (it failed, or another write replaced it).
    if let Some(ours) = page.motion.desk.repointing {
        let node = page.motion.desk.session.as_ref().and_then(|session| session.node.clone());
        let landed = node.and_then(|node| graphic_src(page, &node)) == Some(ours);
        let waiting = page.in_flight.as_ref().is_some_and(|(_, pending)| matches!(pending, super::super::Pending::Op(_))) || page.queue.iter().any(|pending| matches!(pending, super::super::Pending::Op(_)));
        if landed || !waiting {
            page.motion.desk.repointing = None;
        }
    }
    let followed = page.motion.desk.session.as_ref().and_then(|session| {
        let node = session.node.clone()?;
        let now = graphic_src(page, &node)?;
        (Some(now) != session.asset && page.motion.desk.publishing.is_none() && page.motion.desk.opening.is_none() && page.motion.desk.repointing.is_none()).then_some((node, now))
    });
    if let Some((node, asset)) = followed {
        if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("motion.reload") {
            log!("motion.reload: {node} names revision {} (not the open one): reopening on it", asset.revision);
        }
        page.motion.desk.opening = Some((Some(node), asset));
        page.motion.desk.send(MotionJob::Open { asset });
    }
    // A live step waits for the next frame (see `write_live`).
    if std::mem::take(&mut page.motion.desk.live_wake) {
        page.motion.desk.frame_next = cx.new_next_frame();
    }
    refresh_film(page, cx, ui);
    makepad_value_pickers::panel::host::timed("desk.request_doc", || request_doc(page));
    // Autosave after a pause.
    let due = page.motion.desk.changed_at.is_some_and(|at| Cx::time_now() - at >= AUTOSAVE);
    if due {
        page.motion.desk.changed_at = None;
        makepad_value_pickers::panel::host::timed("desk.publish", || publish_now(page));
    }
    // Playback.
    let fps = page.motion.desk.session.as_ref().map_or(60.0, |session| session.fps());
    if let Some(session) = &mut page.motion.desk.session {
        if let Some((since, from)) = session.playing {
            let t = from + (Cx::time_now() - since);
            let duration = session.duration();
            session.playhead = if t >= duration { session.playing = Some((Cx::time_now(), 0.0)); 0.0 } else { (t * fps).floor() / fps };
            page.motion.desk.frame_next = cx.new_next_frame();
            page.motion.desk.repaint = true;
        }
    }
    play_sound(page, cx, shown);
    let open = page.motion.desk.session.is_some();
    if !shown {
        return;
    }
    ui.widget(cx, ids!(cut_split_box)).set_visible(cx, !open);
    ui.widget(cx, ids!(cut_motion)).set_visible(cx, open);
    if open {
        // One bar: the film as the way back, the graphic's name, its undo
        // (the edit's own undo is not this graphic's).
        for (id, on) in [(ids!(cut_undo), false), (ids!(cut_redo), false), (ids!(cut_title_tip), false), (ids!(cut_save_tip), false), (ids!(m_undo), true), (ids!(m_redo), true), (ids!(cut_scene), true)] {
            ui.widget(cx, id).set_visible(cx, on);
        }
        makepad_value_pickers::panel::host::timed("desk.settle", || settle(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.variants", || super::variants::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.deep_make", || super::deep_make::pump(page, cx));
        makepad_value_pickers::panel::host::timed("desk.scene_context", || super::scene_context::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.selected_text", || super::selected_text::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.editor_panel", || super::editor_panel::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.structure", || super::structure_panel::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.curves", || super::curve_editor::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.history", || super::history_panel::pump(page, cx, ui));
        makepad_value_pickers::panel::host::timed("desk.value_pick", || super::value_pick::pump(page, cx, &ui.widget(cx, ids!(cut_motion))));
        makepad_value_pickers::panel::host::timed("desk.paint", || paint(page, cx, ui));
        // Images and SVGs the frame shows, from the library (once each).
        let mut missing = ui.widget(cx, ids!(cut_motion)).widget(cx, ids!(m_viewer)).borrow::<MotionViewer>().map(|viewer| viewer.missing()).unwrap_or_default();
        // And the SVGs characters are built from (before they can draw).
        if let Some(doc) = page.motion.desk.session.as_ref().and_then(|session| session.doc.as_ref()) {
            missing.extend(doc.character_models().into_iter().map(makepad_motion::Media::Asset));
        }
        for media in missing {
            if page.motion.desk.media_asked.insert(media.clone()) {
                page.motion.desk.send(MotionJob::Media { media });
            }
        }
    } else {
        for (id, on) in [(ids!(cut_undo), true), (ids!(cut_redo), true), (ids!(cut_title_tip), true), (ids!(cut_save_tip), true), (ids!(m_undo), false), (ids!(m_redo), false), (ids!(cut_scene), false)] {
            ui.widget(cx, id).set_visible(cx, on);
        }
        ui.widget(cx, ids!(m_status)).set_text(cx, "");
    }
}

fn clock_text() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // Local time is the store's concern; the minute is enough here.
    let minutes = (secs / 60) % (24 * 60);
    format!("{:02}:{:02} UTC", minutes / 60, minutes % 60)
}

pub fn handle_event(page: &mut VideoCut, event: &Event) {
    if page.motion.desk.frame_next.is_event(event).is_some() {
        flush_live(page);
        // Painted by the page pump (its 30 Hz timer); what the paint changes
        // redraws itself. Never the whole window: under it is the code view,
        // the slowest thing to lay out.
        page.motion.desk.repaint = true;
    }
    if let Some(pane) = &mut page.motion.desk.source {
        if pane.handle_event(event) {
            if let Some(checker) = &page.motion.checker {
                pane.pump(checker, &mut page.motion.serial);
            }
        }
    }
}

fn selected_layer(page: &VideoCut) -> Option<String> {
    page.motion.desk.session.as_ref()?.selection.layer.clone()
}

pub(super) fn set_playhead(page: &mut VideoCut, t: f64) {
    if let Some(session) = &mut page.motion.desk.session {
        let fps = session.fps();
        let t = ((t.clamp(0.0, session.duration()) * fps).round()) / fps;
        // A new place sounds a touch of the film there (`play_sound`).
        if t != session.playhead || session.playing.is_some() {
            page.motion.desk.scrub = Some((t, None));
        }
        session.playhead = t;
        session.playing = None;
    }
    page.motion.desk.repaint = true;
}

/// Desk actions: the bar, the transport, the layers, the inspector, the
/// viewer, and the graph and source of the graphic.
pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    if page.motion.desk.session.is_none() {
        return;
    }
    super::variants::handle_actions(page, cx, ui, actions);
    super::scene_context::handle_actions(page, cx, ui, actions);
    super::selected_text::handle_actions(page, cx, ui, actions);
    super::structure_panel::handle_actions(page, cx, ui, actions);
    super::curve_editor::handle_actions(page, cx, ui, actions);
    super::history_panel::handle_actions(page, cx, ui, actions);
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    super::value_pick::handle_actions(page, cx, &desk_ref, actions);
    // The bar's (the project bar's) way back and undo.
    let clicked = |cx: &Cx, id: &[LiveId]| ui.widget(cx, id).as_button().clicked(actions);
    if clicked(cx, ids!(m_back)) {
        close(page);
        return;
    }
    if clicked(cx, ids!(m_undo)) {
        undo(page, false);
    }
    if clicked(cx, ids!(m_redo)) {
        undo(page, true);
    }
    // The inspector.
    let inspector = desk_ref.widget(cx, ids!(m_inspector));
    for action in actions.filter_widget_actions_cast::<InspectorAction>(inspector.widget_uid()) {
        // A font or a colour: its picker (`value_pick.rs`).
        if let InspectorAction::Pick { id, label } = &action {
            if !super::value_pick::open_field(page, cx, &desk_ref, id, label) {
                page.motion.desk.status = "That value is not written as a font or a colour here".into();
                page.motion.desk.repaint = true;
            }
            continue;
        }
        inspector_action(page, cx, action);
    }
    // The viewer.
    let viewer = desk_ref.widget(cx, ids!(m_viewer));
    for action in actions.filter_widget_actions_cast::<ViewerAction>(viewer.widget_uid()) {
        match action {
            ViewerAction::Point { at, click, alt, shift } => point_action(page, cx, &desk_ref, at, click, alt, shift),

            action => viewer_action(page, action),
        }
    }
    // The code on its timeline.
    let code = desk_ref.widget(cx, ids!(m_code));
    for action in actions.filter_widget_actions_cast::<super::code_timeline::CodeTimelineAction>(code.widget_uid()) {
        code_action(page, cx, &desk_ref, action);
    }
    // The source of the graphic.
    if let Some(mut pane) = page.motion.desk.source.take() {
        pane.handle_actions(cx, ui, actions);
        page.motion.desk.source = Some(pane);
    }
}

/// The last step undone (a write that broke the scene).
pub(super) fn undo_last(page: &mut VideoCut) {
    undo(page, false);
}

/// A held gesture written and a live drag finished (before the document
/// moves to another state).
pub(super) fn flush_and_settle(page: &mut VideoCut) {
    flush_live(page);
    if let Some(drag) = page.motion.desk.live_drag.take() {
        finish_drag(page, drag);
    }
}

fn undo(page: &mut VideoCut, redo: bool) {
    flush_and_settle(page);
    // The graphic's own steps (one per gesture, as many as were made since
    // it opened), autosaved or not: the project tree keeps one step per
    // autosave, so it would take back every gesture of that save at once
    // and reopen the graphic without its steps. The tree's history only
    // while the graphic has no steps of its own (just opened, or reopened
    // by a tree step); at the end of its own steps it stops there.
    let own = page.motion.desk.session.as_ref().is_some_and(|session| !session.undo.is_empty() || !session.redo.is_empty());
    if !own && page.motion.desk.session.as_ref().is_some_and(|session| session.tree_path.is_some()) && settled(page) {
        page.undo(redo);
        return;
    }
    let Some(session) = &mut page.motion.desk.session else { return };
    match session.undo(redo) {
        Some(what) => {
            page.motion.desk.status = format!("{} {what}", if redo { "Redone:" } else { "Undone:" });
            page.motion.desk.author = Some(if redo { "Redo".into() } else { "Undo".into() });
            changed(page);
        }
        None => page.motion.desk.status = if redo { "Nothing to redo".into() } else { "Nothing to undo".into() },
    }
    page.motion.desk.repaint = true;
}

/// A layer's words as typed (in its bar or on the canvas): written where
/// its text is (an input's default, a `let`), one undo step per edit.
fn layer_words(page: &mut VideoCut, path: String, text: String, end: bool) {
    let Some(session) = &page.motion.desk.session else { return };
    if session.locked.contains(&path) || session.outline.layer(&path).is_some_and(|l| l.locked) {
        return;
    }
    let op = session.write_op(&path, "text", patch::fmt_text(&text));
    write_live(page, format!("words:{path}:text"), op, end);
}

/// A lane's numbers as scrubbed: a key at the playhead when it is keyed,
/// else its value (an input's default when it reads one); one undo step.
fn lane_values(page: &mut VideoCut, path: String, prop: String, values: Vec<f64>, end: bool) {
    if values.is_empty() {
        return;
    }
    page.motion.desk.author = Some("You · timeline".into());
    let like = current_text(page, &path, &prop, None);
    let Some(session) = &page.motion.desk.session else { return };
    // A value written as one number stays one while its parts agree (a
    // uniform scale).
    let written_one = like.as_deref().and_then(patch::literal_numbers).is_some_and(|v| v.len() == 1)
        || session.value_site(&path, &prop).is_none() && prop == "scale";
    let values = if written_one && values.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9) { values[..1].to_vec() } else { values };
    let value = if is_color(&prop) && values.len() == 4 { patch::fmt_color([values[0], values[1], values[2], values[3]]) } else { patch::fmt_values(&values, like.as_deref()) };
    let op = session.write_op(&path, &prop, value);
    write_live(page, format!("lane:{path}:{prop}"), op, end);
}

// ------------------------------------------------------------------ curve lanes

/// The files paths resolve in: the graphic (`""`) and its editable modules,
/// and which session doc each is.
fn path_files(session: &MotionSession) -> (Vec<makepad_motion::paths::File<'_>>, Vec<usize>) {
    let files = std::iter::once(makepad_motion::paths::File { name: "", text: &session.source, code: &session.code })
        .chain(session.modules.iter().filter(|m| !m.builtin()).map(|m| makepad_motion::paths::File { name: &m.name, text: &m.source, code: &m.code }))
        .collect();
    let docs = std::iter::once(0).chain(session.modules.iter().enumerate().filter(|(_, m)| !m.builtin()).map(|(k, _)| k + 1)).collect();
    (files, docs)
}

/// The open lanes of the scene shown.
pub(super) fn lanes(page: &VideoCut) -> Vec<String> {
    let Some(session) = &page.motion.desk.session else { return Vec::new() };
    page.motion.desk.lanes.get(&session.title).cloned().unwrap_or_default()
}

/// Each open lane's keys, read from the code as it is now (a path that no
/// longer names keys shows none).
#[allow(dead_code)]
fn lane_data(session: &MotionSession, open: &[String]) -> Vec<super::code_timeline::Lane> {
    let (files, docs) = path_files(session);
    open.iter().filter_map(|path| {
        let site = makepad_motion::paths::resolve_exact(&files, path).ok()?;
        let keys = makepad_motion::lane::parse(files[site.file].text, site.range.clone())?;
        Some(super::code_timeline::Lane { path: path.clone(), label: path.clone(), doc: docs[site.file], keys })
    }).collect()
}

/// Open the curve lane of the keyed value at `path`; why not when it is
/// not `keys(…)`.
pub(super) fn open_lane(page: &mut VideoCut, path: &str) -> Result<(), String> {
    let Some(session) = &page.motion.desk.session else { return Err("no graphic is open".into()) };
    let (files, _) = path_files(session);
    let site = makepad_motion::paths::resolve(&files, path)?;
    makepad_motion::lane::parse(files[site.file].text, site.range.clone()).ok_or_else(|| format!("`{path}` is not keys(…): animate it first"))?;
    let title = session.title.clone();
    let open = page.motion.desk.lanes.entry(title).or_default();
    if !open.iter().any(|p| p == path) {
        open.push(path.to_string());
    }
    page.motion.desk.repaint = true;
    Ok(())
}

pub(super) fn close_lane(page: &mut VideoCut, path: &str) {
    let Some(session) = &page.motion.desk.session else { return };
    if let Some(open) = page.motion.desk.lanes.get_mut(&session.title) {
        open.retain(|p| p != path);
    }
    page.motion.desk.repaint = true;
}

/// Animate the value at `path`: its literal becomes `keys([T, v])` (T the
/// playhead, v its value), one undo step through the edit primitive; its
/// lane opens.
pub(super) fn animate(page: &mut VideoCut, cx: &mut Cx, path: &str) -> Result<(), String> {
    // `?path`: only its curve shown.
    let (path, show_only) = match path.strip_prefix('?') {
        Some(p) => (p, true),
        None => (path, false),
    };
    let Some(session) = &page.motion.desk.session else { return Err("no graphic is open".into()) };
    let (files, docs) = path_files(session);
    let site = makepad_motion::paths::resolve(&files, path)?;
    if let Some(keys) = makepad_motion::lane::parse(files[site.file].text, site.range.clone()) {
        // Keyed already: a key at the playhead (I), unless one is there.
        let t = session.playhead;
        let half = 0.5 / session.fps().max(1.0);
        let doc = docs[site.file];
        if !show_only && !keys.keys.iter().any(|k| k.t.is_some_and(|kt| (kt - t).abs() < half)) {
            if let Some(v) = makepad_motion::lane::sample(&keys, t) {
                let (range, text) = makepad_motion::lane::add_key(&keys, t, v);
                page.motion.desk.author = Some("You · key".into());
                code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, text, u64::MAX / 4 + 2, true)]);
            }
        }
        // Its curve shown when asked (the cue's click), or on the CURVES
        // page already; a key added from the panel stays in the panel.
        if show_only || page.motion.curves.shown {
            page.motion.curves.reveal = Some((doc, site.range.start));
        }
        return open_lane(page, path);
    }
    if show_only {
        return Err(format!("`{path}` is not animated"));
    }
    {
        let value = site.text.trim().to_string();
        let t = makepad_motion::patch::fmt_num(session.playhead);
        let written = makepad_motion::edit::write(&files, path, &makepad_motion::edit::Value::Code(format!("keys([{t}, {value}])")))?;
        let doc = docs[written.file];
        page.motion.desk.author = Some("You · animate".into());
        code_edits(page, cx, doc, vec![Edit::splice(written.range.start..written.range.end, written.literal, u64::MAX / 4, true)]);
        page.motion.curves.reveal = Some((doc, written.range.start));
    }
    open_lane(page, path)
}

/// Take a value's animation away: its `keys(…)` becomes the value it has
/// at the playhead (one undo step); its lane closes.
pub(super) fn unanimate(page: &mut VideoCut, cx: &mut Cx, path: &str) -> Result<(), String> {
    // `!path`: the animation taken away whole; else (Alt+I) the key at the
    // playhead, the whole animation with the last key.
    let (path, whole) = match path.strip_prefix('!') {
        Some(p) => (p, true),
        None => (path, false),
    };
    let Some(session) = &page.motion.desk.session else { return Err("no graphic is open".into()) };
    let (files, docs) = path_files(session);
    let site = makepad_motion::paths::resolve(&files, path)?;
    let keys = makepad_motion::lane::parse(files[site.file].text, site.range.clone()).ok_or_else(|| format!("`{path}` is not animated"))?;
    if !whole && keys.keys.len() > 1 {
        let half = 0.5 / session.fps().max(1.0);
        let at = keys.keys.iter().position(|k| k.t.is_some_and(|kt| (kt - session.playhead).abs() < half)).ok_or("no key at the playhead")?;
        let (range, text) = makepad_motion::lane::remove_key(&keys, at).ok_or("its only key")?;
        let doc = docs[site.file];
        page.motion.desk.author = Some("You · key".into());
        code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, text, u64::MAX / 4 + 3, true)]);
        return Ok(());
    }
    let v = makepad_motion::lane::value_at(&keys, session.playhead).or_else(|| keys.keys.first().and_then(|k| k.v)).ok_or("its keys are not numbers")?;
    let written = makepad_motion::edit::write(&files, path, &makepad_motion::edit::Value::Code(makepad_motion::patch::fmt_num(v)))?;
    let doc = docs[written.file];
    page.motion.desk.author = Some("You · animate".into());
    code_edits(page, cx, doc, vec![Edit::splice(written.range.start..written.range.end, written.literal, u64::MAX / 4 + 1, true)]);
    close_lane(page, path);
    Ok(())
}

/// Play from the playhead (from the start when it is at the end), or pause.
fn toggle_play(page: &mut VideoCut, cx: &mut Cx) {
    if let Some(session) = &mut page.motion.desk.session {
        session.playing = match session.playing {
            Some(_) => None,
            None => Some((Cx::time_now(), if session.playhead >= session.duration() - 1e-6 { 0.0 } else { session.playhead })),
        };
    }
    page.motion.desk.frame_next = cx.new_next_frame();
    page.motion.desk.repaint = true;
}

fn desk_key(page: &mut VideoCut, cx: &mut Cx, key: &KeyEvent) {
    let command = key.modifiers.logo || key.modifiers.control;
    match key.key_code {
        KeyCode::KeyZ if command => undo(page, key.modifiers.shift),
        KeyCode::Space => toggle_play(page, cx),
        KeyCode::ArrowLeft | KeyCode::ArrowRight => {
            let (t, fps) = page.motion.desk.session.as_ref().map_or((0.0, 60.0), |session| (session.playhead, session.fps()));
            let step = if key.modifiers.shift { 10.0 } else { 1.0 } / fps;
            set_playhead(page, if key.key_code == KeyCode::ArrowLeft { t - step } else { t + step });
        }
        KeyCode::Home => set_playhead(page, 0.0),
        // The bottom pane's other page: CODE ↔ CURVES.
        KeyCode::Tab if !command => page.motion.curves.toggle = true,
        KeyCode::Delete | KeyCode::Backspace => {
            if let Some(layer) = selected_layer(page) {
                page.motion.desk.author = Some("You · timeline".into());
                apply(page, MotionOp::Remove { layer });
            }
        }
        KeyCode::KeyD if command => {
            if let Some(layer) = selected_layer(page) {
                page.motion.desk.author = Some("You · timeline".into());
                apply(page, MotionOp::Duplicate { layer });
            }
        }
        // Escape lets go of a module selected in the panel first.
        KeyCode::Escape if makepad_value_pickers::panel::host::selected_module().is_some() => makepad_value_pickers::panel::host::clear_selection(),
        KeyCode::Escape => close(page),
        _ => {}
    }
}

/// The written value of a property now, as Splash text: its value at `t`
/// (the layer's clock; the playhead's when `None`) as the model evaluates
/// it, else as written.
fn current_text(page: &VideoCut, layer: &str, prop: &str, t: Option<f64>) -> Option<String> {
    let session = page.motion.desk.session.as_ref()?;
    let outline = session.outline.layer(layer)?;
    let written = outline.prop(prop);
    let like = written.and_then(|p| match &p.form {
        PropForm::Value(text) => Some(text.clone()),
        PropForm::Keys { keys, .. } => keys.first().map(|key| key.value.clone()),
        _ => None,
    });
    let local = t.unwrap_or(session.playhead - session.clock_of(layer));
    let model = session.model_layer(layer).and_then(|model| super::session::value_at(model, prop, local));
    match model {
        Some(values) if is_color(prop) && values.len() == 4 => Some(patch::fmt_color([values[0], values[1], values[2], values[3]])),
        Some(values) => Some(patch::fmt_values(&values, like.as_deref())),
        None => like,
    }
}

/// Whether a layer's property holds words (typed in a multi-line box,
/// live): what it is written as says so (`schema`), through an input or a
/// `let`; a keyed one by its keys' values.
fn is_words(session: &MotionSession, layer: &str, prop: &str) -> bool {
    let site = session.value_site(layer, prop).or_else(|| {
        session.field_sites(layer, prop).into_iter().find(|site| matches!(site.owner, Owner::Key { what: KeyPart::Value, .. }))
    });
    matches!(site.map(|site| &session.resolved(site).kind), Some(ValueKind::Text { .. }))
}

fn write(page: &mut VideoCut, layer: &str, prop: &str, value: String) {
    let Some(session) = &page.motion.desk.session else { return };
    let op = session.write_op(layer, prop, value);
    apply(page, op);
}

/// A step of a live gesture (typing, dragging) under `key`: held until the
/// next frame, so a burst of steps re-evaluates at most once a frame, and
/// the whole gesture is one undo step. `end` closes the gesture.
fn write_live(page: &mut VideoCut, key: String, op: MotionOp, end: bool) {
    page.motion.desk.live = Some((key, op, end));
    if end {
        flush_live(page);
    } else {
        page.motion.desk.live_wake = true;
    }
}

/// The held live step, if any, applied.
fn flush_live(page: &mut VideoCut) {
    let Some((key, op, end)) = page.motion.desk.live.take() else { return };
    let Some(session) = &mut page.motion.desk.session else { return };
    match session.apply_burst(&op, &key) {
        Ok(true) => changed(page),
        Ok(false) => {}
        Err(error) => page.motion.desk.status = format!("Refused: {error}"),
    }
    if end {
        if let Some(session) = &mut page.motion.desk.session {
            session.end_burst();
        }
    }
    page.motion.desk.repaint = true;
}

pub(super) fn inspector_action(page: &mut VideoCut, cx: &mut Cx, action: InspectorAction) {
    // A code value (the picked block's).
    let code_id = match &action {
        InspectorAction::Numbers { id, .. } | InspectorAction::NumbersLive { id, .. } | InspectorAction::Text { id, .. } | InspectorAction::TextLive { id, .. } | InspectorAction::Choice { id, .. } => id.strip_prefix("code:").and_then(|rest| {
            let (doc, literal) = rest.split_once(':')?;
            Some((doc.parse::<usize>().ok()?, literal.parse::<usize>().ok()?))
        }),
        _ => None,
    };
    if let Some((doc, literal)) = code_id {
        code_value_write(page, cx, doc, literal, &action);
        return;
    }
    // A site (`site:<index>`: an input, a document field, a record's or a
    // list's entry, any plain value): its bytes replaced. Words typed are
    // live, one undo step.
    let site_id = match &action {
        InspectorAction::Numbers { id, .. } | InspectorAction::Text { id, .. } | InspectorAction::TextLive { id, .. } | InspectorAction::Choice { id, .. } => id.strip_prefix("site:").and_then(|i| i.parse::<usize>().ok()),
        _ => None,
    };
    if let Some(index) = site_id {
        let Some(session) = &page.motion.desk.session else { return };
        let layer = session.sites.get(index).and_then(|site| site.layer().map(str::to_string));
        if layer.as_ref().is_some_and(|layer| session.locked.contains(layer) || session.outline.layer(layer).is_some_and(|l| l.locked)) {
            page.motion.desk.status = "This layer is locked: unlock it in the layer list to edit it".into();
            page.motion.desk.repaint = true;
            return;
        }
        match site_value(session, index, &action) {
            Some((range, text, what)) => {
                page.motion.desk.author = Some("You · inspector".into());
                let op = MotionOp::Splice { range, text, what };
                match action {
                    InspectorAction::TextLive { .. } => write_live(page, format!("site:{index}"), op, false),
                    InspectorAction::Text { .. } => write_live(page, format!("site:{index}"), op, true),
                    _ => apply(page, op),
                }
            }
            None => {
                page.motion.desk.status = "That is not a value this field takes".into();
                page.motion.desk.repaint = true;
            }
        }
        return;
    }
    // Words as they are typed, and their end: live, one undo step.
    if let InspectorAction::TextLive { id, text } | InspectorAction::Text { id, text } = &action {
        let end = matches!(action, InspectorAction::Text { .. });
        let layer = selected_layer(page);
        let op = page.motion.desk.session.as_ref().and_then(|session| {
            let layer = layer.as_deref()?;
            is_words(session, layer, id).then(|| session.write_op(layer, id, patch::fmt_text(text)))
        });
        if let Some(op) = op {
            let locked = layer.as_ref().is_some_and(|layer| page.motion.desk.session.as_ref().is_some_and(|session| session.locked.contains(layer) || session.outline.layer(layer).is_some_and(|l| l.locked)));
            if locked {
                page.motion.desk.status = "This layer is locked: unlock it in the layer list to edit it".into();
                page.motion.desk.repaint = true;
                return;
            }
            page.motion.desk.author = Some("You · inspector".into());
            write_live(page, format!("words:{}:{id}", layer.unwrap_or_default()), op, end);
            return;
        }
        if !end {
            return;
        }
    }
    let Some(layer) = selected_layer(page) else { return };
    let locked = page.motion.desk.session.as_ref().is_some_and(|session| session.locked.contains(&layer) || session.outline.layer(&layer).is_some_and(|l| l.locked));
    if locked && !matches!(action, InspectorAction::KeyNav(..)) {
        page.motion.desk.status = "This layer is locked: unlock it in the layer list to edit it".into();
        page.motion.desk.repaint = true;
        return;
    }
    page.motion.desk.author = Some("You · inspector".into());
    match action {
        InspectorAction::Numbers { id, values } => match id.as_str() {
            "span.start" | "span.length" => {
                let Some((start, length)) = page.motion.desk.session.as_ref().and_then(|session| session.span_of(&layer)) else { return };
                let (start, length) = if id == "span.start" { (values[0], length) } else { (start, values[0].max(0.01)) };
                apply(page, MotionOp::Span { layer, start, length, keep_keys: false });
            }
            _ => {
                let like = current_text(page, &layer, &id, None);
                write(page, &layer, &id, patch::fmt_values(&values, like.as_deref()));
            }
        },
        InspectorAction::Text { id, text } => match id.as_str() {
            "name" => apply(page, MotionOp::Field { layer, name: "name".into(), value: (!text.trim().is_empty()).then(|| patch::fmt_text(text.trim())) }),
            prop if is_color(prop) => {
                let text = text.trim();
                let hex = if text.starts_with('#') { text.to_string() } else { format!("#{text}") };
                match patch::literal_color(&hex) {
                    Some(_) => write(page, &layer, prop, hex),
                    None => page.motion.desk.status = format!("`{text}` is not a colour: write #rrggbb"),
                }
            }
            "svg" | "text" | "font" | "effect" => write(page, &layer, &id, patch::fmt_text(&text)),
            prop => write(page, &layer, prop, text),
        },
        InspectorAction::Choice { id, choice } => {
            let value = format!("@{choice}");
            apply(page, MotionOp::Value { layer, prop: id, value });
        }
        InspectorAction::KeyToggle(prop) => toggle_key(page, layer, prop),
        InspectorAction::KeyNav(prop, dir) => {
            let Some(session) = &page.motion.desk.session else { return };
            let clock = session.clock_of(&layer);
            let now = session.playhead;
            let times: Vec<f64> = session.outline.layer(&layer).and_then(|l| l.prop(&prop))
                .map(|p| p.form.keys().iter().filter_map(|key| key.t).map(|t| clock + t).collect())
                .unwrap_or_default();
            let target = if dir < 0 {
                times.iter().copied().filter(|t| *t < now - 1e-6).fold(None, |best: Option<f64>, t| Some(best.map_or(t, |b| b.max(t))))
            } else {
                times.iter().copied().filter(|t| *t > now + 1e-6).fold(None, |best: Option<f64>, t| Some(best.map_or(t, |b| b.min(t))))
            };
            if let Some(t) = target {
                set_playhead(page, t);
            }
        }
        InspectorAction::TextLive { .. } | InspectorAction::NumbersLive { .. } | InspectorAction::Pick { .. } | InspectorAction::None => {}
    }
}

/// A key at the playhead on a property: removed when there is one, else
/// added with the value there.
fn toggle_key(page: &mut VideoCut, layer: String, prop: String) {
    let Some(session) = &page.motion.desk.session else { return };
    let fps = session.fps();
    let t = session.playhead - session.clock_of(&layer);
    let at = session.outline.layer(&layer).and_then(|l| l.prop(&prop)).and_then(|p| p.form.keys().iter().position(|key| key.t.is_some_and(|kt| (kt - t).abs() < 0.5 / fps)));
    match at {
        Some(index) => apply(page, MotionOp::DeleteKeys(vec![(layer, prop, index)])),
        None => {
            if let Some(value) = current_text(page, &layer, &prop, None) {
                apply(page, MotionOp::Key { layer, prop, t: t.max(0.0), value });
            }
        }
    }
}

/// Key `index` of a layer's `at` moved to `at` (one undo step per drag).
fn path_key(page: &mut VideoCut, path: String, index: usize, at: [f64; 2], end: bool) {
    let Some(session) = &page.motion.desk.session else { return };
    let Some(key) = session.outline.layer(&path).and_then(|layer| layer.prop("at")).and_then(|prop| prop.form.keys().get(index).cloned()) else { return };
    let Some(t) = key.t else { return };
    page.motion.desk.author = Some("You · viewer".into());
    let op = MotionOp::Key { layer: path.clone(), prop: "at".into(), t, value: patch::fmt_values(&at, Some(&key.value)) };
    write_live(page, format!("path:{path}:{index}"), op, end);
}

/// What a click picks from: the frame at the playhead's drawn things (made
/// once per frame, when first asked).
fn ensure_picks(page: &mut VideoCut) {
    let desk = &mut page.motion.desk;
    let (Some(session), Some((of, t, frame, boxes))) = (&desk.session, &desk.picture) else { return };
    if desk.picks.as_ref().is_some_and(|(o, tt, _)| o == of && tt == t) {
        return;
    }
    let Some(doc) = session.doc.clone() else { return };
    let mut sites = std::mem::take(&mut desk.sites);
    let list = super::picking::pickables(session, &doc, frame, boxes, &mut sites);
    desk.sites = sites;
    desk.picks = Some((*of, *t, list));
}

/// The pointer over the picture: what it would pick (hover), or the pick
/// (click): its drawing hugged by an outline, its code shown and lit.
fn point_action(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, at: [f64; 2], click: bool, alt: bool, shift: bool) {
    ensure_picks(page);
    // What the renderer says is under the pointer (through warps, lenses
    // and pictures on 3D surfaces): asked for on the first click on a
    // picture, then known for every point of it.
    let names = picked_names(page, cx, desk_ref, at);
    if click && names.is_none() && page.motion.desk.pick_wait.is_none() {
        let fresh = match (&page.motion.desk.picture, &page.motion.desk.pick_image) {
            (Some((of, t, ..)), Some((pof, pt, _))) => of != pof || t != pt,
            _ => true,
        };
        if fresh {
            let viewer = desk_ref.widget(cx, ids!(m_viewer));
            let ticket = viewer.borrow::<MotionViewer>().and_then(|v| v.request_pick(cx));
            if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("motion.pick") {
                log!("motion.pick: asked the renderer ({:?})", ticket);
            }
            if let Some(ticket) = ticket {
                page.motion.desk.pick_wait = Some((ticket, at, alt, shift));
                cx.new_next_frame();
                return;
            }
        }
    }
    let Some(session) = &page.motion.desk.session else { return };
    let Some((_, _, list)) = &page.motion.desk.picks else { return };
    let scale = session.doc.as_ref().map_or(1.0, |doc| doc.size[0]) / desk_ref.widget(cx, ids!(m_viewer)).area().rect(cx).size.x.max(1.0);
    let size = session.doc.as_ref().map_or([1920.0, 1080.0], |doc| doc.size);
    let off_picture = at[0] < 0.0 || at[1] < 0.0 || at[0] > size[0] || at[1] > size[1];
    let ranked = match (&names, off_picture) {
        (_, true) => Vec::new(),
        (Some(names), false) => super::picking::pick_named(list, names, at, 4.0 * scale),
        (None, false) => super::picking::pick(list, at, 4.0 * scale),
    };
    let viewer = desk_ref.widget(cx, ids!(m_viewer));
    if !click {
        let quad = ranked.first().map(|&i| list[i].quad);
        if let Some(mut viewer) = viewer.borrow_mut::<MotionViewer>() {
            viewer.set_hover_pick(cx, quad);
        }
        return;
    }
    // The same spot again: one step out through the code that drew it
    // (the call, the function calling it, …), then the next thing down.
    let depth = match page.motion.desk.pick_cycle {
        Some((p, d)) if (p[0] - at[0]).abs() <= 3.0 * scale && (p[1] - at[1]).abs() <= 3.0 * scale && !alt => d + 1,
        _ => 0,
    };
    // Alt: the canvas or layer the thing under the pointer was drawn in.
    // A plain click picks what is drawn (a canvas or layer behind it is
    // empty space: the pick clears).
    let ranked: Vec<usize> = if alt {
        ranked.first().and_then(|&i| list[i].parent).into_iter().chain(ranked.iter().copied().filter(|&i| !list[i].content)).take(1).collect()
    } else {
        ranked.into_iter().filter(|&i| list[i].content).collect()
    };
    // Each distinct place in the code that drew it, innermost first.
    let steps: Vec<(usize, usize)> = ranked.iter().flat_map(|&i| {
        let code = &list[i].code;
        (0..code.len().max(1)).filter(move |&j| j == 0 || code.get(j) != code.get(j - 1)).map(move |j| (i, j))
    }).collect();
    let Some(&(index, frame)) = steps.get(depth % steps.len().max(1)) else {
        // Nothing there: the selection clears (shift-click on nothing
        // keeps it).
        if shift {
            return;
        }
        if let Some(mut viewer) = viewer.borrow_mut::<MotionViewer>() {
            viewer.set_highlight(cx, Vec::new(), String::new());
        }
        if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_trace(cx, String::new(), Vec::new(), 0, None);
        }
        if !page.motion.desk.selection.is_empty() {
            page.motion.desk.selection.clear();
            page.motion.desk.selection_rev += 1;
        }
        page.motion.desk.pick_outlined = false;
        return;
    };
    if makepad_widgets::makepad_platform::makepad_error_log::trace_enabled("motion.pick") {
        log!("motion.pick: at {:.0},{:.0} renderer {:?} -> {} {:?}", at[0], at[1], names, list[index].label, list[index].code.get(frame));
    }
    page.motion.desk.pick_cycle = Some((at, depth));
    page.motion.desk.pick_point = at;
    show_pick(page, cx, desk_ref, index, frame, shift);
}

/// The renderer's names for what was drawn under `at` (document units),
/// nearest first, from the pick image of the picture shown; `None` while
/// there is none for it.
fn picked_names(page: &VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, at: [f64; 2]) -> Option<Vec<(String, Option<usize>)>> {
    let desk = &page.motion.desk;
    let (of, t, ..) = desk.picture.as_ref()?;
    let (pof, pt, image) = desk.pick_image.as_ref()?;
    if pof != of || pt != t {
        return None;
    }
    let image = image.as_ref()?;
    let size = desk.session.as_ref()?.doc.as_ref()?.size;
    let (x, y) = (at[0] / size[0] * image.width as f64, at[1] / size[1] * image.height as f64);
    if x < 0.0 || y < 0.0 {
        return Some(Vec::new());
    }
    let ids = image.ids_near(x as usize, y as usize, 3);
    let viewer = desk_ref.widget(cx, ids!(m_viewer));
    let viewer = viewer.borrow::<MotionViewer>()?;
    Some(ids.into_iter().filter_map(|id| viewer.pick_name(id)).collect())
}

/// Where the clicked glyph of a picked text came from: the record its
/// word was written in (by what built it: `MotionDoc::text_origin`), as
/// the document it is in, the bytes of its words' literal, and a label
/// ("“see” ← song line 188").
fn text_origin(session: &MotionSession, pick: &super::picking::Pickable, file: &makepad_motion::schema::CodeFile, byte: usize, at: [f64; 2]) -> Option<(usize, std::ops::Range<usize>, String)> {
    let doc = session.doc.as_ref()?;
    let (code, text) = match file {
        makepad_motion::schema::CodeFile::Doc => (&session.code, session.source.as_str()),
        makepad_motion::schema::CodeFile::Module(key) => session.modules.iter().find(|m| m.key == *key).map(|m| (&m.code, m.source.as_str()))?,
    };
    // The glyph under the click (else the first), and its loops.
    let inside = |q: &[[f64; 2]; 4]| {
        let (x0, x1) = (q.iter().map(|p| p[0]).fold(f64::MAX, f64::min), q.iter().map(|p| p[0]).fold(f64::MIN, f64::max));
        let (y0, y1) = (q.iter().map(|p| p[1]).fold(f64::MAX, f64::min), q.iter().map(|p| p[1]).fold(f64::MIN, f64::max));
        at[0] >= x0 && at[0] <= x1 && at[1] >= y0 && at[1] <= y1
    };
    let loops = pick.glyphs.iter().find(|(q, _)| inside(q)).or(pick.glyphs.first()).map(|(_, l)| l.clone())?;
    // The lists its code reads through (top-level `let`s), and which loop
    // indexes the character (named in the call's text argument) and which
    // the row (named where the lists are indexed).
    let arg = makepad_motion::schema::code::call_argument(code, text, byte, 0)?;
    let call = code.region_at(byte)?;
    let chain = makepad_motion::schema::code::gather_from(code, text, call, &arg, 12);
    let lists: Vec<String> = chain.iter().filter(|g| code.regions.iter().any(|r| r.parent.is_none() && r.range == g.range)).map(|g| g.name.clone()).collect();
    let names_in = |t: &str, name: &str| t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| w == name);
    let arg_text = &text[arg.clone()];
    let ch = loops.iter().rev().find(|(id, _)| names_in(arg_text, &id.to_string()))?;
    let row = loops.iter().find(|(id, _)| *id != ch.0 && chain.iter().any(|g| names_in(&text[g.range.clone()], &id.to_string()))).map_or(0.0, |(_, v)| *v);
    let (origin, words, start) = doc.text_origin(&lists, row as usize, ch.1 as usize)?;
    // Its words' literal in the record written there (between its braces);
    // a record copied from another (`{text: w.w start: w.start}`): the
    // record written with those words at that time, in any open file (the
    // song's timing module first).
    let in_record = || -> Option<(makepad_motion::schema::CodeFile, std::ops::Range<usize>)> {
        let origin_text = super::picking::text_of(session, &origin.file)?;
        let at_byte = origin.byte_in(origin_text)?;
        let code = code_of_file(session, &origin.file)?;
        let end = origin_text[at_byte..].find('}').map_or(origin_text.len(), |n| at_byte + n);
        let l = code.literals.iter().find(|l| l.range.start >= at_byte && l.range.start < end && l.words().is_some_and(|w| w == words))?;
        Some((origin.file.clone(), l.range.clone()))
    };
    let (file, range) = in_record().or_else(|| record_by_time(session, &words, start?))?;
    let d = doc_index(session, &file)?;
    Some((d, range.clone(), format!("“{words}” ← {}", place_of(session, &file, range.start))))
}

/// A sung word: a text drawn from a song's word timing (records `{w:
/// "thread" start: … end: …}`, the song's timing module), one word per run of
/// glyphs. The word clicked (its glyphs share the iteration that drew them)
/// is found by its run of drawn words in the timing's words, in order (the
/// last drawn word may be partly drawn: its start); its `w` is what the box
/// edits, named with where it is written.
fn sung_word(session: &MotionSession, pick: &super::picking::Pickable, words: &str, at: [f64; 2], song_t: f64) -> Option<(usize, std::ops::Range<usize>, String)> {
    let tokens: Vec<&str> = words.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    // Which drawn word was clicked: the glyphs grouped by the iteration
    // that drew their word (their loops but the innermost), in order across.
    let inside = |q: &[[f64; 2]; 4]| {
        let (x0, x1) = (q.iter().map(|p| p[0]).fold(f64::MAX, f64::min), q.iter().map(|p| p[0]).fold(f64::MIN, f64::max));
        let (y0, y1) = (q.iter().map(|p| p[1]).fold(f64::MAX, f64::min), q.iter().map(|p| p[1]).fold(f64::MIN, f64::max));
        at[0] >= x0 && at[0] <= x1 && at[1] >= y0 && at[1] <= y1
    };
    let word_of = |loops: &Vec<(makepad_widgets::LiveId, f64)>| -> Vec<(makepad_widgets::LiveId, u64)> { loops.iter().take(loops.len().saturating_sub(1)).map(|(id, v)| (*id, v.to_bits())).collect() };
    let mut runs: Vec<(Vec<(makepad_widgets::LiveId, u64)>, f64)> = Vec::new();
    for (q, loops) in &pick.glyphs {
        let key = word_of(loops);
        let x = q.iter().map(|p| p[0]).fold(f64::MAX, f64::min);
        match runs.iter_mut().find(|(k, _)| *k == key) {
            Some(run) => run.1 = run.1.min(x),
            None => runs.push((key, x)),
        }
    }
    runs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let clicked = pick.glyphs.iter().find(|(q, _)| inside(q)).map(|(_, loops)| word_of(loops));
    let k = match clicked {
        Some(key) if runs.len() == tokens.len() => runs.iter().position(|(r, _)| *r == key).unwrap_or(0),
        // Else by place: the clicked glyph's rank across (a glyph drawn
        // twice, outline and fill, counted once) among the words' letters.
        _ => {
            let left = |q: &[[f64; 2]; 4]| q.iter().map(|p| p[0]).fold(f64::MAX, f64::min);
            let mut xs: Vec<f64> = pick.glyphs.iter().map(|(q, _)| left(q)).collect();
            xs.sort_by(|a, b| a.total_cmp(b));
            xs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
            let rank = pick.glyphs.iter().find(|(q, _)| inside(q)).map_or(0, |(q, _)| xs.iter().position(|x| (x - left(q)).abs() < 0.5).unwrap_or(0));
            let mut seen = 0;
            tokens.iter().position(|t| {
                seen += t.chars().count();
                rank < seen
            }).unwrap_or(tokens.len() - 1)
        }
    };
    // The timing's words, in order, in each file (modules first).
    let files = session.modules.iter().map(|m| (makepad_motion::schema::CodeFile::Module(m.key.clone()), &m.code)).chain(std::iter::once((makepad_motion::schema::CodeFile::Doc, &session.code)));
    for (file, code) in files {
        let sung: Vec<(&std::ops::Range<usize>, String)> = code.literals.iter().filter(|l| l.key.as_deref() == Some("w")).filter_map(|l| Some((&l.range, l.words()?.to_string()))).collect();
        let fits = |i: usize| tokens.iter().enumerate().all(|(j, t)| sung.get(i + j).is_some_and(|(_, w)| if j + 1 == tokens.len() { w.starts_with(t) } else { w == t }));
        // One glyph drawn alone (a lyric drawn letter by letter): the word
        // with that letter sung nearest the playhead (the song's time).
        let glyph = tokens.len() == 1 && tokens[0].chars().count() == 1;
        let text_here = super::picking::text_of(session, &file);
        let start_of = |r: &std::ops::Range<usize>| -> Option<f64> {
            let rest = &text_here?[r.end..];
            let rest = &rest[..rest.find('}').unwrap_or(rest.len())];
            let n = rest.split("start:").nth(1)?.trim_start();
            n[..n.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(n.len())].parse().ok()
        };
        let found = if glyph {
            (0..sung.len()).filter(|&i| sung[i].1.contains(tokens[0])).filter_map(|i| Some((i, (start_of(sung[i].0)? - song_t).abs()))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
        } else {
            (0..sung.len()).find(|&i| fits(i)).map(|i| i + k)
        };
        let Some(i) = found else { continue };
        let (range, _) = &sung[i];
        let d = doc_index(session, &file)?;
        // Its line: the words of the list it is written in.
        let text = super::picking::text_of(session, &file)?;
        let (a, b) = enclosing_list(text, range)?;
        let line: Vec<&String> = sung.iter().filter(|(r, _)| r.start > a && r.end < b).map(|(_, w)| w).collect();
        let n = sung.iter().filter(|(r, _)| r.start > a && r.end < b && r.start <= range.start).count();
        let words = line.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(" ");
        return Some((d, (*range).clone(), format!("sung word {n} of line “{words}” · song lyrics ({})", place_of(session, &file, range.start))));
    }
    None
}

/// The bytes of the list (`[` … `]`) the bytes `inner` are written in.
pub(super) fn enclosing_list(text: &str, inner: &std::ops::Range<usize>) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let mut depth = 0;
    let mut start = None;
    for i in (0..inner.start).rev() {
        match b[i] {
            b']' => depth += 1,
            b'[' if depth == 0 => {
                start = Some(i);
                break;
            }
            b'[' => depth -= 1,
            _ => {}
        }
    }
    let mut depth = 0;
    for i in inner.end..b.len() {
        match b[i] {
            b'[' => depth += 1,
            b']' if depth == 0 => return Some((start?, i)),
            b']' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// A drawn text's origin by its words: the string literal written with
/// exactly those words (a listing's raw string, a note's text, a table's
/// cell), or the one they begin (a line being typed on); the graphic's own
/// code first, then its modules. When the identity path finds no record
/// (the text was copied through lists of strings, `N_TEXT[ni]`).
fn text_by_words(session: &MotionSession, words: &str) -> Option<(usize, std::ops::Range<usize>, String)> {
    let words = words.trim();
    if words.is_empty() {
        return None;
    }
    let files = std::iter::once((makepad_motion::schema::CodeFile::Doc, &session.code, session.source.as_str())).chain(session.modules.iter().map(|m| (makepad_motion::schema::CodeFile::Module(m.key.clone()), &m.code, m.source.as_str())));
    let mut exact = None;
    let mut begun = Vec::new();
    for (file, code, text) in files {
        for l in &code.literals {
            let Some(w) = l.words() else { continue };
            // Words handed to a call while the file loads (`let EM =
            // fit_em("A red thread of", …)`: a measure) are not drawn.
            let at_load = code.regions.iter().find(|r| r.kind == makepad_motion::schema::code::RegionKind::Let && r.depth == 0 && r.range.start <= l.range.start && l.range.end <= r.range.end).is_some_and(|r| {
                let value = text[r.range.clone()].split_once('=').map_or("", |(_, v)| v).trim_start();
                let callee = value.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).map_or("", |n| &value[..n]);
                !callee.is_empty() && value[callee.len()..].starts_with('(')
            });
            if at_load {
                continue;
            }
            if w.trim() == words {
                exact = exact.or(Some((file.clone(), l.range.clone())));
            } else if words.chars().count() >= 4 && w.starts_with(words) {
                begun.push((file.clone(), l.range.clone()));
            }
        }
    }
    let (file, range) = exact.or_else(|| (begun.len() == 1).then(|| begun.remove(0)))?;
    let d = doc_index(session, &file)?;
    let short: String = words.chars().take(24).collect();
    Some((d, range.clone(), format!("“{short}” ← {}", place_of(session, &file, range.start))))
}

/// A file's code: the graphic's, or an open module's.
fn code_of_file<'a>(session: &'a MotionSession, file: &makepad_motion::schema::CodeFile) -> Option<&'a makepad_motion::schema::code::CodeOutline> {
    match file {
        makepad_motion::schema::CodeFile::Doc => Some(&session.code),
        makepad_motion::schema::CodeFile::Module(key) => session.modules.iter().find(|m| m.key == *key).map(|m| &m.code),
    }
}

/// The record written with `words` at time `start` (a `start:` within
/// 1.5 ms, its words field in the same braces), in any open file: modules
/// first (a song's timing), then the graphic.
fn record_by_time(session: &MotionSession, words: &str, start: f64) -> Option<(makepad_motion::schema::CodeFile, std::ops::Range<usize>)> {
    let files = session.modules.iter().map(|m| (makepad_motion::schema::CodeFile::Module(m.key.clone()), &m.code, m.source.as_str())).chain(std::iter::once((makepad_motion::schema::CodeFile::Doc, &session.code, session.source.as_str())));
    for (file, code, text) in files {
        for l in code.literals.iter().filter(|l| matches!(l.key.as_deref(), Some("start" | "t0" | "at")) && l.text.trim().parse::<f64>().is_ok_and(|v| (v - start).abs() < 0.0015)) {
            let open = text[..l.range.start].rfind('{').unwrap_or(0);
            let close = text[l.range.end..].find('}').map_or(text.len(), |n| l.range.end + n);
            if let Some(w) = code.literals.iter().find(|w| w.range.start > open && w.range.end <= close && w.words().is_some_and(|t| t.trim() == words.trim())) {
                return Some((file, w.range.clone()));
            }
        }
    }
    None
}

/// Where byte `byte` of `file` is, said for a person: its line, and the
/// helper function or module it is in when that is not the graphic's own
/// drawing.
fn place_of(session: &MotionSession, file: &makepad_motion::schema::CodeFile, byte: usize) -> String {
    use makepad_motion::schema::code::RegionKind;
    let line = super::picking::text_of(session, file).map_or(0, |text| text[..byte.min(text.len())].matches('\n').count()) + 1;
    let code = match file {
        makepad_motion::schema::CodeFile::Doc => &session.code,
        makepad_motion::schema::CodeFile::Module(key) => match session.modules.iter().find(|m| m.key == *key) {
            Some(m) => return format!("line {line} of module {}", m.name),
            None => return format!("line {line}"),
        },
    };
    // The outermost function holding it, and what holds that: a top-level
    // `let name = fn` is a helper; the graphic's own `draw: fn` is not.
    let mut r = code.region_at(byte);
    let mut function: Option<usize> = None;
    let mut top = None;
    while let Some(i) = r {
        if code.regions[i].kind == RegionKind::Fn {
            function = Some(i);
        }
        top = Some(i);
        r = code.regions[i].parent;
    }
    match (function, top) {
        (Some(f), Some(t)) if code.regions[t].kind == RegionKind::Let => {
            let name = if code.regions[f].name.is_empty() { code.regions[t].name.clone() } else { code.regions[f].name.clone() };
            format!("line {line} in helper {name}")
        }
        _ => format!("line {line}"),
    }
}

/// Show pick `index` of the picture's picks at `frame` of the code that
/// drew it (0: innermost, or the literal it shows): outlined with a label
/// saying what it is and where, its other places as chips, and its code
/// found, unfolded, centred and marked.
fn show_pick(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, index: usize, frame: usize, add: bool) {
    let Some(session) = &page.motion.desk.session else { return };
    let Some((_, _, list)) = &page.motion.desk.picks else { return };
    let Some(pick) = list.get(index).cloned() else { return };
    let viewer = desk_ref.widget(cx, ids!(m_viewer));
    let target = pick.code.get(frame).cloned();
    // Each distinct place in the code that drew it, innermost first.
    let frames: Vec<usize> = (0..pick.code.len()).filter(|&j| j == 0 || pick.code.get(j) != pick.code.get(j - 1)).collect();
    let code_of = |file: &makepad_motion::schema::CodeFile| -> Option<(&makepad_motion::schema::code::CodeOutline, &str)> {
        match file {
            makepad_motion::schema::CodeFile::Doc => Some((&session.code, session.source.as_str())),
            makepad_motion::schema::CodeFile::Module(key) => session.modules.iter().find(|m| m.key == *key).map(|m| (&m.code, m.source.as_str())),
        }
    };
    // The pick as a stack trace: each place in the code that drew it,
    // innermost first, with its call as written.
    let trace: Vec<super::code_timeline::TraceFrame> = frames.iter().filter_map(|&j| {
        let (file, byte) = pick.code.get(j)?;
        let d = doc_index(session, file)?;
        let (code, text) = code_of(file)?;
        let call = code.region_at(*byte).map(|r| text[code.regions[r].range.clone()].lines().next().unwrap_or("").trim().to_string()).unwrap_or_default();
        let call: String = if call.chars().count() > 60 { format!("{}…", call.chars().take(59).collect::<String>()) } else { call };
        Some(super::code_timeline::TraceFrame { doc: d, at: *byte, frame: j, place: place_of(session, file, *byte), call })
    }).collect();
    // A text's origin, by what built it: the glyph clicked, the loops it
    // was drawn in, the lists its code reads through; the record its word
    // was written in (the song's timing module). Else the literal written
    // with its words.
    let origin = match (&pick.words, pick.code.first()) {
        (Some(words), Some((file, byte))) => text_origin(session, &pick, file, *byte, page.motion.desk.pick_point).or_else(|| sung_word(session, &pick, words, page.motion.desk.pick_point, session.playhead + page.motion.desk.film.as_ref().map_or(0.0, |(_, film)| film.offset))).or_else(|| text_by_words(session, words)),
        _ => None,
    };
    let target = target.and_then(|(file, byte)| Some((doc_index(session, &file)?, byte)));
    if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
        code.set_trace(cx, pick.label.clone(), trace, frame, origin.clone());
    }
    // The selection: this thing (shift: added, or taken away when it is in
    // it already).
    let selected = Selected {
        code: pick.code.clone(),
        label: pick.label.clone(),
        quad: pick.quad,
        words: pick.words.clone(),
        paths: read_paths(session, &pick.code),
        origin: origin.as_ref().map(|(d, range, _)| (*d, range.clone())),
        origin_said: origin.as_ref().map(|(_, _, said)| said.clone()),
    };
    let selection = &mut page.motion.desk.selection;
    match selection.iter().position(|s| s.code == selected.code && s.label == selected.label) {
        Some(i) if add => {
            selection.remove(i);
        }
        _ if add => selection.push(selected),
        _ => *selection = vec![selected],
    }
    page.motion.desk.selection_rev += 1;
    // The structure's tree selects the node that drew it.
    super::structure_panel::reveal_pick(page, cx, desk_ref, &pick.code, &pick.path, &pick.loops);
    // On the picture: only a thin outline of each selected thing.
    let quads: Vec<[[f64; 2]; 4]> = page.motion.desk.selection.iter().map(|s| s.quad).collect();
    if let Some(mut viewer) = viewer.borrow_mut::<MotionViewer>() {
        viewer.set_highlight(cx, quads, String::new());
    }
    page.motion.desk.pick_outlined = true;
    let Some(session) = &page.motion.desk.session else { return };
    if let Some((doc, byte)) = target {
        let code = if doc == 0 { Some(&session.code) } else { session.modules.get(doc - 1).map(|m| &m.code) };
        // Words picked (a word, a line, drawn whole or a letter at a time):
        // the cursor goes into their string, the words selected, ready to
        // type over (in the plate, or in the module they come from).
        let words = (frame == 0 && pick.words.is_some()).then(|| code.and_then(|c| c.literal_at(byte)).and_then(|l| code.map(|c| c.literals[l].range.clone()))).flatten();
        page.motion.desk.code_selection = code.and_then(|code| code.region_at(byte)).map(|region| (doc, region));
        page.motion.desk.code_focus = Some(byte);
        page.motion.mode = ViewMode::Timeline;
        let code = desk_ref.widget(cx, ids!(m_code));
        if let Some(mut code) = code.borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            match words {
                Some(range) => code.reveal_words(cx, doc, range),
                None => code.reveal(cx, doc, byte),
            }
        };
    }
    page.motion.desk.repaint = true;
}

/// Values edited in document `doc` (the code view's, the inspector's
/// scrubs): a dragged number is set in the running film and
/// written at its release (`done`), as one step per gesture.
pub(super) fn code_edits(page: &mut VideoCut, cx: &mut Cx, doc: usize, edits: Vec<Edit>) {
    // Each splice in order; an edit's `done` is its last splice's.
    let steps: Vec<(usize, usize, String, u64, bool)> = edits
        .iter()
        .flat_map(|e| {
            let n = e.splices().count();
            e.splices().enumerate().map(move |(i, s)| (s.range.start, s.range.end, s.text.clone(), e.gesture, e.done && i + 1 == n))
        })
        .collect();
    for (start, end, text, gesture, done) in steps {
        let step = makepad_value_pickers::panel::host::timed("edit.live_number", || live_number(page, cx, doc, start, end, &text, gesture, done));
        let Some(session) = &mut page.motion.desk.session else { return };
        let key = format!("code:{doc}:{gesture}");
        page.motion.desk.author = Some("You · code".into());
        // The text as it goes: this splice, or (a held drag's
        // release) the whole of what it moved; held, nothing yet.
        let next = |source: &str| -> Result<String, String> {
            match &step {
                DragStep::Held => Ok(source.to_string()),
                DragStep::Write => makepad_motion::schema::splice_code(source, start..end, &text),
                DragStep::WriteAll(all) => Ok(all.clone()),
            }
        };
        let result = makepad_value_pickers::panel::host::timed("edit.replace", || if matches!(step, DragStep::Held) {
            Ok(false)
        } else if doc == 0 {
            next(&session.source).map(|next| session.replace_burst(next, "set value", &key))
        } else {
            match session.modules.get(doc - 1).map(|m| (m.key.clone(), m.source.clone())) {
                Some((module, source)) => next(&source).map(|next| session.replace_module(&module, next, "set value", Some(&key))),
                None => Ok(false),
            }
        });
        match result {
            Ok(true) => makepad_value_pickers::panel::host::timed("edit.changed", || changed(page)),
            Ok(false) => {}
            Err(error) => page.motion.desk.status = format!("Refused: {error}"),
        }
        if done {
            page.motion.desk.live_drag = None;
            if let Some(session) = &mut page.motion.desk.session {
                session.end_burst();
            }
            // The preview loaded is the text written: shown at once.
            if let DragStep::WriteAll(all) = step {
                super::value_pick::adopt(page, &[(doc, all)]);
            }
        }
    }
    page.motion.desk.repaint = true;
}

/// The code view's gestures: values edited (in the graphic or a module),
/// the playhead scrubbed, code selected (its drawing lit on the picture).
fn code_action(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, action: super::code_timeline::CodeTimelineAction) {
    use super::code_timeline::CodeTimelineAction;
    match action {
        CodeTimelineAction::Edits { doc, edits } => code_edits(page, cx, doc, edits),
        CodeTimelineAction::Scrub(t) => set_playhead(page, t),
        CodeTimelineAction::Key(key) => desk_key(page, cx, &key),
        CodeTimelineAction::PlayPause => toggle_play(page, cx),
        CodeTimelineAction::LaneClose(path) => close_lane(page, &path),
        CodeTimelineAction::Select(selected) => {
            page.motion.desk.code_selection = selected;
            page.motion.desk.code_focus = None;
            page.motion.desk.repaint = true;
            show_code_drawing(page, cx, desk_ref, selected);
        }
        CodeTimelineAction::None => {}
    }
}

/// Light up on the picture what code block `selected` (doc, region) draws.
fn show_code_drawing(page: &mut VideoCut, cx: &mut Cx, desk_ref: &WidgetRef, selected: Option<(usize, usize)>) {
    {
        {
            ensure_picks(page);
            let Some(session) = &page.motion.desk.session else { return };
            let mut quads = Vec::new();
            let mut label = String::new();
            if let (Some((doc, region)), Some((_, _, list))) = (selected, &page.motion.desk.picks) {
                let file = if doc == 0 { Some(makepad_motion::schema::CodeFile::Doc) } else { session.modules.get(doc - 1).map(|m| makepad_motion::schema::CodeFile::Module(m.key.clone())) };
                let code = if doc == 0 { Some(&session.code) } else { session.modules.get(doc - 1).map(|m| &m.code) };
                if let (Some(file), Some(code)) = (file, code) {
                    if let Some(r) = code.regions.get(region) {
                        label = r.name.clone();
                        let range = r.range.clone();
                        quads = list.iter().filter(|p| p.content && p.code.iter().any(|(f, b)| *f == file && range.contains(b))).map(|p| p.quad).collect();
                    }
                }
            }
            // Many strokes: their outline together.
            if quads.len() > 64 {
                let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
                for q in &quads {
                    for p in q {
                        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                    }
                }
                quads = vec![[[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]]];
            }
            if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<MotionViewer>() {
                viewer.set_highlight(cx, quads, label);
            }
        }
    }
}

fn viewer_action(page: &mut VideoCut, action: ViewerAction) {
    let end = matches!(action, ViewerAction::Move { .. } | ViewerAction::Text { .. });
    match action {
        ViewerAction::Select(layer) => {
            if layer.is_some() {
                page.motion.desk.code_selection = None;
            }
            if let Some(session) = &mut page.motion.desk.session {
                session.selection = LayerSelection { layer, ..Default::default() };
            }
            page.motion.desk.repaint = true;
        }
        ViewerAction::MoveLive { path, delta } | ViewerAction::Move { path, delta } => {
            page.motion.desk.author = Some("You · viewer".into());
            move_layer(page, &path, delta, end);
        }
        ViewerAction::TextLive { path, text } | ViewerAction::Text { path, text } => {
            page.motion.desk.author = Some("You · viewer".into());
            layer_words(page, path, text, end);
        }
        // A handle: scale, rotate, anchor, the text box's width; a key at
        // the playhead when the property is keyed, else its value.
        ViewerAction::TransformLive { path, prop, values } => lane_values(page, path, prop, values, false),
        ViewerAction::Transform { path, prop, values } => lane_values(page, path, prop, values, true),
        // A key of the motion path moved: that key's value.
        ViewerAction::PathKeyLive { path, index, at } => path_key(page, path, index, at, false),
        ViewerAction::PathKey { path, index, at } => path_key(page, path, index, at, true),
        ViewerAction::Point { .. } | ViewerAction::None => {}
    }
}

/// Move a layer in the frame: its position (`at`, where its origin is;
/// `x`/`y` are the offsets animations use) gets a key at the playhead when
/// it is animated, else its value moves.
/// `delta` is the drag's whole move so far: each live step writes the
/// position the drag began at plus it, and the drag is one undo step.
fn move_layer(page: &mut VideoCut, path: &str, delta: [f64; 2], end: bool) {
    let Some(session) = &page.motion.desk.session else { return };
    if let Some(placement) = session.placement(path) {
        page.motion.desk.status = format!("Not moved: {}", placement.hint);
        page.motion.desk.repaint = true;
        return;
    }
    let from = match page.motion.desk.drag_from.clone().filter(|(p, _, _)| p == path) {
        Some(from) => from,
        None => {
            let Some(outline) = session.outline.layer(path) else { return };
            let at = session.value_now(path, "at").or_else(|| outline.prop("at").and_then(|p| match &p.form {
                PropForm::Value(text) => patch::literal_numbers(text),
                _ => None,
            })).unwrap_or(vec![0.0, 0.0]);
            let like = current_text(page, path, "at", None);
            let from = (path.to_string(), at, like);
            page.motion.desk.drag_from = Some(from.clone());
            from
        }
    };
    let (_, at, like) = from;
    let Some(session) = &page.motion.desk.session else { return };
    let op = session.write_op(path, "at", patch::fmt_values(&[at[0] + delta[0], at.get(1).copied().unwrap_or(0.0) + delta[1]], like.as_deref()));
    if end {
        page.motion.desk.drag_from = None;
    }
    write_live(page, format!("move:{path}"), op, end);
}

/// The graph editor's gestures: keys and handles live (one undo step a
/// gesture, each step written from the source the gesture began with).
/// How a property's numbers show: × scale, in a unit.
fn units_of(prop: &str) -> (f64, &'static str) {
    match prop {
        "opacity" | "scale" | "trim_start" | "trim_end" | "p1" | "p2" | "p3" | "p4" => (100.0, "%"),
        "rotate" => (1.0, "°"),
        "at" | "x" | "y" | "size" | "radius" | "width" | "tracking" => (1.0, "px"),
        _ => (1.0, ""),
    }
}

/// Every view of the desk from the session.
fn paint(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let mode = page.motion.mode;
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    desk_ref.widget(cx, ids!(m_layers_box)).set_visible(cx, mode == ViewMode::Timeline);
    desk_ref.widget(cx, ids!(m_source_pane)).set_visible(cx, mode == ViewMode::Source);
    // The Source view follows the document.
    let Some((_, source)) = source_of(page) else { return };
    let revision = page.motion.desk.session.as_ref().map_or(0, |session| session.revision);
    if page.motion.desk.seen != Some(revision) {
        page.motion.desk.seen = Some(revision);
        let author = page.motion.desk.author.take().unwrap_or_else(|| "Edit".into());
        if let Some(pane) = &mut page.motion.desk.source {
            makepad_value_pickers::panel::host::timed("paint.source_sync", || pane.sync(cx, ui, &source, &author));
        }
        page.motion.desk.repaint = true;
    }
    // A typed draft that loads replaces the document.
    if let Some(proven) = page.motion.desk.source.as_mut().and_then(|pane| pane.take_proven()) {
        replace_source(page, proven, "You · source");
    }
    let p = makepad_look::palette::get(cx);
    if let Some(pane) = &mut page.motion.desk.source {
        pane.paint(cx, ui, &p);
    }
    if !std::mem::take(&mut page.motion.desk.repaint) {
        return;
    }
    // A hovered variant is drawn in place of the graphic.
    // So is a font or a colour being picked (`value_pick.rs`).
    let variant = super::variants::shown_doc(page).or_else(|| super::value_pick::shown_doc(page));
    let Some(session) = &page.motion.desk.session else { return };
    // The bar.
    ui.widget(cx, ids!(m_title)).set_text(cx, &session.title);
    let (dot, state) = if page.motion.desk.save_error.is_some() {
        (Some(p.danger), "Save failed".to_string())
    } else if page.motion.desk.publishing.is_some() || page.motion.desk.tree_write.is_some() || (session.tree_path.is_some() && page.saving) {
        (Some(p.amber), "Saving…".to_string())
    } else if session.dirty() {
        (Some(p.amber), "Edited · autosaves".to_string())
    } else if let Some(at) = &page.motion.desk.saved_at {
        (None, format!("Saved {at}"))
    } else if session.tree_path.is_some() && page.session.as_ref().is_some_and(|session| session.dirty()) {
        (Some(p.amber), "Tree draft".to_string())
    } else if session.asset.is_some() || session.tree_path.is_some() {
        (None, "Saved".to_string())
    } else {
        (Some(p.amber), "Not in the library yet".to_string())
    };
    ui.widget(cx, ids!(m_dot)).set_visible(cx, dot.is_some());
    if let Some(dot) = dot {
        let mut view = ui.widget(cx, ids!(m_dot));
        let color = Vec4f::from_u32(dot);
        script_apply_eval!(cx, view, {draw_bg +: {color: #(color)}});
    }
    // The state and the reason are in the dot's Tip, never in the bar.
    let tip = match &page.motion.desk.save_error {
        Some(error) => format!("{state}: {error}. It is tried again on the next change."),
        None if session.tree_path.is_some() => format!("{state} · autosaved with the project tree two seconds after the last change"),
        None => format!("{state} · autosaved to the library two seconds after the last change"),
    };
    let mut tip_view = ui.widget(cx, ids!(m_state_tip));
    script_apply_eval!(cx, tip_view, {text: #(tip)});
    // The way back names the edit.
    let back = page.session.as_ref().map(|session| session.doc.label.clone()).unwrap_or_else(|| "Edit".into());
    let back: String = if back.chars().count() > 24 { format!("{}…", back.chars().take(23).collect::<String>()) } else { back };
    ui.widget(cx, ids!(m_back)).set_text(cx, &back);
    let status = if !page.motion.desk.status.is_empty() {
        page.motion.desk.status.clone()
    } else if let Some(problem) = session.problems.first() {
        format!("Line {}:{} · {}", problem.line, problem.col, super::source_pane::human(&problem.message))
    } else if let Some(error) = &session.outline_error {
        format!("The source does not read: {error}")
    } else if let Some(warning) = session.warnings.first() {
        format!("Line {} · {}", warning.line, super::source_pane::human(&warning.message))
    } else {
        String::new()
    };
    ui.widget(cx, ids!(m_status)).set_text(cx, &status);
    // The viewer.
    // Paused, the frame says which code drew what (picking, "drawing
    // now"); playing, nothing extra is recorded.
    let playing = session.playing.is_some();
    let fresh = !playing && page.motion.desk.picture.as_ref().is_none_or(|(of, t, _, _)| *of != session.doc_of || *t != session.playhead);
    let frame = makepad_value_pickers::panel::host::timed("paint.eval", || variant.as_ref().map(|(_, doc)| doc).or(session.doc.as_ref()).map(|doc| {
        let mut frame = if playing { doc.eval(session.playhead) } else { doc.eval_traced(session.playhead) };
        super::session::without_hidden(&mut frame, &session.hidden);
        frame
    }));
    let items = makepad_value_pickers::panel::host::timed("paint.view_items", || frame.as_ref().map(|frame| view_items(session, frame)).unwrap_or_default());
    // What a click picks from, and which code draws now.
    if let (Some(frame), Some(doc), false, None) = (&frame, session.doc.clone(), playing, &variant) {
        if fresh {
            let boxes: Vec<(String, [[f64; 2]; 4])> = items.iter().map(|item| (item.path.clone(), item.quad)).collect();
            page.motion.desk.picture = Some((session.doc_of, session.playhead, frame.clone(), boxes.clone()));
            let mut sites = std::mem::take(&mut page.motion.desk.sites);
            page.motion.desk.activity_now = makepad_value_pickers::panel::host::timed("paint.picking_activity", || super::picking::activity(session, &doc, frame, &mut sites));
            page.motion.desk.sites = sites;
            // What its `let`s hold and what each piece of code drew are
            // read once the playhead rests (not while it is dragged).
            page.motion.desk.settle = Some((session.doc_of, session.playhead.to_bits(), Cx::time_now()));
        }
    }
    // A new frame: the pick's outline is not where its thing is now. It
    // goes until the frame rests, then follows the thing (`settle`).
    if (fresh || playing) && page.motion.desk.pick_outlined {
        page.motion.desk.pick_outlined = false;
        if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<MotionViewer>() {
            viewer.set_highlight(cx, Vec::new(), String::new());
        }
    }
    if playing && !page.motion.desk.activity_now.is_empty() {
        page.motion.desk.activity_now.clear();
        // Its `let`s keep their last read: the timeline reads a value that
        // moves from its samples at the playhead each frame.
    }
    // The code on its timeline.
    // The scrubber's filmstrip: the graphic as it is now, made once the
    // desk rests on a revision (not while it plays).
    let film_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (session.revision, session.doc_of, page.motion.desk.doc_in_film, &session.node, page.session.as_ref().map(|e| e.revision), session.duration().to_bits()).hash(&mut h);
        h.finish()
    };
    if page.motion.desk.film_of != Some(film_key) && session.playing.is_none() && session.doc_of == session.revision {
        page.motion.desk.film_of = Some(film_key);
        let source = film_source(page, film_key);
        if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_film(cx, source);
        }
    }
    // What the panel binds, by path, resolved again whenever the text or
    // the paths change; within a held gesture the view's anchors carry them.
    let held = page.motion.desk.live_drag.is_some();
    let asked = super::panel_time::asked();
    let bindings_key = (session.revision, asked.len());
    if !held && page.motion.desk.bindings_of != Some(bindings_key) {
        page.motion.desk.bindings_of = Some(bindings_key);
        let (bound, gone) = makepad_value_pickers::panel::host::timed("paint.bindings", || super::editor_panel::bindings(page));
        for why in &gone {
            log!("editor panel: {why}");
        }
        if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_bindings(cx, bound);
        }
    }
    // The code, what it measured drawing, and the loaded graphic (what its `let`s computed).
    let docs_key = (session.revision, page.motion.desk.activity_rev, session.doc_of);
    let code = desk_ref.widget(cx, ids!(m_code));
    if page.motion.desk.code_docs_of != Some(docs_key) {
        page.motion.desk.code_docs_of = Some(docs_key);
        let docs = makepad_value_pickers::panel::host::timed("paint.code_docs", || code_docs(&page.motion.desk, session));
        if let Some(mut code) = code.borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_docs(cx, docs);
        };
    } else {
        // Each frame: only the values at the playhead.
        let live = makepad_value_pickers::panel::host::timed("paint.code_live_now", || code_live_now(&page.motion.desk, session));
        if let Some(mut code) = code.borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_now(cx, live);
        };
    }
    if let Some(mut code) = code.borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
        code.set_time(cx, session.duration(), session.playhead, session.playing.is_some());
    };
    // The curve lanes, read from the code when it or the lanes changed.
    let open = lanes(page);
    let session = page.motion.desk.session.as_ref().unwrap();
    if page.motion.desk.lanes_of.as_ref() != Some(&(session.revision, open.clone())) {
        page.motion.desk.lanes_of = Some((session.revision, open.clone()));
        // Curves are edited on the CURVES page (curve_editor.rs), not as
        // lanes under the code's scrubber.
        let _ = &open;
        if let Some(mut code) = desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>() {
            code.set_lanes(cx, Vec::new());
        }
    }
    let Some(session) = page.motion.desk.session.as_ref() else { return };
    // The Source view shows the same values at its lines' ends.
    // (Paused: a value read again every frame of playback would lay the
    // text out every frame.)
    if page.motion.mode == ViewMode::Source && session.playing.is_none() {
        let text = session.source.as_str();
        let readouts: Vec<(usize, String)> = code_live_now(&page.motion.desk, session).into_iter().next().unwrap_or_default().into_iter().map(|(b, v)| (text[..b.min(text.len())].matches('\n').count(), v)).collect();
        if let Some(pane) = &mut page.motion.desk.source {
            pane.set_readouts(cx, ui, readouts);
        }
    }
    let size = session.doc.as_ref().map(|doc| doc.size).or(session.outline.size).unwrap_or([1920.0, 1080.0]);
    let background = session.doc.as_ref().and_then(|doc| doc.background);
    if let Some(mut viewer) = desk_ref.widget(cx, ids!(m_viewer)).borrow_mut::<MotionViewer>() {
        viewer.set_frame(cx, size, background, items, session.selection.layer.clone());
        // What the frame is, without hashing what it draws: the model, the
        // moment, what is hidden, the characters' art in.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            session.doc_of.hash(&mut h);
            session.revision.hash(&mut h);
            page.motion.desk.live_rev.hash(&mut h);
            session.playhead.to_bits().hash(&mut h);
            let mut hidden: Vec<&String> = session.hidden.iter().collect();
            hidden.sort();
            hidden.hash(&mut h);
            page.motion.desk.character_svgs.len().hash(&mut h);
            variant.as_ref().map(|(id, _)| *id).hash(&mut h);
            h.finish()
        };
        makepad_value_pickers::panel::host::timed("paint.viewer_frame", || viewer.set_motion_frame(cx, frame.clone(), key));
        // What the renderer could not draw (a shader, an effect) is said.
        if let (true, Some(error)) = (status.is_empty(), viewer.draw_errors().first()) {
            ui.widget(cx, ids!(m_status)).set_text(cx, &format!("Render: {error}"));
        }
        let note = if session.doc.is_none() && !session.problems.is_empty() { "The document does not load; the last good frame is not shown".to_string() } else { String::new() };
        viewer.set_note(cx, &note);
    }
    // The inspector: the picked code's values, else the layer's.
    let (title, subtitle, hue, sections) = match page.motion.desk.code_selection.and_then(|(doc, region)| code_sections(session, doc, region, page.motion.desk.code_focus)) {
        Some(code) => code,
        None => makepad_value_pickers::panel::host::timed("paint.inspector_sections", || inspector_sections(session)),
    };
    // INSPECTOR caps only for the document; a layer has its own header.
    desk_ref.widget(cx, ids!(m_inspector_head)).set_visible(cx, session.selection.layer.is_none());
    if let Some(mut inspector) = desk_ref.widget(cx, ids!(m_inspector)).borrow_mut::<MotionInspector>() {
        makepad_value_pickers::panel::host::timed("paint.inspector_set", || inspector.set(cx, &title, &subtitle, hue, sections));
    }
    page.motion.desk.status.clear();
}

/// The frame's layers as viewer boxes (document units).
/// One box per layer the frame draws: what a layer draws internally (a
/// kinetic text's letters, a Repeat's copies) is the layer's, never a box
/// of its own. The boxes here are the model's estimate; the viewer
/// measures what each layer draws.
fn view_items(session: &MotionSession, frame: &MotionFrame) -> Vec<ViewItem> {
    let mut out = Vec::new();
    let layers: Vec<String> = session.outline.all().iter().map(|layer| layer.path.clone()).collect();
    let mut seen = std::collections::HashSet::new();
    for (_, item) in frame.walk() {
        let Some(owner) = super::viewer::owner_of(&layers, &item.path) else { continue };
        if !seen.insert(owner.to_string()) {
            continue;
        }
        let path = owner.to_string();
        let outline = session.outline.layer(&path);
        let (size, text) = match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { text, size, line_height, tracking, weight, .. }) => {
                // An estimate of the laid-out box (heavier faces run wider).
                let lines: Vec<&str> = text.lines().collect();
                let longest = lines.iter().map(|line| line.chars().count()).max().unwrap_or(0) as f64;
                let advance = 0.52 + 0.12 * ((weight - 400.0) / 400.0).clamp(-0.5, 1.0);
                let w = longest * (size * advance + tracking);
                let h = lines.len().max(1) as f64 * size * line_height.max(1.0);
                ([w.max(*size), h], Some(text.clone()))
            }
            ItemKind::Primitive(PrimitiveFrame::Path { path, width, .. }) => (path_bounds(path, *width), None),
            _ => match item.box_size() {
                Some(size) if size[0] > 0.0 && size[1] > 0.0 => (size, None),
                _ => continue,
            },
        };
        // Text animated per unit keeps its offsets in its units: the box
        // follows the first.
        let unit_offset = match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) if units.len() > 1 || units.first().is_some_and(|unit| unit.offset != [0.0, 0.0]) => units.first().map_or([0.0, 0.0], |unit| unit.offset),
            _ => [0.0, 0.0],
        };
        let transform = makepad_motion::Affine::translate(unit_offset[0], unit_offset[1]).then(&item.transform(size));
        let quad = [[0.0, 0.0], [size[0], 0.0], [size[0], size[1]], [0.0, size[1]]].map(|p| transform.apply(p));
        let (ink, round, text_size) = match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Rect { fill, stroke, .. }) => (fill.or(*stroke), false, 0.0),
            ItemKind::Primitive(PrimitiveFrame::Ellipse { fill, stroke, .. }) => (fill.or(*stroke), true, 0.0),
            ItemKind::Primitive(PrimitiveFrame::Path { stroke, fill, .. }) => (stroke.or(*fill), false, 0.0),
            ItemKind::Primitive(PrimitiveFrame::Text { units, size, .. }) => (units.first().map(|unit| unit.color), false, *size),
            _ => (None, false, 0.0),
        };
        let opacity = match &item.kind {
            ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) => item.opacity * units.first().map_or(1.0, |unit| unit.opacity),
            _ => item.opacity,
        };
        let placed = session.selection.layer.as_deref().filter(|selected| *selected == path)
            .and_then(|_| session.placement(&path)).map(|placement| (placement.owner, placement.hint));
        out.push(ViewItem {
            quad,
            label: outline.map(|layer| session.label_of(layer)).unwrap_or_default(),
            // The words as written (an input's default when it reads one).
            text: match (&text, outline) {
                (Some(_), Some(layer)) => words_of(session, layer).or(text),
                _ => text,
            },
            locked: outline.is_some_and(|layer| layer.locked) || session.locked.contains(&path),
            placed,
            scale: session.value_now(&path, "scale").filter(|v| v.len() == 2).map_or([1.0, 1.0], |v| [v[0], v[1]]),
            rotate: session.value_now(&path, "rotate").and_then(|v| v.first().copied()).unwrap_or(0.0),
            anchor: session.model_layer(&path).map_or([0.5, 0.5], |model| model.transform.anchor),
            wrap: session.value_site(&path, "wrap").and_then(|site| session.resolved(site).numbers()).and_then(|v| v.first().copied()),
            motion: if session.selection.layer.as_deref() == Some(path.as_str()) { motion_path(session, &path) } else { Vec::new() },
            keys: if session.selection.layer.as_deref() == Some(path.as_str()) { motion_keys(session, &path) } else { Vec::new() },
            container: matches!(item.kind, ItemKind::Draw2d { .. } | ItemKind::Shader { .. } | ItemKind::Group { .. } | ItemKind::Scene3D { .. } | ItemKind::Scene3dDoc { .. }) || {
                let doc_size = session.doc.as_ref().map_or([1920.0, 1080.0], |doc| doc.size);
                let (w, h) = ((quad[1][0] - quad[0][0]).hypot(quad[1][1] - quad[0][1]), (quad[3][0] - quad[0][0]).hypot(quad[3][1] - quad[0][1]));
                w * h > doc_size[0] * doc_size[1] * 0.6
            },
            path,
            ink,
            round,
            text_size,
            opacity,
        });
    }
    out
}

/// Where a layer's origin (`at`) goes over its span, in document units
/// (sampled from the model), when it moves.
fn motion_path(session: &MotionSession, path: &str) -> Vec<[f64; 2]> {
    let Some(model) = session.model_layer(path) else { return Vec::new() };
    if !matches!(model.transform.at, makepad_motion::Prop::Track(_)) {
        return Vec::new();
    }
    let length = session.span_of(path).map_or(session.duration(), |(_, length)| length);
    let n = 96;
    (0..=n).filter_map(|i| {
        let v = super::session::value_at(model, "at", length * i as f64 / n as f64)?;
        (v.len() == 2).then(|| [v[0], v[1]])
    }).collect()
}

/// A layer's `at` keys where they are: `(key index, point)`.
fn motion_keys(session: &MotionSession, path: &str) -> Vec<(usize, [f64; 2])> {
    let Some(prop) = session.outline.layer(path).and_then(|layer| layer.prop("at")) else { return Vec::new() };
    prop.form.keys().iter().enumerate().filter_map(|(index, key)| {
        let v = patch::literal_numbers(&key.value)?;
        (v.len() == 2).then(|| (index, [v[0], v[1]]))
    }).collect()
}

/// A layer's words as written: its `text` (an input's default or a `let`'s
/// value when it reads one), unescaped.
fn words_of(session: &MotionSession, layer: &makepad_motion::patch::LayerOutline) -> Option<String> {
    session.value_site(&layer.path, "text").and_then(|site| session.resolved(site).words())
}

/// A path's box from (0, 0) (the model's: the path's bounds from its own
/// origin), and its stroke.
fn path_bounds(path: &makepad_motion::geometry::VectorPath, width: f64) -> [f64; 2] {
    let bounds = makepad_motion::geometry::bounds(path).unwrap_or([0.0, 0.0, 1.0, 1.0]);
    [bounds[2].max(1.0) + width, bounds[3].max(1.0) + width]
}

/// The inspector for the selection: the layer's sections. Driven by the
/// document, not by a panel per layer
/// type: every field the layer's component has is a row, its editor chosen
/// by what the value is (`makepad_motion::schema`): written fields in
/// declaration order (the transform's first), a record's or a list's
/// entries each a row of their own, then the fields not set yet, typed as
/// Splash.
fn inspector_sections(session: &MotionSession) -> (String, String, Option<u32>, Vec<Section>) {
    let Some(path) = session.selection.layer.clone() else {
        let subtitle = format!("Motion · {}×{} · {} layers", patch::fmt_num(session.outline.size.map_or(0.0, |s| s[0])), patch::fmt_num(session.outline.size.map_or(0.0, |s| s[1])), session.outline.all().len());
        return (session.title.clone(), subtitle, None, document_sections(session));
    };
    let Some(layer) = session.outline.layer(&path).cloned() else {
        return (String::new(), String::new(), None, Vec::new());
    };
    let fps = session.fps();
    let mut sections = Vec::new();
    let mut layer_fields = vec![Field::text("name", "Name", layer.name.clone().unwrap_or_default())];
    if let Some((start, length)) = session.span_of(&path) {
        layer_fields.push(Field { kind: FieldKind::Time, ..Field::number("span.start", "Starts", vec![start], 1.0 / fps, 1.0, "s") });
        layer_fields.push(Field { kind: FieldKind::Time, ..Field::number("span.length", "Length", vec![length], 1.0 / fps, 1.0, "s") });
    }
    sections.push(Section { title: "Layer".into(), fields: layer_fields });
    // Rows the layer list and the bar above own.
    let owned = |name: &str| matches!(name, "name" | "span" | "layers" | "mask" | "layer" | "visible" | "locked");
    let declared = makepad_motion::layers::fields(&layer.ty);
    let written = |name: &str| layer.prop(name).is_some();
    let mut transform = Vec::new();
    let mut own = Vec::new();
    let mut unset = Vec::new();
    for name in declared.iter().filter(|name| !owned(name)) {
        let common = makepad_motion::layers::is_common(name);
        if !written(name) && !(common && matches!(name.as_str(), "at" | "opacity")) {
            unset.push(Field::text(name, &label_of_field(name), String::new()));
            continue;
        }
        let rows = field_rows(session, &layer, name);
        if common { transform.extend(rows) } else { own.extend(rows) }
    }
    // Fields a document writes that the component does not declare (a
    // derived type's own): rows too, so nothing written is hidden.
    for prop in &layer.props {
        if !owned(&prop.name) && !declared.iter().any(|name| *name == prop.name) {
            own.extend(field_rows(session, &layer, &prop.name));
        }
    }
    // A layer a layout places: its position is the layout's, shown, not
    // edited here.
    if let Some(placement) = session.placement(&path) {
        for field in transform.iter_mut().filter(|field| matches!(field.id.as_str(), "at" | "x" | "y")) {
            let shown = super::session::show(&field.id, &field.values);
            field.text = format!("{shown} · {}", placement.hint);
            field.kind = FieldKind::Fixed;
            field.keyable = false;
        }
    }
    sections.push(Section { title: "Transform".into(), fields: transform });
    if !own.is_empty() {
        sections.push(Section { title: layer.ty.clone(), fields: own });
    }
    if !unset.is_empty() {
        sections.push(Section { title: "Not set".into(), fields: unset });
    }
    // The kind line (§3.5): its type, and the group it is in.
    let parent = parent_path(&path).strip_suffix(".layers")
        .and_then(|group| session.outline.layer(group)).map(|group| session.label_of(group));
    let mut subtitle = match parent {
        Some(group) => format!("{} · in {group}", layer.ty),
        None => layer.ty.clone(),
    };
    if layer.locked || session.locked.contains(&path) {
        subtitle.push_str(" · locked");
    }
    (session.label_of(&layer), subtitle, Some(layer_hue(&layer.ty)), sections)
}

/// The inspector for a code block: its values, each by what it is (words,
/// numbers, a colour, a name), labelled as the code writes them.
fn code_sections(session: &MotionSession, doc: usize, region: usize, focus: Option<usize>) -> Option<(String, String, Option<u32>, Vec<Section>)> {
    let (code, file_name) = if doc == 0 { (&session.code, session.title.clone()) } else { let m = session.modules.get(doc - 1)?; (&m.code, m.name.clone()) };
    let r = code.regions.get(region)?;
    let mut fields = Vec::new();
    // The picked value first.
    let mut order = code.literals_in(region);
    if let Some(at) = focus {
        if let Some(pos) = order.iter().position(|&i| code.literals[i].range.start <= at && at <= code.literals[i].range.end) {
            let picked = order.remove(pos);
            order.insert(0, picked);
        }
    }
    for index in order.into_iter().take(120) {
        let literal = &code.literals[index];
        let inner = literal.region.filter(|&x| x != region).map(|x| code.regions[x].name.clone());
        let label = match (inner, literal.label.is_empty()) {
            (Some(inner), false) => format!("{inner} · {}", literal.label),
            (Some(inner), true) => inner,
            (None, _) => literal.label.clone(),
        };
        fields.push(literal_field(literal, doc, index, label));
    }
    let kind = match r.kind {
        makepad_motion::schema::code::RegionKind::Let => "let",
        makepad_motion::schema::code::RegionKind::Fn => "function",
        makepad_motion::schema::code::RegionKind::Object => "object",
        makepad_motion::schema::code::RegionKind::Call => "call",
    };
    let subtitle = format!("{kind} · {file_name} · {} values", fields.len());
    Some((r.name.clone(), subtitle, Some(0x4ec9b0ff), vec![Section { title: "Values".into(), fields }]))
}

/// A code value as an inspector field (`code:<doc>:<literal>`: written back
/// by [`code_value_write`]), its control by its kind.
pub(super) fn literal_field(literal: &makepad_motion::schema::code::Literal, doc: usize, index: usize, label: String) -> Field {
let id = format!("code:{doc}:{index}");
    let numbers = patch::literal_numbers(&literal.text).unwrap_or_default();
    let kind = match &literal.kind {
        ValueKind::Number { .. } => {
            let decimals = literal.text.split('.').nth(1).map_or(0, |d| d.len());
            FieldKind::Number { step: 10f64.powi(-(decimals as i32)), scale: 1.0, unit: "" }
        }
        ValueKind::Color => FieldKind::Color,
        ValueKind::Text { .. } => FieldKind::Words,
        ValueKind::Font => FieldKind::Font,
        ValueKind::Bool => FieldKind::Choice(vec!["false".into(), "true".into()]),
        ValueKind::Enum { choices } if choices.len() > 1 => FieldKind::Choice(choices.clone()),
        _ => FieldKind::Text,
    };
    let mut field = Field { id, label, kind: kind.clone(), values: Vec::new(), text: String::new(), animated: false, on_key: false, prev_key: false, next_key: false, keyable: false };
    match kind {
        FieldKind::Number { .. } => field.values = numbers,
        FieldKind::Color => {
            let rgba = patch::literal_color(&literal.text).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            field.values = rgba.to_vec();
            field.text = patch::fmt_color(rgba);
        }
        FieldKind::Words => field.text = literal.words().unwrap_or_default(),
        FieldKind::Choice(_) | FieldKind::Font => field.text = literal.words().unwrap_or_else(|| literal.text.trim_start_matches('@').to_string()),
        _ => field.text = literal.text.trim_start_matches('@').to_string(),
    }
    field
}

/// An inspector edit of a code value (`code:<doc>:<literal>`): its bytes
/// replaced in the graphic or its module, live while typed.
fn code_value_write(page: &mut VideoCut, cx: &mut Cx, doc: usize, literal: usize, action: &InspectorAction) {
    let Some(session) = &mut page.motion.desk.session else { return };
    let code = if doc == 0 { &session.code } else { match session.modules.get(doc - 1) { Some(m) => &m.code, None => return } };
    let Some(lit) = code.literals.get(literal).cloned() else { return };
    // A scrubbed number: set in the running film as it moves, written at
    // the release (the code view's drag path).
    if let InspectorAction::NumbersLive { values, .. } | InspectorAction::Numbers { values, .. } = action {
        let live = matches!(action, InspectorAction::NumbersLive { .. });
        let drag = page.motion.desk.insp_drag.take().filter(|d| d.0 == doc && d.1 == literal);
        if live || drag.is_some() {
            let (gesture, range) = match drag {
                Some((_, _, gesture, range)) => (gesture, range),
                None => {
                    page.motion.desk.insp_gesture += 1;
                    (u64::MAX - page.motion.desk.insp_gesture, lit.range.clone())
                }
            };
            let text = patch::fmt_values(values, Some(&lit.text));
            let now = range.start..range.start + text.len();
            if live {
                page.motion.desk.insp_drag = Some((doc, literal, gesture, now));
            }
            page.motion.desk.author = Some("You · inspector".into());
            // A value of a top-level `let` (a named record, a constant) is
            // read once, as the film loads: setting it in the running film
            // shows nothing, so each step is written (and the film loads
            // again behind it), one undo step with the release.
            let in_let = code.regions.iter().any(|r| r.kind == makepad_motion::schema::code::RegionKind::Let && r.depth == 0 && r.range.start <= lit.range.start && lit.range.end <= r.range.end);
            if in_let {
                let key = format!("insp-drag:{doc}:{gesture}");
                let result = if doc == 0 {
                    makepad_motion::schema::splice_code(&session.source, range.clone(), &text).map(|next| session.replace_burst(next, "set value", &key))
                } else {
                    let (module, source) = (session.modules[doc - 1].key.clone(), session.modules[doc - 1].source.clone());
                    makepad_motion::schema::splice_code(&source, range.clone(), &text).map(|next| session.replace_module(&module, next, "set value", Some(&key)))
                };
                if !live {
                    session.end_burst();
                }
                match result {
                    Ok(true) => changed(page),
                    Ok(false) => {}
                    Err(error) => page.motion.desk.status = format!("Refused: {error}"),
                }
                page.motion.desk.repaint = true;
                return;
            }
            code_edits(page, cx, doc, vec![Edit::splice(range.start..range.end, text, gesture, !live)]);
            return;
        }
    }
    let text = match action {
        InspectorAction::Numbers { values, .. } => patch::fmt_values(values, Some(&lit.text)),
        InspectorAction::Text { text, .. } | InspectorAction::TextLive { text, .. } => match &lit.kind {
            ValueKind::Color => {
                let t = text.trim();
                if t.starts_with('#') { t.to_string() } else { format!("#{t}") }
            }
            ValueKind::Text { .. } | ValueKind::Font => patch::fmt_text(text),
            ValueKind::Enum { .. } | ValueKind::Ease => format!("@{}", text.trim().trim_start_matches('@')),
            _ => text.trim().to_string(),
        },
        InspectorAction::Choice { choice, .. } => match &lit.kind {
            ValueKind::Font => patch::fmt_text(choice),
            ValueKind::Bool => choice.clone(),
            _ => format!("@{choice}"),
        },
        _ => return,
    };
    let key = format!("insp-code:{doc}:{literal}");
    let end = !matches!(action, InspectorAction::TextLive { .. });
    page.motion.desk.author = Some("You · inspector".into());
    let result = if doc == 0 {
        makepad_motion::schema::splice_code(&session.source, lit.range.clone(), &text).map(|next| session.replace_burst(next, "set value", &key))
    } else {
        let (module, source) = (session.modules[doc - 1].key.clone(), session.modules[doc - 1].source.clone());
        makepad_motion::schema::splice_code(&source, lit.range.clone(), &text).map(|next| session.replace_module(&module, next, "set value", Some(&key)))
    };
    match result {
        Ok(true) => changed(page),
        Ok(false) => {}
        Err(error) => page.motion.desk.status = format!("Refused: {error}"),
    }
    if end {
        if let Some(session) = &mut page.motion.desk.session {
            session.end_burst();
        }
    }
    page.motion.desk.repaint = true;
}

/// `font_size` → "Font size".
fn label_of_field(name: &str) -> String {
    let label = name.replace(['_', '.'], " ");
    let mut chars = label.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// The inspector's editor for a value of `kind` written under `name`.
fn field_kind(kind: &ValueKind, name: &str, numbers: &[f64]) -> FieldKind {
    match kind {
        ValueKind::Number { .. } => {
            let (scale, unit) = units_of(name);
            let step = if scale > 1.0 { 0.01 } else if unit == "°" { 0.5 } else if numbers.iter().all(|v| v.abs() <= 4.0) && unit.is_empty() { 0.01 } else { 1.0 };
            FieldKind::Number { step, scale, unit }
        }
        ValueKind::Color => FieldKind::Color,
        ValueKind::Text { multiline: true } => FieldKind::Words,
        ValueKind::Font => FieldKind::Font,
        ValueKind::Text { .. } | ValueKind::Media | ValueKind::Ease => FieldKind::Text,
        // A name with no known alternatives yet: typed.
        ValueKind::Enum { choices } if choices.len() < 2 => FieldKind::Text,
        ValueKind::Enum { choices } => FieldKind::Choice(choices.clone()),
        ValueKind::Bool => FieldKind::Choice(vec!["false".into(), "true".into()]),
        ValueKind::Time => FieldKind::Time,
        ValueKind::Data | ValueKind::Input(_) | ValueKind::Signal | ValueKind::Code => FieldKind::Fixed,
    }
}

/// A site as an inspector field (`site:<index>`), its value as written.
fn site_field(session: &MotionSession, index: usize, label: &str) -> Field {
    let site = &session.sites[index];
    let shown = session.resolved(site);
    let numbers = shown.numbers().unwrap_or_default();
    let kind = field_kind(&shown.kind, site.field().rsplit('.').next().unwrap_or(""), &numbers);
    let mut field = Field { id: format!("site:{index}"), label: label.to_string(), kind: kind.clone(), values: Vec::new(), text: String::new(), animated: false, on_key: false, prev_key: false, next_key: false, keyable: false };
    match kind {
        FieldKind::Number { .. } | FieldKind::Time => field.values = numbers,
        FieldKind::Color => {
            let rgba = shown.color().unwrap_or([0.0, 0.0, 0.0, 1.0]);
            field.values = rgba.to_vec();
            field.text = patch::fmt_color(rgba);
        }
        FieldKind::Text | FieldKind::Words => field.text = shown.words().unwrap_or_else(|| shown.text.trim_start_matches('@').to_string()),
        FieldKind::Choice(_) | FieldKind::Font => field.text = shown.words().unwrap_or_else(|| shown.text.trim_start_matches('@').to_string()),
        FieldKind::Fixed => field.text = shown.text.lines().next().unwrap_or("").to_string(),
    }
    field
}

/// The rows of one written field: one for a plain or keyed value (keyed:
/// the value at the playhead, a key there when edited), one per entry of a
/// record or list, the text of anything else (code) shown.
fn field_rows(session: &MotionSession, layer: &makepad_motion::patch::LayerOutline, name: &str) -> Vec<Field> {
    let path = &layer.path;
    let label = label_of_field(name);
    let index_of = |site: &Site| session.sites.iter().position(|s| s.range == site.range);
    // A plain value (or an input's default, or a `let`'s value).
    if let Some(site) = session.value_site(path, name) {
        if !matches!(layer.prop(name).map(|p| &p.form), Some(PropForm::Other(_))) || site.kind != ValueKind::Code {
            if let Some(index) = index_of(site) {
                let mut field = site_field(session, index, &label);
                // Numbers and colours can take keys (the diamond).
                field.keyable = matches!(field.kind, FieldKind::Number { .. } | FieldKind::Color) && !matches!(site.kind, ValueKind::Input(_));
                if field.keyable {
                    field.id = name.to_string();
                }
                return vec![field];
            }
        }
    }
    let keys = session.field_sites(path, name);
    let form = layer.prop(name).map(|p| &p.form);
    match form {
        // Keyed: the value at the playhead; editing writes a key there.
        Some(PropForm::Keys { keys: written, .. }) => {
            let local = session.playhead - session.clock_of(path);
            let fps = session.fps();
            let value_site = keys.iter().find(|s| matches!(s.owner, Owner::Key { what: KeyPart::Value, .. }));
            let shown_kind = value_site.map(|s| session.resolved(s).kind.clone()).unwrap_or(ValueKind::Code);
            let now = session.value_now(path, name);
            let first = value_site.map(|s| session.resolved(s));
            let numbers = now.clone().or_else(|| first.and_then(|s| s.numbers())).unwrap_or_default();
            let kind = field_kind(&shown_kind, name, &numbers);
            let mut field = Field { id: name.to_string(), label, kind: kind.clone(), values: Vec::new(), text: String::new(), animated: true, on_key: false, prev_key: false, next_key: false, keyable: true };
            field.on_key = written.iter().any(|key| key.t.is_some_and(|t| (t - local).abs() < 0.5 / fps));
            field.prev_key = written.iter().any(|key| key.t.is_some_and(|t| t < local - 1e-6));
            field.next_key = written.iter().any(|key| key.t.is_some_and(|t| t > local + 1e-6));
            match kind {
                FieldKind::Number { .. } => field.values = numbers,
                FieldKind::Color => {
                    let rgba = numbers.get(..4).map(|v| [v[0], v[1], v[2], v[3]]).or_else(|| first.and_then(|s| s.color())).unwrap_or([0.0, 0.0, 0.0, 1.0]);
                    field.values = rgba.to_vec();
                    field.text = patch::fmt_color(rgba);
                }
                FieldKind::Text | FieldKind::Words => field.text = first.and_then(|s| s.words()).unwrap_or_default(),
                FieldKind::Choice(_) | FieldKind::Font => field.text = first.map(|s| s.text.trim_start_matches('@').to_string()).unwrap_or_default(),
                _ => {
                    field.kind = FieldKind::Fixed;
                    field.keyable = false;
                    field.text = written.first().map(|k| k.value.clone()).unwrap_or_default();
                }
            }
            vec![field]
        }
        // A record or a list: a row per entry.
        _ if !keys.is_empty() && keys.iter().all(|s| matches!(&s.owner, Owner::Field { part, field, .. } if !part.is_empty() || field != name)) => {
            keys.iter().filter_map(|site| {
                let index = index_of(site)?;
                let sub = match &site.owner {
                    Owner::Field { field, part, .. } => {
                        let field = field.strip_prefix(name).unwrap_or(field).trim_start_matches('.');
                        [name, field, part.as_str()].iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(".")
                    }
                    _ => name.to_string(),
                };
                Some(site_field(session, index, &label_of_field(&sub)))
            }).collect()
        }
        // Code: shown as written.
        _ => {
            let text = layer.prop(name).map(|p| session.source.get(p.range.clone()).unwrap_or("").to_string()).unwrap_or_default();
            vec![Field { id: name.to_string(), label, kind: FieldKind::Fixed, values: Vec::new(), text: text.lines().next().unwrap_or("").to_string(), animated: matches!(form, Some(f) if f.is_animated()), on_key: false, prev_key: false, next_key: false, keyable: false }]
        }
    }
}

/// With nothing selected: the document's inputs (edited as their defaults)
/// and its own fields (duration, size, background, …), each by its kind.
fn document_sections(session: &MotionSession) -> Vec<Section> {
    let mut inputs = Vec::new();
    let mut document = Vec::new();
    for (index, site) in session.sites.iter().enumerate() {
        match &site.owner {
            Owner::Input { name } => inputs.push(site_field(session, index, name)),
            Owner::Doc { field } => document.push(site_field(session, index, &label_of_field(field))),
            _ => {}
        }
    }
    let mut sections = Vec::new();
    if !inputs.is_empty() {
        sections.push(Section { title: "Inputs".into(), fields: inputs });
    }
    if !document.is_empty() {
        sections.push(Section { title: "Document".into(), fields: document });
    }
    sections
}

/// The Splash text an inspector edit of site `index` writes.
fn site_value(session: &MotionSession, index: usize, action: &InspectorAction) -> Option<(std::ops::Range<usize>, String, String)> {
    let site = session.resolved(session.sites.get(index)?);
    let text = match action {
        InspectorAction::Numbers { values, .. } => patch::fmt_values(values, Some(&site.text)),
        InspectorAction::Text { text, .. } | InspectorAction::TextLive { text, .. } => match &site.kind {
            ValueKind::Color => {
                let text = text.trim();
                let hex = if text.starts_with('#') { text.to_string() } else { format!("#{text}") };
                patch::literal_color(&hex)?;
                hex
            }
            ValueKind::Text { .. } | ValueKind::Font | ValueKind::Media => patch::fmt_text(text),
            ValueKind::Enum { .. } => format!("@{}", text.trim().trim_start_matches('@')),
            // A name, an ease or anything else: as typed (Splash).
            _ => text.trim().to_string(),
        },
        InspectorAction::Choice { choice, .. } if site.kind == ValueKind::Bool => choice.clone(),
        InspectorAction::Choice { choice, .. } if site.kind == ValueKind::Font => patch::fmt_text(choice),
        InspectorAction::Choice { choice, .. } => format!("@{choice}"),
        _ => return None,
    };
    Some((site.range.clone(), text, site.field().to_string()))
}

fn sketch_thumbnail(session: &MotionSession) -> (usize, usize, Vec<u32>) {
    let (w, h) = THUMB;
    let bg = session.doc.as_ref().and_then(|doc| doc.background).unwrap_or([0.04, 0.05, 0.07, 1.0]);
    let word = |c: [f64; 4]| {
        let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        (b(c[3]) << 24) | (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
    };
    let mut pixels = vec![word(bg); w * h];
    let Some(doc) = &session.doc else { return (w, h, pixels) };
    let frame = doc.eval(doc.duration * 0.5);
    let scale = (w as f64 / doc.size[0].max(1.0)).min(h as f64 / doc.size[1].max(1.0));
    for item in view_items(session, &frame) {
        let fill = frame.walk().into_iter().find(|(_, i)| i.path == item.path).and_then(|(_, i)| match &i.kind {
            ItemKind::Primitive(PrimitiveFrame::Rect { fill, stroke, .. }) | ItemKind::Primitive(PrimitiveFrame::Ellipse { fill, stroke, .. }) => fill.or(*stroke),
            ItemKind::Primitive(PrimitiveFrame::Path { stroke, fill, .. }) => stroke.or(*fill),
            ItemKind::Primitive(PrimitiveFrame::Text { units, .. }) => units.first().map(|unit| unit.color),
            _ => None,
        });
        let Some(fill) = fill else { continue };
        let xs = item.quad.map(|p| p[0] * scale);
        let ys = item.quad.map(|p| p[1] * scale);
        let (x0, x1) = (xs.iter().cloned().fold(f64::MAX, f64::min).max(0.0) as usize, xs.iter().cloned().fold(f64::MIN, f64::max).min(w as f64) as usize);
        let (y0, y1) = (ys.iter().cloned().fold(f64::MAX, f64::min).max(0.0) as usize, ys.iter().cloned().fold(f64::MIN, f64::max).min(h as f64) as usize);
        let ink = word(fill);
        for y in y0..y1.min(h) {
            for x in x0..x1.min(w) {
                pixels[y * w + x] = ink;
            }
        }
    }
    (w, h, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every layer of every shipped graphic gets an inspector from what its
    /// component declares and what the document writes; every text layer's
    /// words are a words box, every colour a colour, every `@name` a choice,
    /// and an edit of a site rewrites exactly its bytes.
    #[test]
    fn the_inspector_reads_every_layer_of_every_shipped_graphic() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/resources/motion");
        let mut texts = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("splash") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let mut session = MotionSession::new(source.clone(), "t".into(), None, None);
            if session.outline_error.is_some() {
                continue;
            }
            let _ = inspector_sections(&session);
            for layer in session.outline.all().into_iter().map(|l| l.path.clone()).collect::<Vec<_>>() {
                session.selection.layer = Some(layer.clone());
                let (_, _, _, sections) = inspector_sections(&session);
                let outline = session.outline.layer(&layer).unwrap().clone();
                let fields: Vec<&Field> = sections.iter().flat_map(|s| s.fields.iter()).collect();
                if let Some(words) = words_of(&session, &outline) {
                    texts += 1;
                    let field = fields.iter().find(|f| f.label == "Text").unwrap_or_else(|| panic!("{path:?} {layer}: no Text row"));
                    assert!(matches!(field.kind, FieldKind::Words), "{path:?} {layer}: text is {:?}", field.kind);
                    assert_eq!(field.text, words);
                }
                for field in fields.iter().filter(|f| f.id.starts_with("site:")) {
                    let index: usize = field.id[5..].parse().unwrap();
                    let site = session.resolved(&session.sites[index]);
                    match &site.kind {
                        ValueKind::Color => assert_eq!(field.kind, FieldKind::Color),
                        ValueKind::Enum { choices } if choices.len() > 1 => assert!(matches!(field.kind, FieldKind::Choice(_))),
                        ValueKind::Enum { .. } => assert_eq!(field.kind, FieldKind::Text),
                        _ => {}
                    }
                }
            }
        }
        assert!(texts > 20, "{texts} text layers");
    }

    /// An inspector edit of a site (a words box, a choice, numbers) is one
    /// splice: the rest of the document is byte-for-byte as written.
    #[test]
    fn inspector_site_edits_splice_one_value() {
        let doc = "use mod.motion.*; // hi\nlet ink = #ffffff\nMotion{size: vec2(640, 360) duration: 2.0 layers: [\n    Text{text: \"One\" font: Font{family: \"Inter\"} align: @left color: ink size: 40}\n]}";
        let session = MotionSession::new(doc.to_string(), "t".into(), None, None);
        let site = |field: &str| session.sites.iter().position(|s| s.field() == field && s.layer().is_some()).unwrap();
        let edit = |index: usize, action: InspectorAction| {
            let (range, text, _) = site_value(&session, index, &action).unwrap();
            super::super::session::apply(&session.source, &MotionOp::Splice { range, text, what: String::new() }).unwrap()
        };
        let next = edit(site("text"), InspectorAction::TextLive { id: String::new(), text: "Two\nlines".into() });
        assert_eq!(next, doc.replace("\"One\"", "\"Two\\nlines\""));
        let next = edit(site("align"), InspectorAction::Choice { id: String::new(), choice: "center".into() });
        assert_eq!(next, doc.replace("@left", "@center"));
        // The colour is the `let`'s: written there.
        let next = edit(site("color"), InspectorAction::Text { id: String::new(), text: "ff0000".into() });
        assert_eq!(next, doc.replace("let ink = #ffffff", "let ink = #ff0000"));
        let next = edit(site("size"), InspectorAction::Numbers { id: String::new(), values: vec![48.0] });
        assert_eq!(next, doc.replace("size: 40", "size: 48"));
    }

    /// Many tweaks of a scene's code (a value in the code dragged), some autosaved into the project
    /// tree on the way: each drag is one undo step, undo walks back through
    /// every one to the text as opened, and redo forward to the last.
    #[test]
    fn every_tweak_drag_is_one_undo_step_through_autosaves() {
        const SCENE: &str = r#"use mod.motion.*
let LOOK = {size: 40 glow: 0.5}
Motion{size: vec2(640, 360) duration: 2.0 layers: [
    Text{text: "One" size: LOOK.size at: vec2(160, 120)}
]}
"#;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut page = VideoCut::default();
        let mut session = MotionSession::new(SCENE.into(), "scene".into(), Some("g1".into()), None);
        // A plate of the project tree: autosaved into it two seconds after
        // a change, settled then.
        session.tree_path = Some("scenes/scene.splash".into());
        page.motion.desk.session = Some(session);
        let source = |page: &VideoCut| page.motion.desk.session.as_ref().unwrap().source.clone();
        // A scrub: steps while the finger is down, then the release.
        let drag = |page: &mut VideoCut, cx: &mut Cx, id: &str, steps: Vec<Vec<f64>>| {
            for values in &steps {
                inspector_action(page, cx, InspectorAction::NumbersLive { id: id.into(), values: values.clone() });
            }
            inspector_action(page, cx, InspectorAction::Numbers { id: id.into(), values: steps.last().unwrap().clone() });
        };
        let mut states = vec![SCENE.to_string()];
        for k in 0..12usize {
            let session = page.motion.desk.session.as_ref().unwrap();
            let v = k as f64;
            // A layer's value dragged (set in the running film, written
            // at the release).
            let byte = session.source.find("at: vec2(").unwrap() + 9;
            let index = session.code.literals.iter().rposition(|l| l.range.start <= byte && byte < l.range.end).unwrap();
            let field = literal_field(&session.code.literals[index], 0, index, "at".into());
            let id = field.id.clone();
            let mut step = field.values.clone();
            let mut steps = Vec::new();
            for d in [1.0, 5.0, 9.0 + v] {
                step[0] += d;
                steps.push(step.clone());
            }
            drag(&mut page, &mut cx, &id, steps);
            let now = source(&page);
            assert_ne!(now, *states.last().unwrap(), "drag {k} changed nothing");
            states.push(now);
            // Every third drag the autosave wrote the plate into the tree.
            if k % 3 == 2 {
                let session = page.motion.desk.session.as_mut().unwrap();
                session.saved = session.source.clone();
                assert!(settled(&page));
            }
        }
        assert_eq!(page.motion.desk.session.as_ref().unwrap().undo.len(), 12, "one step per drag");
        for k in (0..12).rev() {
            let revision = page.motion.desk.session.as_ref().unwrap().revision;
            undo(&mut page, false);
            assert_eq!(source(&page), states[k], "undo back to before drag {k}");
            // The views and the picture load the text undone to.
            assert!(page.motion.desk.session.as_ref().unwrap().revision > revision);
            assert!(page.motion.desk.changed_at.is_some(), "autosaved again");
        }
        assert_eq!(source(&page), SCENE);
        // Autosaved as opened: one more undo stops there (not into the
        // tree's last save, which has the drags again).
        {
            let session = page.motion.desk.session.as_mut().unwrap();
            session.saved = session.source.clone();
        }
        undo(&mut page, false);
        assert_eq!(source(&page), SCENE);
        assert_eq!(page.motion.desk.status, "Nothing to undo");
        for k in 1..=12 {
            undo(&mut page, true);
            assert_eq!(source(&page), states[k], "redo of drag {}", k - 1);
        }
        // Back half-way and a new drag: the redo steps go, the undo stays.
        for _ in 0..6 {
            undo(&mut page, false);
        }
        assert_eq!(source(&page), states[6]);
        let session = page.motion.desk.session.as_ref().unwrap();
        let byte = session.source.find("at: vec2(").unwrap() + 9;
        let index = session.code.literals.iter().rposition(|l| l.range.start <= byte && byte < l.range.end).unwrap();
        let field = literal_field(&session.code.literals[index], 0, index, "at".into());
        let id = field.id.clone();
        let mut values = field.values.clone();
        values[0] += 99.0;
        drag(&mut page, &mut cx, &id, vec![values]);
        let session = page.motion.desk.session.as_ref().unwrap();
        assert!(session.redo.is_empty());
        assert_eq!(session.undo.len(), 7);
        undo(&mut page, false);
        assert_eq!(source(&page), states[6]);
    }

    #[test]
    fn path_boxes_cover_their_coordinates() {
        assert_eq!(path_bounds(&makepad_motion::geometry::parse_path("M0 0 L400 0"), 8.0), [408.0, 9.0]);
        assert_eq!(path_bounds(&makepad_motion::geometry::parse_path("M10 20 L500 80 Z"), 0.0), [500.0, 80.0]);
    }
}

/// `doc` with its `Character{src: asset(…)}` models built from the SVGs the
/// desk fetched (lane CH1); the same document when it has none.
fn with_characters(doc: &std::sync::Arc<makepad_motion::MotionDoc>, svgs: &std::collections::HashMap<makepad_motion::Media, String>) -> std::sync::Arc<makepad_motion::MotionDoc> {
    if doc.character_models().is_empty() {
        return doc.clone();
    }
    let mut out = (**doc).clone();
    out.resolve_characters(&|asset| svgs.get(&makepad_motion::Media::Asset(asset.clone())).cloned());
    std::sync::Arc::new(out)
}
