//! The cut's own editor panel: the whole film's controls, a Splash
//! program written into the cut, `let cut_panel = fn() { View{ … } }`,
//! run in its isolate down the CUT page's left side (the same runtime and
//! `scene` API as a graphic's panel, `makepad_editor_panel`).
//!
//! Its paths name their document first ([`makepad_motion::paths::docs`]):
//! `edit.p_open.span` (and a clip's `cut`, rolled on the cut's beats),
//! `p_open:WORD_SPARKS.count` (a graphic's own), `look.POST.bloom` (a module
//! the graphics share); a set (`*:LYRIC_LOOK.font`, `edit.*.cut`,
//! `lyric_plates:BONE` from `let cut_sets = {…}`) writes every member.
//! `scene.open("p_open")` opens a graphic's own panel.
//!
//! The documents are the cut's text and each graphic's and module's
//! library revision. A gesture's edits are held on those texts (the
//! picture follows where the running film takes the value: see `live`);
//! at the release every document it changed is saved as its next revision
//! (modules first, their imports following them, then the graphics), and
//! the cut points its clips at the new revisions in ONE edit operation:
//! cmd-Z in the cut undoes all of them together.
//!
//! The panel is made by the cut's agent when a cut without one opens
//! (`make.rs`), remade with a brief, and changed from the chat.
//!
//! Main-loop cost: per pump a compare of the cut's revision; the texts are
//! fetched from the library once per revision on the task pool; a gesture's
//! step resolves its path (a scan of the `let` it names) and splices.

use super::motion_edit::store::{MotionJob, MotionReply, MotionStore};
use super::ops::CutOp;
use super::VideoCut;
use makepad_motion::paths::docs::{DocTable, NodeDoc};
use makepad_motion::paths::{self, File};
use makepad_motion::schema::code::{outline, CodeOutline};
use makepad_editor_panel::{self as panel, EditorPanelWidgetRefExt};
use makepad_story_cut::{AssetRef, GraphicSrc};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;

mod make;
pub(super) use make::lacks_panel;
/// The song's own timing (beats, sections, words) in the cut's seconds.
mod timing;
pub use timing::SongTiming;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    /// The cut's panel column: the panel, or its making.
    mod.widgets.CutPanelColumn = View{width: Fill height: Fill flow: Down
        ep_panel := mod.widgets.EditorPanel{visible: false}
        ep_making := View{width: Fill height: Fill flow: Down visible: false padding: Inset{left: 12 right: 12 top: 10 bottom: 10} spacing: 10
            Label{width: Fill text: "FILM" draw_text +: {color: stage.secondary text_style: theme.font_bold{font_size: 8}}}
            ep_progress := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 9}}}
        }
    }
}

/// Who holds the panel widgets' host.
const OWNER: &str = "edit";
/// A document a held gesture loads again does so at most this often.
const RELOAD_EVERY: f64 = 0.25;
/// Saves at once (each store a worker with its own connection).
const STORES: usize = 4;
/// The cut's panel's `let`.
pub const LET: &str = "cut_panel";

/// What a document is.
#[derive(Clone, Debug, PartialEq)]
enum Of {
    Cut,
    Graphic(AssetRef),
    Module(AssetRef),
}

/// One of the documents the panel reaches: its text now (with the panel's
/// changes not saved yet) and as its revision has it.
struct Doc {
    of: Of,
    name: String,
    text: String,
    loaded: String,
    /// The text the viewer's running document was loaded from (its
    /// revision's, or what a held step loaded again).
    running: String,
    code: CodeOutline,
}

impl Doc {
    fn new(of: Of, name: String, text: String) -> Self {
        let code = outline(&text);
        Self { of, name, loaded: text.clone(), running: text.clone(), text, code }
    }

    fn set_text(&mut self, text: String) {
        self.code = outline(&text);
        self.text = text;
    }
}

/// Saving what a gesture changed: one document at a time, then the cut.
struct Commit {
    started: f64,
    /// The document being saved, and the job's serial.
    waiting: Vec<(u64, usize)>,
    saved: Vec<String>,
}

#[derive(Default)]
pub struct State {
    runtime: panel::Runtime,
    docs: Vec<Doc>,
    table: DocTable,
    /// The cut revision the documents were built for.
    built_for: Option<u64>,
    /// Library texts by (asset, revision).
    texts: HashMap<(String, String), String>,
    fetching: Option<TaskHandle<Result<Vec<(String, String, String)>, String>>>,
    fetch_error: Option<String>,
    /// The texts the isolate reads changed since they were given.
    dirty_scene: bool,
    /// Stores of its own (the panel's saves never wait behind the graphic
    /// desk's), the documents spread over them.
    stores: Vec<MotionStore>,
    serial: u64,
    commit: Option<Commit>,
    /// A save failed: the changes stay held until the next gesture.
    failed: bool,
    /// The last commit: what it saved and how long it took.
    pub last_commit: Option<(Vec<String>, f64)>,
    pub status: String,
    status_shown: String,
    /// The panel's own agent: making, remaking, the chat.
    make: make::Make,
    /// The documents' changes since the last gesture began (for the
    /// control API's report).
    pub gestures: u64,
    lit: (u64, Vec<String>),
    /// The next renderer loads behind the shown one (see `take_held_switch`).
    held_switch: bool,
    /// A gesture the control API plays, a step per frame, timing each from
    /// its value set to the picture showing it.
    drive: Option<Drive>,
    /// The song's timing in the cut's seconds, when a module writes it.
    pub timing: Option<SongTiming>,
    /// The revision and pictures the scene cards were made for.
    cards_of: Option<(u64, usize)>,
    /// The program the `when:`s were read for.
    when_of: Option<String>,
    /// The renderer (by the revision it shows) and whether its graphics
    /// load live.
    live_set: Option<(Option<u64>, bool)>,
    /// The last held step: places it set in the running film, and the
    /// documents that show it only at the release.
    pub live_places: usize,
    /// Documents to load again (a held step they could not take), from when.
    reload_due: HashMap<usize, f64>,
    reload_sent: HashMap<usize, f64>,
    /// Documents loaded again during gestures (the control API's report).
    pub reloads: usize,
    pub patch_ms: f64,
    pub later: Vec<String>,
    focus_shown: Vec<String>,
}

// ------------------------------------------------------------------ the hooks' view of the film

#[derive(Default)]
struct Feed {
    playhead: (f64, f64, bool),
    beat: f64,
    /// Each clip's span.
    spans: Vec<(String, f64, f64)>,
    /// Per file: the graphics that read it (the cut's file: none).
    users: Vec<Vec<String>>,
    cut_file: usize,
    lit_rev: u64,
    focus: Option<Vec<String>>,
    /// Each asked path's places: (file, member).
    reach: HashMap<String, Vec<(usize, String)>>,
    timing: Option<SongTiming>,
    /// Each clip's card (its picture from the library's thumbnails).
    cards: HashMap<String, makepad_value_pickers::panel::host::SceneCard>,
    /// What the panel's `when:`s say: a module's paths and its spans.
    when: Vec<(Vec<String>, Vec<(f64, f64)>)>,
}

