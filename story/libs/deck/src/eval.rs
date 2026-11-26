//! `DeckDoc::load`: evaluate the file in a VM that holds the cut
//! vocabulary and `mod.deck`, read the records out of it and check them.
//!
//! A slide's `Flow{...}` is an ordinary cut: the VM evaluates it with the
//! rest of the file, and `CutDoc::from_value` finishes and extracts that
//! value in place, so its locations are the deck file's own.

use crate::scan;
use crate::{AppDecl, Transition, DemoStep, Diagnostic, Link, Loop, DeckDoc, Slide, SlideCut, Step, PRELUDE};
use makepad_flow::document::Registry;
use makepad_flow::graph::evaluate_in;
use makepad_script::*;
use makepad_story_app_run::call::call_from_sv;
use makepad_story_app_run::doc::install_path_ids;
use makepad_story_app_run::values::{read, Sv};
use makepad_story_cut::{with_cut_vm, CodeSpec, CutDoc, CutNodeKind};

const FILE: &str = "deck.splash";
const DECK_FIELDS: &[&str] = &["title", "look", "output", "transition", "talk", "slides"];
const SLIDE_FIELDS: &[&str] = &["name", "sketch", "graphic", "app", "duration", "size", "cut", "play", "beats", "enter", "exit", "notes", "apps", "setup", "demo", "target_seconds", "link", "link_slide"];
const APP_FIELDS: &[&str] = &["app", "size", "audio", "args", "env", "dpi", "resolution", "next", "setup", "demo"];
/// Tolerance for a loop range ending at the cut's end.
const EPSILON: f64 = 1e-9;

fn problem(message: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic { line: 1, col: 1, slide: None, message: message.into() }]
}

/// `first_only`: tree graphics after the first slide stay [`SlideCut::Pending`].
pub fn load(source: &str, tree: Option<makepad_story_cut::tree::TreeContext>, first_only: bool) -> Result<DeckDoc, Vec<Diagnostic>> {
    load_reusing(source, tree, first_only, None)
}

/// What a reload may take from the deck loaded before ([`DeckDoc::reload_tree`]).
pub(crate) struct Reuse<'a> {
    prior: &'a DeckDoc,
    changed: &'a [String],
    /// The base names (`title_motion` of `slides/title_motion.splash`) of
    /// the changed files: a graphic whose text names one may read it.
    names: Vec<String>,
}

impl<'a> Reuse<'a> {
    /// None: nothing is taken again. A changed file a graphic may read
    /// without naming it in its own text (a module another module imports,
    /// data, a file that is gone) could change any graphic.
    pub(crate) fn new(prior: &'a DeckDoc, changed: &'a [String], tree: &makepad_story_cut::tree::TreeContext) -> Option<Self> {
        let mut names = Vec::new();
        for path in changed.iter().filter(|path| **path != tree.document) {
            let file = tree.resolve(path).ok()?;
            let document = match file.kind {
                makepad_asset_data::AssetKind::Cut => true,
                makepad_asset_data::AssetKind::Motion => file.text().is_ok_and(|text| !makepad_motion::modules::is_module(text)),
                _ => false,
            };
            if !document { return None; }
            let name = path.rsplit('/').next().unwrap_or(path);
            names.push(name.split('.').next().unwrap_or(name).to_string());
        }
        Some(Self { prior, changed, names })
    }

    /// `path`'s graphic as loaded before, when its file and what it names are unchanged.
    fn graphic(&self, path: &str, output: (u32, u32)) -> Option<std::sync::Arc<CutDoc>> {
        let (was, doc) = self.prior.graphics.get(path)?;
        let unchanged = *was == output && !self.changed.iter().any(|changed| changed == path) && !self.names.iter().any(|name| doc.source.contains(name.as_str()));
        unchanged.then(|| doc.clone())
    }
}

pub(crate) fn load_reusing(source: &str, tree: Option<makepad_story_cut::tree::TreeContext>, first_only: bool, reuse: Option<Reuse>) -> Result<DeckDoc, Vec<Diagnostic>> {
    if let Some(tree) = &tree {
        makepad_story_cut::tree::validate(source, tree).map_err(|diags| diags.into_iter().map(|d| Diagnostic {
            line: d.line, col: d.col, slide: None, message: format!("{}: {}", tree.document, d.message),
        }).collect::<Vec<_>>())?;
    }
    let file = tree.as_ref().map_or(FILE, |tree| tree.document.as_str());
    let evaluated = with_cut_vm(PRELUDE, |vm, registry| {
        install_path_ids(vm);
        let value = match evaluate_in(vm, source, file) {
            Ok(value) => value,
            Err(error) => return Err(vec![Diagnostic { line: error.line, col: error.col, slide: None, message: error.message }]),
        };
        let mut reader = Reader { vm, registry, source, tree: tree.clone(), file, output: (1920, 1080), fps: 60.0, look: None, diags: Vec::new(), first_only, reuse: reuse.as_ref(), graphics: Default::default() };
        let doc = reader.deck(value);
        if reader.diags.is_empty() {
            if let Some(doc) = doc {
                return Ok(doc);
            }
            reader.diags.push(Diagnostic { line: 1, col: 1, slide: None, message: "a deck file's value is one `Deck{...}`".into() });
        }
        reader.diags.sort_by_key(|diag| (diag.line, diag.col));
        Err(reader.diags)
    });
    match evaluated {
        Ok(result) => result,
        Err(error) => Err(problem(error.message)),
    }
}