thread_local! {
    static FEED: RefCell<Feed> = RefCell::new(Feed::default());
}

/// Whether `path` acts at the playhead: a clip's when the playhead is in
/// it, a graphic's or a module's when a graphic reading it shows then.
/// When a path acts: the spans its module's `when:` says, else the clips
/// that show what it reaches.
fn spans_of(path: &str) -> Vec<(f64, f64)> {
    if let Some(spans) = FEED.with(|f| f.borrow().when.iter().find(|(paths, _)| paths.iter().any(|p| p == path)).map(|(_, s)| s.clone())) {
        return spans;
    }
    let Ok(bounds) = panel::scene_bounds(path) else { return Vec::new() };
    FEED.with(|feed| {
        let feed = feed.borrow();
        let mut out = Vec::new();
        for b in bounds {
            let members = if b.site.file() == feed.cut_file { vec![b.member] } else { feed.users.get(b.site.file()).cloned().unwrap_or_default() };
            for m in members {
                if let Some((_, a, e)) = feed.spans.iter().find(|(c, ..)| *c == m) {
                    if !out.contains(&(*a, *e)) {
                        out.push((*a, *e));
                    }
                }
            }
        }
        out
    })
}

/// The panel's modules: each one's `when:` and every path in it.
fn when_records(body: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = body[from..].find("PanelModule{").map(|i| from + i) {
        let open = at + "PanelModule".len();
        let (mut depth, mut end, mut quoted) = (0i32, body.len(), false);
        for (i, c) in body[open..].char_indices() {
            match c {
                '"' => quoted = !quoted,
                '{' if !quoted => depth += 1,
                '}' if !quoted => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let text = &body[open..end];
        let field = |key: &str, from: &str| -> Vec<String> {
            from.match_indices(&format!("{key}: \"")).filter_map(|(i, _)| {
                let rest = &from[i + key.len() + 3..];
                Some(rest[..rest.find('"')?].to_string())
            }).collect()
        };
        let head = &text[..text.find('\n').unwrap_or(text.len())];
        let when = field("when", head).into_iter().next().unwrap_or_default();
        let mut paths = field("path", text);
        for t in field("targets", text) {
            for target in t.split(',') {
                let words: Vec<&str> = target.split_whitespace().collect();
                if words.len() > 2 {
                    paths.push(words[..words.len() - 2].join(" "));
                }
            }
        }
        out.push((when, paths));
        from = end;
    }
    out
}

fn active(path: &str) -> Option<bool> {
    // Measured, never declared (a module's `when:` covers paths it shares
    // with others): where a path reaches, resolved once per state of the
    // texts, is in use while a clip it reaches is on.
    let reach = FEED.with(|feed| feed.borrow().reach.get(path).cloned()).or_else(|| {
        let r: Vec<(usize, String)> = panel::scene_bounds(path).ok()?.into_iter().map(|b| (b.site.file(), b.member)).collect();
        FEED.with(|feed| feed.borrow_mut().reach.insert(path.to_string(), r.clone()));
        Some(r)
    })?;
    FEED.with(|feed| {
        let feed = feed.borrow();
        let t = feed.playhead.0;
        let on = |clip: &str| feed.spans.iter().any(|(c, a, b)| c == clip && t >= *a && t < *b);
        Some(reach.iter().any(|(file, member)| {
            if *file == feed.cut_file {
                on(member)
            } else {
                feed.users.get(*file).is_some_and(|users| users.iter().any(|u| on(u)))
            }
        }))
    })
}

fn hooks(host: makepad_value_pickers::panel::host::PanelHost) -> makepad_value_pickers::panel::host::PanelHost {
    makepad_value_pickers::panel::host::PanelHost {
        playhead: Some(Box::new(|| FEED.with(|f| f.borrow().playhead))),
        // The song's beats and sung words (else the cut's tempo).
        beats: Some(Box::new(|t0, t1| FEED.with(|f| {
            let f = f.borrow();
            if let Some(timing) = &f.timing {
                return timing.beats.iter().copied().filter(|t| *t >= t0 && *t <= t1).collect();
            }
            if f.beat <= 1e-3 {
                return Vec::new();
            }
            let first = (t0 / f.beat).ceil() as i64;
            (first..).map(|k| k as f64 * f.beat).take_while(|t| *t <= t1).take(4096).collect()
        }))),
        words: Some(Box::new(|t0, t1| FEED.with(|f| f.borrow().timing.as_ref().map_or(Vec::new(), |t| t.words.iter().filter(|(_, a, b)| *b >= t0 && *a <= t1).map(|(w, a, b)| (*a, *b, w.clone())).collect())))),
        spans: Some(Box::new(|paths: &[String]| paths.iter().flat_map(|p| spans_of(p)).collect())),
        scene: Some(Box::new(|node: &str| FEED.with(|f| f.borrow().cards.get(node).cloned()))),
        open: Some(Box::new(panel::open)),
        active: Some(Box::new(active)),
        lit_rev: Some(Box::new(|| FEED.with(|f| f.borrow().lit_rev))),
        focus: Some(Box::new(|paths: Option<&[String]>| FEED.with(|f| f.borrow_mut().focus = paths.map(|p| p.to_vec())))),
        ask: Some(Box::new(|prompt: &str, targets: &[String], mode: &str| make::ask(prompt, targets, mode))),
        ask_status: Some(Box::new(make::ask_status)),
        ..host
    }
}

// ------------------------------------------------------------------ documents

/// The texts of the cut's graphics and the modules they import, from the
/// library (what is not known yet): (asset, revision, text).
fn fetch(wanted: Vec<AssetRef>, known: Vec<(String, String)>) -> Result<Vec<(String, String, String)>, String> {
    let library = makepad_motion::modules::Modules::default();
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut todo: Vec<(String, String)> = wanted.into_iter().map(|a| (a.id, a.rev)).collect();
    let mut seen: std::collections::HashSet<(String, String)> = known.into_iter().collect();
    while let Some((id, rev)) = todo.pop() {
        if !seen.insert((id.clone(), rev.clone())) {
            continue;
        }
        let text = library.0.asset(&id, &rev).ok_or_else(|| format!("{id}@{rev} is not in the library"))?.text.to_string();
        todo.extend(makepad_motion::modules::asset_imports(&text).into_iter().map(|r| (r.id, r.rev)));
        out.push((id, rev, text));
    }
    Ok(out)
}

/// `let NAME = import(asset("ID", "REV"))` lines of a text.
fn imports(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("let ") else { continue };
        if !line.contains("import(asset(") {
            continue;
        }
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let quoted: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        if let (false, [id, rev, ..]) = (name.is_empty(), quoted.as_slice()) {
            out.push((name, id.to_string(), rev.to_string()));
        }
    }
    out
}

impl State {
    fn files(&self) -> Vec<File<'_>> {
        self.docs.iter().map(|d| File { name: "", text: &d.text, code: &d.code }).collect()
    }

    /// Something the panel changed is not saved yet.
    fn unsaved(&self) -> bool {
        self.docs.iter().any(|d| d.text != d.loaded)
    }

    /// The documents of the cut as it is now, when every text is known;
    /// else what is missing is fetched.
    fn build(&mut self, cx: &mut Cx, page_source: &str, doc: &makepad_story_cut::CutDoc) -> bool {
        let graphics: Vec<(String, AssetRef)> = doc.graphics.iter().filter_map(|g| match &g.src {
            GraphicSrc::Asset(a) => Some((g.id.clone(), a.clone())),
            GraphicSrc::Builtin(_) | GraphicSrc::Tree(_) => None,
        }).collect();
        // Every text needed, known?
        let mut need: Vec<AssetRef> = Vec::new();
        let mut todo: Vec<AssetRef> = graphics.iter().map(|(_, a)| a.clone()).collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(a) = todo.pop() {
            if !seen.insert((a.id.clone(), a.rev.clone())) {
                continue;
            }
            match self.texts.get(&(a.id.clone(), a.rev.clone())) {
                Some(text) => todo.extend(makepad_motion::modules::asset_imports(text).into_iter().map(|r| AssetRef { id: r.id, rev: r.rev })),
                None => need.push(a),
            }
        }
        if !need.is_empty() {
            if self.fetching.is_none() {
                let known: Vec<(String, String)> = self.texts.keys().cloned().collect();
                match cx.task_pool().submit(Class::Background, move || fetch(need, known)) {
                    Ok(task) => self.fetching = Some(task),
                    Err(error) => self.fetch_error = Some(format!("{error:?}")),
                }
            }
            return false;
        }
        // The cut, its graphics (one document per revision), their modules.
        let mut docs = vec![Doc::new(Of::Cut, "edit".into(), page_source.to_string())];
        let mut nodes: Vec<NodeDoc> = Vec::new();
        let mut file_of: HashMap<(String, String), usize> = HashMap::new();
        for (node, a) in &graphics {
            let key = (a.id.clone(), a.rev.clone());
            let file = *file_of.entry(key.clone()).or_insert_with(|| {
                let text = self.texts[&key].clone();
                docs.push(Doc::new(Of::Graphic(a.clone()), makepad_motion::patch::label_of(&text).unwrap_or_else(|| node.clone()), text));
                docs.len() - 1
            });
            nodes.push(NodeDoc { node: node.clone(), file, imports: Vec::new() });
        }
        // Modules, in the order they are first imported; each named by what
        // most of the documents call it.
        let mut names: HashMap<(String, String), HashMap<String, usize>> = HashMap::new();
        let mut k = 1;
        while k < docs.len() {
            for (name, id, rev) in imports(&docs[k].text.clone()) {
                let key = (id.clone(), rev.clone());
                *names.entry(key.clone()).or_default().entry(name).or_default() += 1;
                if !file_of.contains_key(&key) {
                    if let Some(text) = self.texts.get(&key) {
                        docs.push(Doc::new(Of::Module(AssetRef { id, rev }), String::new(), text.clone()));
                        file_of.insert(key, docs.len() - 1);
                    }
                }
            }
            k += 1;
        }
        let mut modules: Vec<(String, usize)> = Vec::new();
        for (k, d) in docs.iter_mut().enumerate() {
            if let Of::Module(a) = &d.of {
                let name = names.get(&(a.id.clone(), a.rev.clone())).and_then(|n| n.iter().max_by_key(|(name, count)| (**count, std::cmp::Reverse((*name).clone()))).map(|(n, _)| n.clone())).unwrap_or_else(|| a.id.clone());
                d.name = name.clone();
                if !modules.iter().any(|(n, _)| *n == name) {
                    modules.push((name, k));
                }
            }
        }
        for node in &mut nodes {
            node.imports = imports(&docs[node.file].text).into_iter().filter_map(|(name, id, rev)| file_of.get(&(id, rev)).map(|f| (name, *f))).collect();
        }
        // Picture and sound are lanes of their own (a sound's track 0 is
        // not the picture's).
        use makepad_story_cut::CutNodeKind as K;
        let clips: Vec<(String, u32, f64, f64)> = doc.graphics.iter().filter_map(|g| g.span.map(|s| (g.id.clone(), g.track, s.start, s.length)))
            .chain(doc.nodes.iter().filter_map(|n| {
                let lane = match n.kind {
                    K::Clip { .. } | K::Still { .. } | K::Title { .. } => 0,
                    K::AudioClip { .. } | K::VoiceOver { .. } => 1000,
                    _ => return None,
                };
                n.span.map(|s| (n.id.clone(), n.track + lane, s.start, s.length))
            }))
            .collect();
        let cut_files = [File { name: "", text: &docs[0].text, code: &docs[0].code }];
        let sets: Vec<(String, String)> = paths::list(&cut_files).into_iter()
            .filter_map(|e| Some((e.path.strip_prefix("cut_sets.")?.to_string(), e.value.trim_matches('"').to_string())))
            .collect();
        let beat = (doc.bpm > 1.0).then(|| 60.0 / doc.bpm);
        // The song's timing, from the module that writes it, where the cut
        // plays the song (its clip's start, less where in the song it starts).
        let song_at = doc.nodes.iter().find_map(|n| match &n.kind {
            K::AudioClip { source_in, .. } => Some(n.span?.start - match source_in { makepad_story_cut::SourceTime::Seconds(s) => *s, _ => 0.0 }),
            _ => None,
        }).unwrap_or(0.0);
        self.timing = docs.iter().filter(|d| matches!(d.of, Of::Module(_))).find_map(|d| SongTiming::parse(&d.text)).map(|t| t.shifted(song_at));
        let beats = self.timing.as_ref().map(|t| t.beats.clone()).unwrap_or_default();
        self.table = DocTable { edit: Some(0), clips, nodes, modules, sets, beats, beat };
        self.when_of = None;
        self.docs = docs;
        self.dirty_scene = true;
        // The hooks' view: who reads each file.
        FEED.with(|f| {
            let mut f = f.borrow_mut();
            f.beat = beat.unwrap_or(0.0);
            f.timing = self.timing.clone();
            f.cut_file = 0;
            f.spans = self.table.clips.iter().map(|(c, _, s, l)| (c.clone(), *s, s + l)).collect();
            let mut users = vec![Vec::new(); self.docs.len()];
            for node in &self.table.nodes {
                users[node.file].push(node.node.clone());
                for (_, m) in &node.imports {
                    users[*m].push(node.node.clone());
                }
            }
            f.users = users;
        });
        true
    }

    /// Splice the panel's edits into the documents (held while the finger
    /// is down): each value written as one literal of its kind, the text
    /// checked to still read before it is kept.
    fn apply(&mut self, edits: Vec<panel::PanelEdit>) -> Vec<usize> {
        let mut changed = Vec::new();
        for edit in edits {
            let s = &edit.splice;
            let Some(doc) = self.docs.get_mut(s.file) else { continue };
            let text = &doc.text;
            if s.range.end > text.len() {
                continue;
            }
            // A value: exactly its literal's bytes (a number with its sign,
            // a colour, a string), as the cut primitive writes it.
            let range = if edit.kind.is_numeric() || matches!(edit.kind, panel::ValueKind::Text | panel::ValueKind::Bool) {
                makepad_motion::edit::literal_token(text, &s.range).unwrap_or(s.range.clone())
            } else {
                s.range.clone()
            };
            let mut next = String::with_capacity(text.len() + s.text.len());
            next.push_str(&text[..range.start]);
            next.push_str(&s.text);
            next.push_str(&text[range.end..]);
            if let Err(why) = makepad_motion::edit::balanced(&next) {
                self.status = format!("{}: not written ({why})", doc.name);
                continue;
            }
            doc.set_text(next);
            if !changed.contains(&s.file) {
                changed.push(s.file);
            }
        }
        if !changed.is_empty() {
            self.dirty_scene = true;
        }
        changed
    }

    /// The next step of saving: a changed module, else a changed graphic,
    /// else the cut pointed at the new revisions (one operation).
    fn commit_step(&mut self, page_ops: &mut Vec<CutOp>) {
        let Some(commit) = &self.commit else { return };
        let waiting: Vec<usize> = commit.waiting.iter().map(|(_, k)| *k).collect();
        let changed = |d: &Doc| d.text != d.loaded;
        let module_pending = self.docs.iter().enumerate().any(|(k, d)| matches!(d.of, Of::Module(_)) && (changed(d) || waiting.contains(&k)));
        // Modules first (a module importing one being saved waits for it:
        // its import follows the new revision), then every graphic at once.
        let next: Vec<usize> = if module_pending {
            let busy: Vec<&AssetRef> = self.docs.iter().enumerate().filter(|(k, d)| matches!(d.of, Of::Module(_)) && (changed(d) || waiting.contains(k))).filter_map(|(_, d)| match &d.of { Of::Module(a) => Some(a), _ => None }).collect();
            self.docs.iter().enumerate().filter(|(k, d)| matches!(d.of, Of::Module(_)) && changed(d) && !waiting.contains(k))
                .filter(|(_, d)| !imports(&d.text).iter().any(|(_, id, rev)| busy.iter().any(|a| a.id == *id && a.rev == *rev)))
                .map(|(k, _)| k).collect()
        } else {
            self.docs.iter().enumerate().filter(|(k, d)| matches!(d.of, Of::Graphic(_)) && changed(d) && !waiting.contains(k)).map(|(k, _)| k).collect()
        };
        if next.is_empty() && waiting.is_empty() && !module_pending {
            let commit = self.commit.take().expect("checked above");
            if self.docs[0].text != self.docs[0].loaded {
                page_ops.push(CutOp::Source { text: self.docs[0].text.clone() });
                self.held_switch = true;
                // Sent: the cut is that text from now on (the page applies
                // it as one operation).
                self.docs[0].loaded = self.docs[0].text.clone();
            }
            let ms = (Cx::time_now() - commit.started) * 1000.0;
            log!("edit panel: saved {} in {ms:.0} ms", if commit.saved.is_empty() { "the cut".to_string() } else { commit.saved.join(", ") });
            self.status = if commit.saved.is_empty() { String::new() } else { format!("Saved {} ({ms:.0} ms)", commit.saved.join(", ")) };
            self.last_commit = Some((commit.saved, ms));
            return;
        }
        for k in next {
            let (Of::Module(a) | Of::Graphic(a)) = &self.docs[k].of else { continue };
            let (Ok(asset_id), Ok(revision)) = (a.id.parse(), a.rev.parse()) else {
                self.status = format!("{} is not a library asset", self.docs[k].name);
                self.commit = None;
                self.failed = true;
                return;
            };
            // Values and the revisions it names changed, nothing else: saved
            // as it is, with the picture of the revision before. Spread
            // over the stores.
            let from = makepad_asset_data::AssetRevisionRef { asset_id, revision };
            let job = MotionJob::Revise { serial: self.serial + 1, from, title: self.docs[k].name.clone(), source: self.docs[k].text.clone() };
            let Some(store) = self.stores.get(k % self.stores.len().max(1)) else { return };
            match store.try_send(job) {
                Ok(()) => {
                    self.serial += 1;
                    if let Some(c) = &mut self.commit {
                        c.waiting.push((self.serial, k));
                    }
                }
                Err(_) => {
                    if let Some(error) = &store.error {
                        self.status = format!("Not saved: {error}");
                        self.commit = None;
                        self.failed = true;
                        return;
                    }
                }
            }
        }
    }

    /// A revision saved: the documents importing it (or the cut showing it)
    /// follow it.
    fn saved(&mut self, serial: u64, result: Result<makepad_asset_data::AssetRevisionRef, String>) {
        let Some(commit) = &mut self.commit else { return };
        let Some(at) = commit.waiting.iter().position(|(s, _)| *s == serial) else { return };
        let (_, k) = commit.waiting.remove(at);
        let published = match result {
            Ok(p) => p,
            Err(error) => {
                log!("edit panel: {} was not saved: {error}", self.docs[k].name);
                self.status = format!("{} was not saved: {error}", self.docs[k].name);
                self.commit = None;
                // Not tried again until the next gesture.
                self.failed = true;
                return;
            }
        };
        let (old, rev) = match &self.docs[k].of {
            Of::Module(a) | Of::Graphic(a) => (a.clone(), published.revision.to_string()),
            Of::Cut => return,
        };
        commit.saved.push(self.docs[k].name.clone());
        let text = self.docs[k].text.clone();
        self.texts.insert((old.id.clone(), rev.clone()), text.clone());
        let new = AssetRef { id: old.id.clone(), rev: rev.clone() };
        self.docs[k].of = match &self.docs[k].of {
            Of::Module(_) => Of::Module(new),
            _ => Of::Graphic(new),
        };
        self.docs[k].loaded = text;
        // Every `asset("id", "old")` names the new revision (imports in the
        // other documents, the cut's clips).
        let from = format!("asset(\"{}\", \"{}\")", old.id, old.rev);
        let to = format!("asset(\"{}\", \"{}\")", old.id, rev);
        for (j, d) in self.docs.iter_mut().enumerate() {
            if j != k && d.text.contains(&from) {
                let next = d.text.replace(&from, &to);
                d.set_text(next);
            }
        }
        self.dirty_scene = true;
    }
}