struct Reader<'a, 'v> {
    vm: &'a mut ScriptVm<'v>,
    registry: &'a Registry,
    source: &'a str,
    tree: Option<makepad_story_cut::tree::TreeContext>,
    file: &'a str,
    output: (u32, u32),
    fps: f64,
    /// The deck's look module, resolved in the tree.
    look: Option<String>,
    diags: Vec<Diagnostic>,
    first_only: bool,
    reuse: Option<&'a Reuse<'a>>,
    /// The tree graphics loaded (or taken again), for [`DeckDoc::graphics`].
    graphics: std::collections::BTreeMap<String, ((u32, u32), std::sync::Arc<CutDoc>)>,
}

/// An object literal's extent in the source: its `{` to just past its `}`.
type Span = (usize, usize);

/// Compact app slides have the same title band and fitted frame as AddSlide.
/// Lower directly to the typed cut; source locations remain in the Slide record.
fn compact_app_cut(app: &str, output: (u32, u32), fps: f64, duration: f64, at: (u32, u32), source: &str) -> CutDoc {
    use makepad_story_cut::{Animated, Blend, CutNode, Fit, OutputFormat, Wire};
    let label = makepad_app_registry::find(app).map_or(app, |entry| entry.label).to_string();
    let loc = makepad_flow::Loc { line: at.0, col: at.1 };
    let wire = |id: &str| Wire { node: id.into(), port: "frame".into() };
    let node = |id: &str, kind| CutNode { id: id.into(), span: None, track: 0, loc: loc.clone(), kind };
    CutDoc {
        source: source.into(), missing_media: Vec::new(), label: label.clone(),
        output: OutputFormat { width: output.0, height: output.1, fps, ..OutputFormat::default() }, duration, bpm: 120.0,
        nodes: vec![
            node("app_frame", CutNodeKind::AppView { app: crate::ops::APP_NAME.into(), fit: Fit::Fit, rect: Some([0.04, 0.18, 0.92, 0.78]) }),
            node("title", CutNodeKind::Title { text: label.clone(), anchor: [0.5, 0.08], font_size: Animated::scalar(72.0), style: String::new() }),
            node("layers", CutNodeKind::Stack { layers: vec![(wire("app_frame"), Blend::Normal), (wire("title"), Blend::Normal)] }),
            node("output", CutNodeKind::Output { frame: Some(wire("layers")), audio: Vec::new() }),
        ],
        output_node: "output".into(),
        graph: makepad_flow::Graph { revision: 0, label, brief: String::new(), trigger: String::new(), concurrency: 1, autostart: false, nodes: Vec::new(), edges: Vec::new(), tools: Vec::new(), flow_ui_src: None, warnings: Vec::new(), components: Vec::new() },
        synthetic_layers: Vec::new(), generates: Vec::new(), graphics: Vec::new(), time_bindings: Vec::new(),
    }
}

impl<'a, 'v> Reader<'a, 'v> {
    fn error(&mut self, at: (u32, u32), slide: Option<&str>, message: impl Into<String>) {
        self.diags.push(Diagnostic { line: at.0, col: at.1, slide: slide.map(str::to_string), message: message.into() });
    }

    fn record(&self, object: ScriptObject) -> Option<String> {
        let value = self.vm.bx.heap_ref().value(object, id!(record).into(), NoTrap);
        value.as_id().and_then(|id| id.as_string(|s| s.map(str::to_string)))
    }

    /// Own fields of an object, in no particular order.
    fn fields(&self, object: ScriptObject) -> Vec<(String, ScriptValue)> {
        let heap = self.vm.heap();
        let mut fields = Vec::new();
        for entry in heap.map_entries(object).iter() {
            fields.push((self.key(entry.key), entry.value));
        }
        for index in 0..heap.vec_len(object) {
            let entry = heap.vec_key_value(object, index, NoTrap);
            if !entry.key.is_nil() {
                fields.push((self.key(entry.key), entry.value));
            }
        }
        fields
    }

    fn key(&self, key: ScriptValue) -> String {
        match read(self.vm, key) {
            Sv::Id(s) | Sv::Str(s) => s,
            other => format!("{other:?}"),
        }
    }

    /// Where an object was written, as its source offset of `{`.
    fn open_brace(&self, object: ScriptObject) -> Option<usize> {
        let loc = self.vm.bx.code_ref().ip_to_loc(self.vm.bx.heap_ref().made_at(object))?;
        let at = scan::offset_of(self.source, loc.line(), loc.col())?;
        scan::next_code_char(self.source, at, b'{')
    }

    fn span(&self, object: ScriptObject) -> Option<Span> {
        let open = self.open_brace(object)?;
        Some((open, scan::matching_brace(self.source, open)? + 1))
    }

    fn at(&self, span: Option<Span>) -> (u32, u32) {
        span.map_or((1, 1), |span| scan::line_col(self.source, span.0))
    }

    fn field_at(&self, span: Option<Span>, name: &str) -> (u32, u32) {
        span.and_then(|span| scan::field_in(self.source, span, name)).unwrap_or_else(|| self.at(span))
    }