/// `#rrggbb` (`#rrggbbaa` when not opaque).
fn hex(c: [f64; 4]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if b(c[3]) == 255 { format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2])) } else { format!("#{:02x}{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]), b(c[3])) }
}

// ------------------------------------------------------------------ a driven gesture

struct Drive {
    path: String,
    /// Each step's edit, still to play.
    todo: std::collections::VecDeque<panel::Edit>,
    /// Played: what, when it was queued, places set live, ms to the picture.
    steps: Vec<(String, f64, usize, Option<f64>, f64)>,
    released: bool,
}

/// One step of a driven gesture: the last one shown (the preview whole
/// again), then the next queued; after the last, the release.
fn drive_step(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let exact = ui.widget(cx, ids!(cut_preview)).borrow::<super::preview::CutPreview>().is_some_and(|p| p.frame_exact());
    let (places, patch_ms) = (page.cut_panel.live_places, page.cut_panel.patch_ms);
    let Some(drive) = page.cut_panel.drive.as_mut().filter(|d| !d.released) else { return };
    cx.new_next_frame();
    if let Some(last) = drive.steps.last_mut().filter(|s| s.3.is_none()) {
        last.2 = places;
        last.4 = patch_ms;
        let waited = (Cx::time_now() - last.1) * 1000.0;
        // (A step nothing showed live is not waited for long.)
        if !(exact && waited > 1.0) && !(waited > 3000.0 || (places == 0 && waited > 100.0)) {
            return;
        }
        last.3 = Some(waited);
    }
    match drive.todo.pop_front() {
        Some(edit) => {
            panel::press();
            let what = match &edit {
                panel::Edit::Set { value, .. } => value.to_json(),
                panel::Edit::SetCode { code, .. } => code.clone(),
                panel::Edit::Recolour { map } => format!("{} colours", map.len()),
                _ => String::new(),
            };
            panel::queue(edit);
            drive.steps.push((what, Cx::time_now(), 0, None, 0.0));
        }
        None => {
            panel::release();
            drive.released = true;
        }
    }
}

// ------------------------------------------------------------------ live

/// The name a document's literal sites have in the running film (the cut
/// preview loads each graphic as `motion:<id>@<rev>`, a module as
/// `module:<id>@<rev>`).
fn site_file(of: &Of) -> Option<String> {
    match of {
        Of::Graphic(a) => Some(format!("motion:{}@{}", a.id, a.rev)),
        Of::Module(a) => Some(format!("module:{}@{}", a.id, a.rev)),
        Of::Cut => None,
    }
}

/// Whether the next new renderer is of a revision the cut's panel saved:
/// what the shown picture holds live (it loads behind it). Taken.
pub fn take_held_switch(state: &mut State) -> bool {
    std::mem::take(&mut state.held_switch)
}

/// Whether the cut's panel is open (its documents' values are patched
/// into the running film live).
pub fn live(page: &VideoCut) -> bool {
    let state = &page.cut_panel;
    !super::motion_edit::is_open(page) && state.docs.first().is_some_and(|d| panel::let_region(&d.text, &d.code, LET).is_some())
}

/// The files whose literals compile live (`Cx::set_live_literals`) while
/// the cut's panel is open: every graphic and module as the film loaded it.
pub fn live_files(page: &VideoCut) -> Vec<String> {
    if !live(page) {
        return Vec::new();
    }
    let mut out: Vec<String> = page.cut_panel.docs.iter().filter_map(|d| site_file(&d.of)).collect();
    out.sort();
    out
}

/// A held step reached the documents `changed`: each literal it moved is
/// set where the running film holds it (`Cx::patch_literal`). What cannot
/// be set there (a value the load computed with, a change that is not a
/// literal) shows at the release, when the documents' new revisions load.
fn patch_live(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, changed: &[usize]) {
    use makepad_widgets::makepad_script::literal::{literal_edits, LiteralSite};
    let t0 = Cx::time_now();
    let preview_ref = ui.widget(cx, ids!(cut_preview));
    let mut places = 0;
    let mut reload: Vec<usize> = Vec::new();
    // Each colour moved, from what the running documents hold.
    let mut colours: Vec<([f32; 4], [f32; 4])> = Vec::new();
    // Per document: whether all its literals landed, else whether only
    // colours are left (their copies may still take them).
    let mut colour_only: Vec<usize> = Vec::new();
    let mut retime = false;
    {
        let state = &mut page.cut_panel;
        for &k in changed {
            let Some(doc) = state.docs.get(k) else { continue };
            let Some(file) = site_file(&doc.of) else {
                retime = true;
                continue;
            };
            let Some(edits) = literal_edits(&doc.running, &doc.text) else {
                reload.push(k);
                continue;
            };
            let (mut reached, mut colour_left) = (true, true);
            for e in edits {
                let site = LiteralSite::new(&file, e.line(), e.col());
                let reach = cx.patch_literal(&site, e.value());
                places += reach.places();
                if let Some(now) = e.value().as_color() {
                    let was = makepad_motion::schema::literal_at(&doc.running, e.line(), e.col())
                        .and_then(|(r, _)| makepad_motion::palette::sites(&doc.running[r]).first().map(|(_, c)| c.map(|v| v as f32)));
                    if let Some(was) = was {
                        if !colours.iter().any(|(w, _)| *w == was) {
                            colours.push((was, now));
                        }
                    }
                } else if reach.places() == 0 || reach.needs_reload() {
                    colour_left = false;
                }
                reached &= reach.places() > 0 && !reach.needs_reload();
            }
            if !reached {
                if colour_left { colour_only.push(k) } else { reload.push(k) }
            }
        }
    }
    let mut preview = preview_ref.borrow_mut::<super::preview::CutPreview>();
    // The copies the loads made of the colours moved (records, lets).
    if !colours.is_empty() {
        let copied = preview.as_mut().map_or(0, |p| p.recolour_copies(&colours));
        places += copied;
        if copied == 0 {
            reload.extend(colour_only);
        }
    }
    // The clips moved: the viewer re-times them at once.
    if retime {
        if let Some(doc) = retimed(page) {
            if let Some(p) = preview.as_mut() {
                p.retime(cx, doc);
            }
        }
    }
    if places > 0 {
        if let Some(p) = preview.as_mut() {
            p.literals_patched(cx);
        }
    }
    drop(preview);
    // What the running documents cannot take loads again (at most every
    // quarter second per document, the last step always).
    let now = Cx::time_now();
    let state = &mut page.cut_panel;
    for k in reload {
        state.reload_due.entry(k).or_insert(now);
    }
    state.live_places = places;
    state.later = state.reload_due.keys().filter_map(|k| state.docs.get(*k).map(|d| d.name.clone())).collect();
    state.patch_ms = (Cx::time_now() - t0) * 1000.0;
}

/// Documents due to load again (a held step they could not take): their
/// texts to the viewer's renderer, behind the documents that ran.
fn reload_due(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let now = Cx::time_now();
    let state = &mut page.cut_panel;
    let due: Vec<usize> = state.reload_due.iter().filter(|(k, at)| now >= **at && state.reload_sent.get(*k).is_none_or(|t| now - t >= RELOAD_EVERY)).map(|(k, _)| *k).collect();
    if due.is_empty() {
        if !state.reload_due.is_empty() {
            cx.new_next_frame();
        }
        return;
    }
    let (mut graphics, mut modules) = (Vec::new(), Vec::new());
    for k in due {
        state.reload_due.remove(&k);
        state.reload_sent.insert(k, now);
        let doc = &mut state.docs[k];
        if doc.running == doc.text {
            continue;
        }
        doc.running = doc.text.clone();
        match &doc.of {
            Of::Graphic(a) => graphics.push((a.clone(), doc.text.clone())),
            Of::Module(a) => modules.push((a.clone(), doc.text.clone())),
            Of::Cut => {}
        }
    }
    state.reloads += graphics.len() + modules.len();
    if let Some(mut p) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<super::preview::CutPreview>() {
        p.replace_texts(cx, graphics, modules);
    }
}

/// The cut as its clips are now (a held gesture on their times): the
/// open cut's document with each clip's span read from the text.
fn retimed(page: &VideoCut) -> Option<std::sync::Arc<makepad_story_cut::CutDoc>> {
    let session = page.session.as_ref()?;
    let state = &page.cut_panel;
    let files = state.files();
    let mut doc = (*session.doc).clone();
    let span_of = |id: &str| -> Option<(f64, f64)> {
        let b = makepad_motion::paths::docs::resolve(&files, &state.table, &format!("edit.{id}.span")).ok()?;
        let n: Vec<f64> = b[0].site.text.trim_matches(|c| c == '[' || c == ']').split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (n.len() == 2).then(|| (n[0], n[1]))
    };
    for g in &mut doc.graphics {
        if let (Some(span), Some((a, l))) = (g.span.as_mut(), span_of(&g.id)) {
            span.start = a;
            span.length = l;
        }
    }
    for n in &mut doc.nodes {
        if let (Some(span), Some((a, l))) = (n.span.as_mut(), span_of(&n.id)) {
            span.start = a;
            span.length = l;
        }
    }
    Some(std::sync::Arc::new(doc))
}

// ------------------------------------------------------------------ the page's hooks

fn column(cx: &mut Cx, ui: &WidgetRef) -> WidgetRef {
    ui.widget(cx, ids!(cut_panel_col))
}

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let col = column(cx, ui);
    let panel_ref = col.widget(cx, ids!(ep_panel)).as_editor_panel();
    if panel_ref.make_clicked(actions) {
        if let Err(error) = make::make_panel(page, cx, Some("")) {
            page.cut_panel.status = error;
        }
    }
}

/// Every pump: the documents follow the cut, the panel is mounted, its
/// edits held and saved, its agent's answers taken.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, shown: bool) {
    let col = column(cx, ui);
    let panel_ref = col.widget(cx, ids!(ep_panel));
    // The fetched texts.
    if let Some(task) = &mut page.cut_panel.fetching {
        if let Some(result) = task.try_take() {
            page.cut_panel.fetching = None;
            match result {
                Ok(Ok(found)) => {
                    for (id, rev, text) in found {
                        page.cut_panel.texts.insert((id, rev), text);
                    }
                    page.cut_panel.built_for = None;
                }
                Ok(Err(error)) => page.cut_panel.fetch_error = Some(error),
                Err(error) => page.cut_panel.fetch_error = Some(format!("{error:?}")),
            }
        }
    }
    // Saves answered (a store of its own: the panel's saves never wait
    // behind the graphic desk's).
    while page.cut_panel.stores.len() < STORES {
        page.cut_panel.stores.push(MotionStore::new(cx));
    }
    for k in 0..STORES {
        while let Some(reply) = page.cut_panel.stores[k].try_recv() {
            if let MotionReply::Published { serial, result, .. } = reply {
                page.cut_panel.saved(serial, result);
            }
        }
    }
    let Some(session) = page.session.as_ref() else {
        if page.cut_panel.built_for.take().is_some() {
            page.cut_panel.runtime.mount(cx, &panel_ref, None);
            page.cut_panel.runtime.unmount();
            page.cut_panel.docs.clear();
        }
        // The chat works without a cut open (it can make one).
        make::pump(page, cx, ui);
        return;
    };
    let (revision, source, doc) = (session.revision, session.source.clone(), session.doc.clone());
    // The documents follow the cut (never under a gesture or a save).
    let busy = page.cut_panel.runtime.holding() || page.cut_panel.commit.is_some() || panel::pressed();
    if page.cut_panel.built_for != Some(revision) && !busy && page.cut_panel.build(cx, &source, &doc) {
        page.cut_panel.built_for = Some(revision);
    }
    if page.cut_panel.docs.is_empty() {
        make::pump(page, cx, ui);
        return;
    }
    // The panel's widgets read and write this cut only while it is the
    // page shown and its panel is there (the graphic desk, the score and
    // this page share one host: never claimed by a page not seen).
    let owner = shown && !super::motion_edit::is_open(page) && panel_ref.visible();
    let claimed = owner && panel::claim(OWNER);
    if claimed {
        panel::install_host(hooks);
    }
    if owner && (claimed || page.cut_panel.dirty_scene) {
        page.cut_panel.dirty_scene = false;
        FEED.with(|f| f.borrow_mut().reach.clear());
        panel::set_docs(page.cut_panel.docs.iter().map(|d| (d.name.clone(), d.text.clone(), d.code.clone())).collect(), page.cut_panel.table.clone());
        panel::changed();
    }
    let body = panel::body_of_let(&page.cut_panel.docs[0].text, &page.cut_panel.docs[0].code, LET);
    let has = body.is_some();
    // The `when:`s, read again with the program (sections, beats, words).
    if page.cut_panel.when_of.as_ref() != body.as_ref() {
        page.cut_panel.when_of = body.clone();
        let timing = page.cut_panel.timing.clone().unwrap_or_default();
        let when: Vec<(Vec<String>, Vec<(f64, f64)>)> = body.as_deref().map(when_records).unwrap_or_default().into_iter()
            .filter_map(|(when, paths)| {
                let spans = timing.when(&when)?;
                Some((paths, spans))
            }).collect();
        FEED.with(|f| {
            let mut f = f.borrow_mut();
            f.when = when;
            f.lit_rev += 1;
        });
    }
    if page.cut_panel.runtime.mount(cx, &panel_ref, body) {
        panel_ref.set_visible(cx, has);
    }
    // The scenes' cards (again when the library's pictures arrive).
    let cards_key = (revision, page.lib.thumbs.len());
    if page.cut_panel.cards_of != Some(cards_key) {
        page.cut_panel.cards_of = Some(cards_key);
        let state = &page.cut_panel;
        let cards = state.table.clips.iter().map(|(clip, _, start, length)| {
            let node = state.table.nodes.iter().find(|n| n.node == *clip);
            let label = node.map_or(clip.clone(), |n| state.docs[n.file].name.clone());
            let thumb = node.and_then(|n| match &state.docs[n.file].of {
                Of::Graphic(a) => page.lib.thumbs.get(&a.id).map(|(_, t)| t.clone()),
                _ => None,
            });
            (clip.clone(), makepad_value_pickers::panel::host::SceneCard { label, start: *start, end: start + length, film: doc.duration, thumb })
        }).collect();
        FEED.with(|f| f.borrow_mut().cards = cards);
        panel_ref.widget(cx, ids!(p_splash)).redraw(cx);
    }
    // The playhead and what acts there.
    let playhead = page.playhead;
    let duration = doc.duration;
    let playing = page.playing.is_some();
    let mut lit: Vec<String> = page.cut_panel.table.clips.iter().filter(|(_, _, s, l)| playhead >= *s && playhead < s + l).map(|(c, ..)| c.clone()).collect();
    // (and the `when:`s acting now)
    lit.extend(FEED.with(|f| f.borrow().when.iter().enumerate().filter(|(_, (_, spans))| spans.iter().any(|(a, b)| playhead >= *a && playhead < *b)).map(|(k, _)| format!("#{k}")).collect::<Vec<_>>()));
    let focus = FEED.with(|f| {
        let mut f = f.borrow_mut();
        f.playhead = (playhead, duration, playing);
        if lit != page.cut_panel.lit.1 {
            f.lit_rev += 1;
        }
        f.focus.clone()
    });
    if lit != page.cut_panel.lit.1 {
        page.cut_panel.lit.1 = lit;
        panel_ref.widget(cx, ids!(p_splash)).redraw(cx);
    }
    // The clips the control under the pointer reaches, shown on the timeline.
    let reach: Vec<String> = focus.unwrap_or_default().iter().flat_map(|p| panel::scene_bounds(p).unwrap_or_default()).flat_map(|b| {
        if b.site.file() == 0 { vec![b.member] } else { FEED.with(|f| f.borrow().users.get(b.site.file()).cloned().unwrap_or_default()) }
    }).collect();
    if reach != page.cut_panel.focus_shown && shown {
        page.cut_panel.focus_shown = reach.clone();
        let also: Vec<String> = page.also.iter().cloned().chain(reach).collect();
        if let Some(mut timeline) = ui.widget(cx, ids!(cut_timeline)).borrow_mut::<super::timeline::CutTimeline>() {
            timeline.set_selected(cx, page.selected.clone(), also);
        }
    }
    // The header's index asked to go to a module.
    if let Some(rect) = makepad_value_pickers::panel::host::take_scroll_to() {
        let scroll = panel_ref.widget(cx, ids!(p_scroll));
        // (The rect is as drawn, before the scroll.)
        let top = scroll.area().rect(cx).pos.y;
        scroll.set_scroll_pos(cx, dvec2(0.0, (rect.pos.y - top - 8.0).max(0.0)));
    }
    // A graphic's own panel, asked for by the program.
    for node in panel::take_opens() {
        if !super::motion_edit::desk::open_graphic(page, &node) {
            page.cut_panel.status = format!("`{node}` is not a graphic of this cut");
        }
    }
    if owner {
        drive_step(page, cx, ui);
    }
    // The program's edits (its own, while it holds the host): held on the
    // texts while a finger is down.
    let splices = if owner {
        let state = &mut page.cut_panel;
        let files: Vec<File> = state.docs.iter().map(|d| File { name: "", text: &d.text, code: &d.code }).collect();
        state.runtime.take(&makepad_stage_motion_pickers::panel_files(&files), &|_, _| None)
    } else {
        Vec::new()
    };
    let splices_seen = page.cut_panel.apply(splices);
    if !splices_seen.is_empty() && live(page) {
        patch_live(page, cx, ui, &splices_seen);
    }
    // The renderer loads the graphics as an editor's while the panel is open.
    let want_live = live(page);
    if page.cut_panel.live_set != Some((page.rendered, want_live)) {
        page.cut_panel.live_set = Some((page.rendered, want_live));
        if let Some(mut preview) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<super::preview::CutPreview>() {
            preview.set_live_literals(want_live);
        }
    }
    if page.cut_panel.runtime.holding() {
        page.cut_panel.runtime.redraw_program(cx, &panel_ref);
    }
    // Released with changes: saved (documents, then the cut at once).
    let released = !page.cut_panel.runtime.holding() && !panel::pressed();
    if !splices_seen.is_empty() {
        page.cut_panel.failed = false;
    }
    reload_due(page, cx, ui);
    if released && page.cut_panel.commit.is_none() && !page.cut_panel.failed && page.cut_panel.unsaved() {
        // The gesture is over: its recolour's places are forgotten.
        page.cut_panel.reload_due.clear();
        if let Some(mut p) = ui.widget(cx, ids!(cut_preview)).borrow_mut::<super::preview::CutPreview>() {
            p.gesture_end();
        }
        page.cut_panel.gestures += 1;
        page.cut_panel.commit = Some(Commit { started: Cx::time_now(), waiting: Vec::new(), saved: Vec::new() });
    }
    let mut ops = Vec::new();
    page.cut_panel.commit_step(&mut ops);
    for op in ops {
        super::motion_edit::note_author(page, "You · film editor");
        page.op(op);
    }
    if page.cut_panel.commit.is_some() {
        cx.new_next_frame();
    }
    make::pump(page, cx, ui);
    // The state line.
    // (A gesture that holds values; a press alone, a click on a heading,
    // holds nothing and says nothing.)
    let held = page.cut_panel.runtime.holding();
    let status = match (&page.cut_panel.commit, &page.cut_panel.fetch_error) {
        (None, None) if held && !page.cut_panel.later.is_empty() => format!("Live in {} places · loading again: {}", page.cut_panel.live_places, page.cut_panel.later.join(", ")),
        (None, None) if held => format!("Live in {} places", page.cut_panel.live_places),
        (Some(c), _) => format!("Saving… {} of {}", c.saved.len(), c.saved.len() + c.waiting.len() + page.cut_panel.docs.iter().filter(|d| d.text != d.loaded).count()),
        (None, Some(error)) => format!("The film's documents did not load: {error}"),
        (None, None) => page.cut_panel.status.clone(),
    };
    if status != page.cut_panel.status_shown {
        page.cut_panel.runtime.status = status.clone();
        page.cut_panel.status_shown = status;
    }
}