    fn deck(&mut self, value: ScriptValue) -> Option<DeckDoc> {
        let object = value.as_object().filter(|object| self.record(*object).as_deref() == Some("deck"))?;
        let span = self.span(object)?;
        let fields = self.fields(object);
        for (name, _) in &fields {
            if !DECK_FIELDS.contains(&name.as_str()) {
                let at = self.field_at(Some(span), name);
                self.error(at, None, format!("`{name}` is not a Deck field (fields: {})", DECK_FIELDS.join(", ")));
            }
        }
        let get = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| *value);
        let title = match get("title").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => String::new(),
            Some(Sv::Str(title)) => title,
            Some(other) => {
                let at = self.field_at(Some(span), "title");
                self.error(at, None, format!("`title` is a string, not {}", other.kind()));
                String::new()
            }
        };
        let look = match get("look").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => None,
            Some(Sv::Str(path)) => {
                let resolved = self.tree.as_ref().ok_or_else(|| "`look` names a module of the deck's tree; this deck has no tree".to_string())
                    .and_then(|tree| tree.resolve(&path))
                    .and_then(|file| if file.kind == makepad_asset_data::AssetKind::Motion { Ok(file.path) } else { Err(format!("{}: a look is a Splash module (`shared/..._look.splash`)", file.path)) });
                match resolved {
                    Ok(path) => Some(path),
                    Err(message) => {
                        let at = self.field_at(Some(span), "look");
                        self.error(at, None, message);
                        None
                    }
                }
            }
            Some(other) => {
                let at = self.field_at(Some(span), "look");
                self.error(at, None, format!("`look` is a tree path string (`\"shared/deck_look\"`), not {}", other.kind()));
                None
            }
        };
        self.look = look.clone();
        let output = self.output(get("output").map(|v| read(self.vm, v)), self.field_at(Some(span), "output"));
        self.output = output;
        if let Some(value) = get("output") {
            if let Some(value) = read(self.vm, value).field("fps") {
                match value {
                    Sv::Num(fps) if fps.is_finite() && *fps > 0.0 => self.fps = *fps,
                    _ => self.error(self.field_at(Some(span), "output"), None, "output.fps must be finite and positive"),
                }
            }
        }
        let transition = match get("transition") {
            None => Transition::default_dissolve(),
            Some(value) => {
                let at = self.field_at(Some(span), "transition");
                self.transition(read(self.vm, value), at, None, "transition").unwrap_or_else(Transition::default_dissolve)
            }
        };
        // The talk's length in seconds, when the speaker set one (else the
        // slides' speaking targets add up to it).
        let talk_seconds = match get("talk").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => None,
            Some(Sv::Num(seconds)) if seconds.is_finite() && seconds >= 1.0 => Some(seconds.round()),
            Some(other) => {
                let at = self.field_at(Some(span), "talk");
                self.error(at, None, format!("`talk` is the talk's length in seconds (at least 1), not {other:?}"));
                None
            }
        };
        let mut slides = Vec::new();
        match get("slides").and_then(|value| value.as_array()) {
            Some(array) => {
                let len = self.vm.heap().array_len(array);
                for index in 0..len {
                    let entry = self.vm.heap().array_index(array, index, NoTrap);
                    let object = entry.as_object().filter(|object| self.record(*object).as_deref() == Some("slide"));
                    match object {
                        Some(object) => {
                            if let Some(slide) = self.slide(object, index) {
                                slides.push(slide);
                            }
                        }
                        None => {
                            let at = self.field_at(Some(span), "slides");
                            self.error(at, None, format!("`slides[{index}]` is not a `Slide{{...}}`"));
                        }
                    }
                }
                if len == 0 {
                    let at = self.field_at(Some(span), "slides");
                    self.error(at, None, "a deck has at least one slide");
                }
            }
            None => {
                let at = self.field_at(Some(span), "slides");
                self.error(at, None, "`slides` is a list of `Slide{...}`");
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        let mut duplicates = Vec::new();
        for slide in &slides {
            if seen.contains(&slide.name.as_str()) {
                duplicates.push((slide.line, slide.name.clone()));
            }
            seen.push(&slide.name);
        }
        for (line, name) in duplicates {
            self.error((line, 1), Some(&name), format!("two slides are named `{name}`"));
        }
        Some(DeckDoc { source: self.source.to_string(), tree: self.tree.clone(), title, look, output, transition, talk_seconds, slides, graphics: std::mem::take(&mut self.graphics) })
    }

    /// A tree graphic's file as a cut: as loaded before when a reload may
    /// take it again ([`Reuse`]), else evaluated now.
    fn graphic(&mut self, file: &makepad_motion::tree::TreeFile, tree: &makepad_story_cut::tree::TreeContext) -> Result<CutDoc, Vec<makepad_story_cut::Diagnostic>> {
        let doc = match self.reuse.and_then(|reuse| reuse.graphic(&file.path, self.output)) {
            Some(doc) => doc,
            None => std::sync::Arc::new(CutDoc::from_graphic(file, tree.clone(), self.output)?),
        };
        self.graphics.insert(file.path.clone(), (self.output, doc.clone()));
        Ok((*doc).clone())
    }

    fn output(&mut self, value: Option<Sv>, at: (u32, u32)) -> (u32, u32) {
        let default = (1920, 1080);
        match value {
            None | Some(Sv::Nil) => default,
            Some(sv @ Sv::Obj(_)) => {
                let dim = |name: &str| match sv.field(name) {
                    Some(Sv::Num(n)) if n.fract() == 0.0 && (1.0..=16384.0).contains(n) => Some(*n as u32),
                    _ => None,
                };
                match (dim("width"), dim("height")) {
                    (Some(w), Some(h)) => (w, h),
                    _ => {
                        self.error(at, None, "`output` is `{width: 1920 height: 1080}`, whole pixels");
                        default
                    }
                }
            }
            Some(other) => {
                self.error(at, None, format!("`output` is `{{width: 1920 height: 1080}}`, not {}", other.kind()));
                default
            }
        }
    }

    fn transition(&mut self, value: Sv, at: (u32, u32), slide: Option<&str>, field: &str) -> Option<Transition> {
        let is_change = matches!(value.field("record"), Some(Sv::Id(kind)) if kind == "transition");
        if !is_change {
            self.error(at, slide, format!("`{field}` is `transition(seconds, effect, dials)`, not {}", value.kind()));
            return None;
        }
        let seconds = match value.field("seconds") {
            Some(Sv::Num(n)) if *n >= 0.0 && n.is_finite() => *n,
            other => {
                self.error(at, slide, format!("a transition's length is seconds >= 0, not {:?}", other));
                return None;
            }
        };
        let effect = match value.field("effect") {
            Some(Sv::Str(effect)) => effect.clone(),
            other => {
                self.error(at, slide, format!("a transition's effect is a transition preset name string, not {:?}", other));
                return None;
            }
        };
        let mut dials = Vec::new();
        match value.field("dials") {
            None | Some(Sv::Nil) => {}
            Some(Sv::Obj(fields)) => {
                for (name, dial) in fields {
                    if !matches!(name.as_str(), "p1" | "p2" | "p3") {
                        self.error(at, slide, format!("`{name}` is not a transition dial (`p1`, `p2`, `p3`)"));
                        return None;
                    }
                    match dial {
                        Sv::Num(n) if n.is_finite() => dials.push((name.clone(), *n)),
                        other => {
                            self.error(at, slide, format!("dial `{name}` is a number, not {}", other.kind()));
                            return None;
                        }
                    }
                }
            }
            Some(other) => {
                self.error(at, slide, format!("a transition's dials are `{{p1: 0.3}}`, not {}", other.kind()));
                return None;
            }
        }
        Some(Transition { seconds, effect, dials })
    }

    fn slide(&mut self, object: ScriptObject, index: usize) -> Option<Slide> {
        let span = self.span(object);
        let origin = self.at(span);
        let fields = self.fields(object);
        let before = self.diags.len();
        for (name, _) in &fields {
            if !SLIDE_FIELDS.contains(&name.as_str()) {
                let at = self.field_at(span, name);
                self.error(at, None, format!("`{name}` is not a Slide field (fields: {})", SLIDE_FIELDS.join(", ")));
            }
        }
        let get = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| *value);
        let name = match get("name").map(|v| read(self.vm, v)) {
            Some(Sv::Id(name)) | Some(Sv::Str(name)) if !name.trim().is_empty() => name,
            _ => {
                self.error(origin, None, format!("slide {index} has a `name` (`@intro`)"));
                format!("slide{index}")
            }
        };
        let slide = Some(name.as_str());
        let compact_graphic = get("graphic").map(|value| read(self.vm, value));
        let compact_app = get("app").map(|value| read(self.vm, value));
        let sketch = match get("sketch").map(|value| read(self.vm, value)) {
            None => None,
            Some(Sv::Str(text)) => Some(text),
            Some(other) => {
                self.error(self.field_at(span, "sketch"), slide, format!("`sketch` is the slide's text (a string: its first line the title, then a line per bullet), not {}", other.kind()));
                None
            }
        };
        let pictures = [get("cut").is_some(), compact_graphic.is_some(), compact_app.is_some()].into_iter().filter(|v| *v).count();
        // A sketch alone is a whole slide: the default layout shows its text.
        if pictures > 1 || (pictures == 0 && get("sketch").is_none()) {
            self.error(origin, slide, "a Slide has exactly one of graphic, app or cut (or only a sketch)");
        }
        let duration = match get("duration").map(|value| read(self.vm, value)) {
            None => None,
            Some(Sv::Num(n)) if n.is_finite() && n > 0.0 => Some(n),
            _ => { self.error(self.field_at(span, "duration"), slide, "duration must be finite seconds > 0"); None }
        };
        if get("cut").is_some() && duration.is_some() {
            self.error(self.field_at(span, "duration"), slide, "a custom cut owns its duration; set it in the Flow");
        }
        let mut graphic = get("cut").and_then(|value| value.as_object()).and_then(|object| {
            let reference = read(self.vm, self.vm.bx.heap_ref().value(object, id!(ref).into(), NoTrap));
            if !matches!(reference, Sv::Id(ref kind) if kind == "graphic") { return None; }
            let Sv::Str(path) = read(self.vm, self.vm.bx.heap_ref().value(object, id!(path).into(), NoTrap)) else { return None };
            self.tree.as_ref()?.resolve(&path).ok().map(|file| file.path)
        });
        // A referenced graphic or library cut is not written in this file: no span to edit.
        let cut_span = get("cut").and_then(|value| value.as_object()).filter(|cut| {
            let reference = read(self.vm, self.vm.bx.heap_ref().value(*cut, id!(ref).into(), NoTrap));
            !matches!(&reference, Sv::Id(kind) if kind == "graphic" || kind == "cut")
        }).and_then(|cut| self.span(cut));
        let mut app_decl = None;
        let cut = if let Some(value) = compact_graphic {
            match value {
                Sv::Str(path) => {
                    let loaded = self.tree.clone().ok_or_else(|| "graphic requires a deck tree".to_string()).and_then(|tree| {
                        let file = tree.resolve(&path)?;
                        graphic = Some(file.path.clone());
                        if self.first_only && index > 0 { return Ok(None); }
                        self.graphic(&file, &tree).map(|doc| Some((doc, file.kind == makepad_asset_data::AssetKind::Motion))).map_err(|diags| diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))
                    });
                    match loaded {
                        Ok(None) => Some(SlideCut::Pending),
                        Ok(Some((mut doc, plain))) => {
                            doc.output.width = self.output.0;
                            doc.output.height = self.output.1;
                            doc.output.fps = self.fps;
                            if let Some(duration) = duration { doc.duration = duration; }
                            if plain { for node in &mut doc.graphics { node.fit = makepad_story_cut::Fit::Fill; } }
                            Some(SlideCut::Inline(Box::new(doc)))
                        }
                        Err(message) => { self.error(self.field_at(span, "graphic"), slide, message); None }
                    }
                }
                _ => { self.error(self.field_at(span, "graphic"), slide, "graphic is a tree path string"); None }
            }
        } else if let Some(value) = compact_app {
            match value {
                Sv::Str(app) if !app.is_empty() => {
                    let size = match get("size").map(|value| read(self.vm, value)) {
                        None => AppDecl::DEFAULT_SIZE,
                        Some(Sv::Arr(items)) if matches!(items.as_slice(), [Sv::Num(w), Sv::Num(h)] if w.is_finite() && h.is_finite() && *w >= 1.0 && *h >= 1.0) => {
                            let [Sv::Num(w), Sv::Num(h)] = items.as_slice() else { unreachable!() }; (*w, *h)
                        }
                        _ => { self.error(self.field_at(span, "size"), slide, "size is [width, height] in points"); AppDecl::DEFAULT_SIZE }
                    };
                    let cut = compact_app_cut(&app, self.output, self.fps, duration.unwrap_or(6.0), origin, self.source);
                    app_decl = Some(AppDecl { name: crate::ops::APP_NAME.into(), app, size, audio: false, args: Vec::new(), env: Vec::new(), dpi: None, resolution: None, next: false, setup: Vec::new(), demo: Vec::new() });
                    Some(SlideCut::Inline(Box::new(cut)))
                }
                _ => { self.error(self.field_at(span, "app"), slide, "app is a registry id string"); None }
            }
        } else if let (None, Some(text)) = (get("cut"), &sketch) {
            if self.first_only && index > 0 && self.tree.is_some() {
                Some(SlideCut::Pending)
            } else {
                match crate::sketch::layout_cut(text, self.tree.as_ref(), self.look.as_deref(), self.output, self.fps) {
                    Ok(mut doc) => {
                        if let Some(duration) = duration { doc.duration = duration; }
                        Some(SlideCut::Inline(Box::new(doc)))
                    }
                    Err(message) => { self.error(self.field_at(span, "sketch"), slide, message); None }
                }
            }
        } else if get("cut").is_none() && get("sketch").is_some() {
            // The sketch's own error is reported above.
            None
        } else { match get("cut").and_then(|value| value.as_object()) {
            Some(cut) => self.cut(cut, &name, span, index),
            None => {
                let at = self.field_at(span, "cut");
                self.error(at, slide, "a slide has a `cut`: a `Flow{...}`, a tree `graphic(path)` or a library `cut(id, rev)`");
                None
            }
        } };
        let looping = match get("play").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => Loop::Whole,
            Some(Sv::Id(id)) if id == "hold" => Loop::Hold,
            Some(Sv::Arr(items)) => match items.as_slice() {
                [Sv::Num(a), Sv::Num(b)] if a.is_finite() && b.is_finite() && *a >= 0.0 && a < b => {
                    if let Some(duration) = cut.as_ref().and_then(|cut| cut.duration()) {
                        if *b > duration + EPSILON {
                            let at = self.field_at(span, "play");
                            self.error(at, slide, format!("`play` ends at {b}, past the cut's {duration} seconds"));
                        }
                    }
                    Loop::Range(*a, *b)
                }
                _ => {
                    let at = self.field_at(span, "play");
                    self.error(at, slide, "`play: [a, b]` takes two seconds with 0 <= a < b");
                    Loop::Whole
                }
            },
            Some(other) => {
                let at = self.field_at(span, "play");
                self.error(at, slide, format!("`play` is `[a, b]` or `@hold`, not {}", other.kind()));
                Loop::Whole
            }
        };
        let mut beats = Vec::new();
        if let Some(value) = get("beats") {
            let at = self.field_at(span, "beats");
            match read(self.vm, value) {
                Sv::Arr(items) => {
                    for (index, item) in items.iter().enumerate() {
                        match item {
                            Sv::Num(t) if t.is_finite() && *t >= 0.0 && beats.last().is_none_or(|previous| t > previous) => {
                                if let Some(duration) = cut.as_ref().and_then(SlideCut::duration) {
                                    if *t >= duration {
                                        self.error(at, slide, format!("`beats[{index}]` is {t}; it must be inside the cut's {duration} seconds"));
                                    }
                                }
                                beats.push(*t);
                            }
                            _ => self.error(at, slide, format!("`beats[{index}]` must be finite seconds >= 0, strictly greater than the preceding beat")),
                        }
                    }
                }
                other => self.error(at, slide, format!("`beats` is a list of increasing seconds, not {}", other.kind())),
            }
        }
        let mut changes = [None, None];
        for (slot, field) in ["enter", "exit"].into_iter().enumerate() {
            if let Some(value) = get(field) {
                let at = self.field_at(span, field);
                changes[slot] = self.transition(read(self.vm, value), at, slide, field);
            }
        }
        let [enter, exit] = changes;
        let notes = match get("notes").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => String::new(),
            Some(Sv::Str(notes)) => notes,
            Some(other) => {
                let at = self.field_at(span, "notes");
                self.error(at, slide, format!("`notes` is a string, not {}", other.kind()));
                String::new()
            }
        };
        let target_seconds = match get("target_seconds").map(|v| read(self.vm, v)) {
            None | Some(Sv::Nil) => None,
            Some(Sv::Num(n)) if n >= 0.0 && n.is_finite() => Some(n),
            Some(other) => {
                let at = self.field_at(span, "target_seconds");
                self.error(at, slide, format!("`target_seconds` is a number of seconds >= 0, not {other:?}"));
                None
            }
        };
        let mut apps = match get("apps") {
            None => Vec::new(),
            Some(value) => {
                let at = self.field_at(span, "apps");
                self.apps(value, at, &name)
            }
        };
        if let Some(app) = app_decl {
            if apps.iter().any(|decl| decl.name == app.name) {
                self.error(self.field_at(span, "apps"), slide, "compact app already declares apps.app");
            } else { apps.insert(0, app); }
        }
        let setup = match get("setup") {
            None => Vec::new(),
            Some(value) => {
                let at = self.field_at(span, "setup");
                self.steps(value, at, &name, "setup")
            }
        };
        let demo = match get("demo") {
            None => Vec::new(),
            Some(value) => {
                let at = self.field_at(span, "demo");
                self.steps(value, at, &name, "demo")
            }
        };
        if !setup.is_empty() && apps.is_empty() {
            let at = self.field_at(span, "setup");
            self.error(at, slide, "`setup` runs on the slide's first app: the slide has no `apps`");
        }
        if let Some(pause) = setup.iter().find(|step| step.step == Step::Pause) {
            self.error((pause.line, 0), slide, "`setup` runs by itself before the first beat: it cannot `pause()`");
        }
        let link = self.link(get("link").map(|value| read(self.vm, value)), get("link_slide").map(|value| read(self.vm, value)), span, &name);
        let cut = cut?;
        if let SlideCut::Inline(doc) = &cut {
            for node in &doc.nodes {
                let (CutNodeKind::AppView { app, .. } | CutNodeKind::Code(CodeSpec { app, .. })) = &node.kind else { continue };
                if !apps.iter().any(|decl| decl.name == *app) {
                    let names: Vec<&str> = apps.iter().map(|decl| decl.name.as_str()).collect();
                    let listed = if names.is_empty() { "the slide has no `apps`".to_string() } else { format!("the slide's apps: {}", names.join(", ")) };
                    self.error((node.loc.line, node.loc.col), slide, format!("`{}.app` names `{app}`, which is not an entry of `apps` ({listed})", node.id));
                }
            }
        }
        let inline = matches!(cut, SlideCut::Inline(_));
        (self.diags.len() == before).then_some(Slide { name, graphic, sketch, cut, looping, beats, enter, exit, notes, apps, setup, demo, target_seconds, link, line: origin.0, span: span.unwrap_or((0, 0)), cut_span: cut_span.filter(|_| inline) })
    }

    /// `link: "path"` names another deck file of this tree; `link_slide: @name`
    /// (only with `link`) the slide it enters. That the slide exists is
    /// checked against the loaded target ([`crate::check_links`]).
    fn link(&mut self, link: Option<Sv>, slide: Option<Sv>, span: Option<Span>, name: &str) -> Option<Link> {
        let at = self.field_at(span, "link");
        let path = match link {
            None | Some(Sv::Nil) => {
                if slide.is_some() { self.error(self.field_at(span, "link_slide"), Some(name), "`link_slide` needs a `link` (the deck file it is in)"); }
                return None;
            }
            Some(Sv::Str(path)) => path,
            Some(other) => {
                self.error(at, Some(name), format!("`link` is a deck file of this tree (\"deck.splash\"), not {}", other.kind()));
                return None;
            }
        };
        let Some(tree) = &self.tree else {
            self.error(at, Some(name), "`link` jumps to another deck of the same tree; this deck is not in a tree");
            return None;
        };
        let file = match tree.resolve(&path) {
            Ok(file) => file,
            Err(message) => { self.error(at, Some(name), message); return None; }
        };
        if file.kind != makepad_asset_data::AssetKind::Deck {
            self.error(at, Some(name), format!("`link` names {}, which is not a deck file (a `Deck{{...}}` document: deck.splash or <name>.deck.splash)", file.path));
            return None;
        }
        if file.path == self.file {
            self.error(at, Some(name), "`link` names this deck itself; use the slides' own order");
            return None;
        }
        let slide = match slide {
            None | Some(Sv::Nil) => None,
            Some(Sv::Id(slide)) | Some(Sv::Str(slide)) if !slide.is_empty() => Some(slide),
            Some(other) => {
                self.error(self.field_at(span, "link_slide"), Some(name), format!("`link_slide` is a slide name (`@intro`), not {}", other.kind()));
                None
            }
        };
        Some(Link { path: file.path, slide })
    }

    fn apps(&mut self, value: ScriptValue, at: (u32, u32), slide: &str) -> Vec<AppDecl> {
        let Some(entries) = value.as_object() else {
            self.error(at, Some(slide), "`apps` is `{name: App{...}}`");
            return Vec::new();
        };
        let mut apps = Vec::new();
        let mut entries = self.fields(entries);
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, app) in entries {
            let Some(app) = app.as_object().filter(|app| self.record(*app).as_deref() == Some("app")) else {
                self.error(at, Some(slide), format!("`apps.{name}` is an `App{{...}}`"));
                continue;
            };
            let raw_fields = self.fields(app);
            let fields: Vec<(String, Sv)> = raw_fields.iter().map(|(key, value)| (key.clone(), read(self.vm, *value))).collect();
            for (field, _) in &fields {
                if !APP_FIELDS.contains(&field.as_str()) {
                    self.error(at, Some(slide), format!("`{field}` is not an App field (fields: {})", APP_FIELDS.join(", ")));
                }
            }
            let get = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
            let id = match get("app") {
                Some(Sv::Str(id)) if !id.is_empty() => id.clone(),
                _ => {
                    self.error(at, Some(slide), format!("`apps.{name}.app` is a registry app id string"));
                    continue;
                }
            };
            let size = match get("size") {
                None | Some(Sv::Nil) => AppDecl::DEFAULT_SIZE,
                Some(Sv::Arr(items)) => match items.as_slice() {
                    [Sv::Num(w), Sv::Num(h)] if *w >= 1.0 && *h >= 1.0 => (*w, *h),
                    _ => {
                        self.error(at, Some(slide), format!("`apps.{name}.size` is `[width, height]` in points"));
                        continue;
                    }
                },
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.size` is `[width, height]`, not {}", other.kind()));
                    continue;
                }
            };
            let audio = match get("audio") {
                None | Some(Sv::Nil) => false,
                Some(Sv::Bool(audio)) => *audio,
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.audio` is true or false, not {}", other.kind()));
                    continue;
                }
            };
            let next = match get("next") {
                None | Some(Sv::Nil) => false,
                Some(Sv::Bool(next)) => *next,
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.next` is true or false, not {}", other.kind()));
                    continue;
                }
            };
            let args = match get("args") {
                None | Some(Sv::Nil) => Vec::new(),
                Some(Sv::Arr(items)) => {
                    let strings: Vec<String> = items.iter().filter_map(|item| if let Sv::Str(text) = item { Some(text.clone()) } else { None }).collect();
                    if strings.len() != items.len() {
                        self.error(at, Some(slide), format!("`apps.{name}.args` is a list of strings"));
                        continue;
                    }
                    strings
                }
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.args` is a list of strings, not {}", other.kind()));
                    continue;
                }
            };
            let env = match get("env") {
                None | Some(Sv::Nil) => Vec::new(),
                Some(Sv::Obj(pairs)) => {
                    let strings: Vec<(String, String)> = pairs.iter().filter_map(|(key, value)| if let Sv::Str(text) = value { Some((key.clone(), text.clone())) } else { None }).collect();
                    if strings.len() != pairs.len() {
                        self.error(at, Some(slide), format!("`apps.{name}.env` is `{{NAME: \"value\"}}`, every value a string"));
                        continue;
                    }
                    strings
                }
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.env` is `{{NAME: \"value\"}}`, not {}", other.kind()));
                    continue;
                }
            };
            let dpi = match get("dpi") {
                None | Some(Sv::Nil) => None,
                Some(Sv::Num(dpi)) if *dpi >= 0.25 && *dpi <= 8.0 => Some(*dpi),
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.dpi` is a number from 0.25 to 8 (pixels per point), not {}", other.kind()));
                    continue;
                }
            };
            let resolution = match get("resolution") {
                None | Some(Sv::Nil) => None,
                Some(Sv::Arr(items)) => match items.as_slice() {
                    [Sv::Num(w), Sv::Num(h)] if *w >= 1.0 && *h >= 1.0 && *w <= 16384.0 && *h <= 16384.0 && w.fract() == 0.0 && h.fract() == 0.0 => Some((*w as u32, *h as u32)),
                    _ => {
                        self.error(at, Some(slide), format!("`apps.{name}.resolution` is `[width, height]` in whole pixels (1 to 16384)"));
                        continue;
                    }
                },
                Some(other) => {
                    self.error(at, Some(slide), format!("`apps.{name}.resolution` is `[width, height]` in pixels, not {}", other.kind()));
                    continue;
                }
            };
            if dpi.is_some() && resolution.is_some() {
                self.error(at, Some(slide), format!("`apps.{name}` sets `dpi` or `resolution`, not both"));
                continue;
            }
            let mut automation = |field: &str| {
                let Some((_, value)) = raw_fields.iter().find(|(key, value)| key == field && !value.is_nil()) else { return Vec::new() };
                self.steps(*value, at, slide, &format!("apps.{name}.{field}"))
            };
            let setup = automation("setup");
            let demo = automation("demo");
            if setup.iter().any(|step| step.step == Step::Pause) {
                self.error(at, Some(slide), format!("`apps.{name}.setup` runs by itself: it cannot `pause()`"));
            }
            apps.push(AppDecl { name, app: id, size, audio, args, env, dpi, resolution, next, setup, demo });
        }
        apps
    }

    /// A list of steps: the slide's `demo`, or its `setup` (`field` names it in messages).
    fn steps(&mut self, value: ScriptValue, at: (u32, u32), slide: &str, field: &str) -> Vec<DemoStep> {
        let Some(array) = value.as_array() else {
            self.error(at, Some(slide), format!("`{field}` is a list of steps (`click(...)`, `wait_text(...)`, ...)"));
            return Vec::new();
        };
        let mut steps = Vec::new();
        let len = self.vm.heap().array_len(array);
        for index in 0..len {
            let entry = self.vm.heap().array_index(array, index, NoTrap);
            let step = read(self.vm, entry);
            let (Some(Sv::Id(kind)), Some(Sv::Id(call))) = (step.field("record"), step.field("call")) else {
                self.error(at, Some(slide), format!("`{field}[{index}]` is a step (`click(...)`, `wait_text(...)`, ... from `mod.deck`)"));
                continue;
            };
            if kind != "step" {
                self.error(at, Some(slide), format!("`{field}[{index}]` is a step, not a `{kind}`"));
                continue;
            }
            if call == "pause" {
                steps.push(DemoStep { step: Step::Pause, line: at.0 });
                continue;
            }
            // The options object joins the record's own fields: the record
            // is then what app_run's `call_from_sv` reads.
            let Sv::Obj(fields) = &step else { continue };
            let mut flat: Vec<(String, Sv)> = fields.iter().filter(|(name, _)| name != "record" && name != "opts").cloned().collect();
            match step.field("opts") {
                None | Some(Sv::Nil) => {}
                Some(Sv::Obj(options)) => flat.extend(options.iter().cloned()),
                Some(other) => {
                    self.error(at, Some(slide), format!("`{field}[{index}]`: options are an object {{...}}, not {}", other.kind()));
                    continue;
                }
            }
            match call_from_sv(&Sv::Obj(flat)) {
                Ok(call) => steps.push(DemoStep { step: Step::Call(call), line: at.0 }),
                Err(message) => self.error(at, Some(slide), format!("`{field}[{index}]` ({call}): {message}")),
            }
        }
        steps
    }

    /// A slide's cut: a tree graphic, a library revision, or a Flow loaded
    /// from its place in the file.
    fn cut(&mut self, object: ScriptObject, slide: &str, slide_span: Option<Span>, index: usize) -> Option<SlideCut> {
        let at = self.field_at(slide_span, "cut");
        let sv_ref = read(self.vm, self.vm.bx.heap_ref().value(object, id!(ref).into(), NoTrap));
        if matches!(&sv_ref, Sv::Id(kind) if kind == "graphic") {
            let path = read(self.vm, self.vm.bx.heap_ref().value(object, id!(path).into(), NoTrap));
            if self.first_only && index > 0 && self.tree.is_some() && matches!(path, Sv::Str(_)) { return Some(SlideCut::Pending); }
            let loaded = match (self.tree.clone(), path) {
                (Some(tree), Sv::Str(path)) => tree.resolve(&path).and_then(|file| {
                    self.graphic(&file, &tree).map_err(|diags| diags.iter().map(|diag| format!("{}:{diag}", file.path)).collect::<Vec<_>>().join("\n"))
                }),
                _ => Err("graphic(path) requires a tree context and a path string".into()),
            };
            return match loaded {
                Ok(doc) => Some(SlideCut::Inline(Box::new(doc))),
                Err(message) => { self.error(at, Some(slide), message); None }
            };
        }
        if matches!(&sv_ref, Sv::Id(kind) if kind == "cut") {
            if self.tree.is_some() {
                self.error(at, Some(slide), "library cut references are forbidden inside a tree; use graphic(path)");
                return None;
            }
            let text = |name: &str| match read(self.vm, self.vm.bx.heap_ref().value(object, LiveId::from_str(name).into(), NoTrap)) {
                Sv::Str(text) => Some(text),
                _ => None,
            };
            return match (text("id"), text("rev")) {
                (Some(id), Some(rev)) => Some(SlideCut::Library { id, rev }),
                _ => {
                    self.error(at, Some(slide), "`cut(id, rev)` takes the library cut's id and revision strings");
                    None
                }
            };
        }
        match CutDoc::from_value_with_tree(self.vm, self.registry, object.into(), self.source, self.file, self.tree.clone()) {
            Ok(doc) => Some(SlideCut::Inline(Box::new(doc))),
            Err(diags) => {
                for diag in diags {
                    self.error((diag.line, diag.col), Some(slide), format!("cut: {}", diag.message));
                }
                None
            }
        }
    }
}