// ------------------------------------------------------------------ the control API

/// `cut_panel_*` tools: the panel's state, its paths, a driven gesture.
pub fn tool(page: &mut VideoCut, cx: &mut Cx, name: &str, args: &Json) -> Result<Json, String> {
    let state = &page.cut_panel;
    match name {
        "cut_panel_status" => {
            let body = state.docs.first().and_then(|d| panel::body_of_let(&d.text, &d.code, LET));
            Ok(json::obj(vec![
                ("documents", Json::Int(state.docs.len() as i64)),
                ("graphics", Json::Int(state.docs.iter().filter(|d| matches!(d.of, Of::Graphic(_))).count() as i64)),
                ("modules", Json::Arr(state.table.modules.iter().map(|(n, _)| json::s(n.clone())).collect())),
                ("panel", body.map_or(Json::Null, json::s)),
                ("making", json::s(make::progress(page).map(|p| p.0).unwrap_or_default())),
                ("status", json::s(state.runtime.status.clone())),
                ("saving", state.commit.as_ref().map_or(Json::Bool(false), |c| json::s(format!("{} saving, {} saved", c.waiting.len(), c.saved.len())))),
                ("unsaved", Json::Arr(state.docs.iter().filter(|d| d.text != d.loaded).map(|d| json::s(d.name.clone())).collect())),
                ("gestures", Json::Int(state.gestures as i64)),
                ("live_places", Json::Int(state.live_places as i64)),
                ("reloads", Json::Int(state.reloads as i64)),
                ("patch_ms", Json::F64(state.patch_ms)),
                ("at_release", Json::Arr(state.later.iter().map(|s| json::s(s.clone())).collect())),
                ("last_commit", state.last_commit.as_ref().map_or(Json::Null, |(saved, ms)| json::obj(vec![("saved", Json::Arr(saved.iter().map(|s| json::s(s.clone())).collect())), ("ms", Json::F64(*ms))]))),
                ("revision", Json::Int(page.session.as_ref().map_or(0, |s| s.revision as i64))),
            ]))
        }
        "cut_panel_paths" => {
            let files = state.files();
            Ok(json::obj(vec![("paths", json::s(panel::listing_docs(&makepad_stage_motion_pickers::panel_files(&files), &state.table)))]))
        }
        "cut_panel_read" => {
            let path = args.get("path").and_then(Json::as_str).ok_or("path")?;
            let files = state.files();
            let bounds = makepad_motion::paths::docs::resolve(&files, &state.table, path)?;
            Ok(Json::Arr(bounds.iter().map(|b| json::obj(vec![("member", json::s(b.member.clone())), ("document", json::s(state.docs[b.site.file].name.clone())), ("value", json::s(b.site.text.clone()))])).collect()))
        }
        "cut_panel_make" => {
            let brief = args.get("brief").and_then(Json::as_str).unwrap_or("").to_string();
            make::make_panel(page, cx, Some(&brief))?;
            Ok(json::obj(vec![("making", Json::Bool(true))]))
        }
        "cut_panel_write" => {
            let modules = args.get("modules").and_then(Json::as_str).ok_or("modules")?.to_string();
            let issues = make::write_panel(page, cx, &modules)?;
            Ok(json::obj(vec![("issues", Json::Arr(issues.into_iter().map(json::s).collect()))]))
        }
        // A gesture as a hand would play it: each value a step held live,
        // the last one released (one save, one cut step).
        "cut_panel_gesture" => {
            let path = args.get("path").and_then(Json::as_str).ok_or("path")?.to_string();
            let values: Vec<Json> = match args.get("values") {
                Some(Json::Arr(v)) => v.clone(),
                _ => return Err("values: a list".into()),
            };
            panel::press();
            for v in values {
                // A number is set as it is; text is code (a colour, a call).
                panel::queue(match v {
                    Json::Str(code) => panel::Edit::SetCode { path: path.clone(), code },
                    value => panel::Edit::Set { path: path.clone(), value },
                });
            }
            panel::release();
            Ok(json::obj(vec![("queued", Json::Bool(true))]))
        }
        // A gesture played a step per frame, each timed to the picture.
        "cut_panel_drive" => {
            if args.get("path").is_none() && args.get("recolour").is_none() {
                let Some(drive) = &page.cut_panel.drive else { return Err("no gesture was played".into()) };
                return Ok(json::obj(vec![
                    ("path", json::s(drive.path.clone())),
                    ("done", Json::Bool(drive.released && page.cut_panel.commit.is_none() && !page.cut_panel.runtime.holding())),
                    ("steps", Json::Arr(drive.steps.iter().map(|(v, _, places, ms, patch)| json::obj(vec![("value", json::s(v.clone())), ("live_places", Json::Int(*places as i64)), ("patch_ms", Json::F64(*patch)), ("shown_ms", ms.map_or(Json::Null, Json::F64))])).collect())),
                    ("last_commit", page.cut_panel.last_commit.as_ref().map_or(Json::Null, |(saved, ms)| json::obj(vec![("saved", Json::Int(saved.len() as i64)), ("ms", Json::F64(*ms))]))),
                ]));
            }
            let mut todo = std::collections::VecDeque::new();
            let path = args.get("path").and_then(Json::as_str).unwrap_or("recolour").to_string();
            match (args.get("values"), args.get("recolour")) {
                (Some(Json::Arr(values)), _) => {
                    for v in values {
                        todo.push_back(match v {
                            Json::Str(code) => panel::Edit::SetCode { path: path.clone(), code: code.clone() },
                            v => panel::Edit::Set { path: path.clone(), value: v.clone() },
                        });
                    }
                }
                (_, Some(Json::Arr(maps))) => {
                    for map in maps {
                        let Json::Arr(pairs) = map else { continue };
                        let c = |t: &str| makepad_motion::palette::of(t).first().map(|s| s.rgba);
                        let pairs: Vec<([f64; 4], [f64; 4])> = pairs.iter().filter_map(|p| Some((c(p.get("from")?.as_str()?)?, c(p.get("to")?.as_str()?)?))).collect();
                        todo.push_back(panel::Edit::Recolour { map: pairs });
                    }
                }
                _ => return Err("values: [..] for a path, or recolour: [[{from, to}], ..]".into()),
            }
            page.cut_panel.drive = Some(Drive { path, todo, steps: Vec::new(), released: false });
            cx.new_next_frame();
            Ok(json::obj(vec![("started", Json::Bool(true))]))
        }
        // A palette move over every document: colours (as written) to new ones.
        "cut_panel_recolour" => {
            let pairs: Vec<([f64; 4], [f64; 4])> = match args.get("map") {
                Some(Json::Arr(items)) => items.iter().filter_map(|pair| {
                    let (Some(a), Some(b)) = (pair.get("from").and_then(Json::as_str), pair.get("to").and_then(Json::as_str)) else { return None };
                    let c = |t: &str| makepad_motion::palette::of(t).first().map(|s| s.rgba);
                    Some((c(a)?, c(b)?))
                }).collect(),
                _ => return Err("map: [{from, to}]".into()),
            };
            panel::press();
            panel::queue(panel::Edit::Recolour { map: pairs });
            panel::release();
            Ok(json::obj(vec![("queued", Json::Bool(true))]))
        }
        "cut_panel_palette" => {
            let mut out: Vec<([f64; 4], usize)> = Vec::new();
            for d in &state.docs {
                for found in makepad_motion::palette::of(&d.text) {
                    match out.iter_mut().find(|(c, _)| *c == found.rgba) {
                        Some((_, n)) => *n += found.sites.len(),
                        None => out.push((found.rgba, found.sites.len())),
                    }
                }
            }
            out.sort_by(|a, b| b.1.cmp(&a.1));
            Ok(Json::Arr(out.iter().take(48).map(|(c, n)| json::obj(vec![("colour", json::s(hex(*c))), ("uses", Json::Int(*n as i64))])).collect()))
        }
        _ => Err(format!("no tool `{name}`")),
    }
}
