//! A deck's structural edits as source edits: each takes the file's
//! source and returns the next source and its loaded document. They are the
//! ones the AI's slide tools and the slide sorter share, so a drag, a click
//! and a model's call are the same change and one undo step.
//!
//! Nothing is generated beyond the record the op is about: a new slide's
//! `Slide{...}`, a duplicate's renamed copy, a demo step's call, a cut. Every
//! other byte of the file stays as written, and every result is loaded and
//! checked to say what was asked before it is returned.
//!
//! A slide's *edit* is edited by the edit ops of the app (`add_title`,
//! `add_effect`, ...): [`SlideCut::extract`] hands them the slide as an
//! ordinary edit source and [`Extracted::put_back`] writes what they made
//! into the deck.

use crate::{Transition, Diagnostic, Loop, DeckDoc, Slide, SlideCut};
use makepad_story_cut::{CameraDevice, CameraSpec, Fit};
use makepad_story_cut::source;
use makepad_script::syntax::{self, edit::{self, Replace}, At, Kind, Loc, Node, Syntax};

/// What a new slide shows.
#[derive(Clone, Debug, PartialEq)]
pub enum NewCut {
    Graphic { path: String },
    ImagePath { path: String, fit: ImageFit },
    VideoPath { path: String, play: Option<(f64, f64)>, seconds: Option<f64>, audio: bool },
    /// An empty edit (one picture stack and its output) in the deck's
    /// output size, 6 seconds.
    Blank,
    /// A library cut at an exact revision, played looping (as a compound
    /// clip). `seconds` is its length (10 when unknown).
    Cut { id: String, rev: String, seconds: Option<f64> },
    /// A title over a living background, with an optional subtitle.
    Title { title: String, subtitle: Option<String> },
    /// A heading and bullets that appear one by one on the slide clock, then
    /// hold.
    Text { title: String, bullets: Vec<String> },
    /// A library image, held.
    Image { id: String, rev: String, fit: ImageFit },
    /// A library video looping `play` (the whole video when None). `seconds`
    /// is its length when the whole video plays (10 when unknown). Muted
    /// unless `audio`.
    Video { id: String, rev: String, play: Option<(f64, f64)>, seconds: Option<f64>, audio: bool },
    /// A live app fitted below a title band, its `apps` entry and optional demo.
    App { app: String, size: (f64, f64), demo: Vec<StepSpec> },
    /// A live camera (a `Camera` node): a document camera, a phone under it.
    Camera(CameraSpec),
    /// A sketch: text the person types on the slide (first line the title,
    /// then a line per bullet), shown in the deck's default layout until a
    /// graphic is attached ([`crate::sketch`]).
    Sketch { text: String },
}

/// How a picture sits in the slide.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImageFit {
    /// Cover the slide (crops).
    Fill,
    /// Letterbox: all of it shows.
    Contain,
}

/// The kinds of slide `add_slide` makes.
pub const SLIDE_KINDS: [&str; 9] = ["sketch", "graphic", "blank", "title", "text", "image", "video", "app", "camera"];

/// The fields of a slide to change; `None` leaves a field alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SlideSet {
    pub name: Option<String>,
    pub duration: Option<f64>,
    /// `Loop::Whole` is the default, so it removes the field.
    pub play: Option<Loop>,
    /// An empty list removes the beat holds.
    pub beats: Option<Vec<f64>>,
    /// `Some(None)` removes the slide's own cut.
    pub enter: Option<Option<Transition>>,
    pub exit: Option<Option<Transition>>,
    pub notes: Option<String>,
    pub target_seconds: Option<Option<f64>>,
    /// The slide's sketch text (empty is an empty sketch, still a sketch).
    pub sketch: Option<String>,
    /// `Some(Some(path))` attaches a tree graphic as the slide's picture (a
    /// sketch slide keeps its sketch); `Some(None)` removes it, so a sketch
    /// slide shows its default layout again.
    pub graphic: Option<Option<String>>,
}

/// The fields of a slide's camera to change; `None` leaves a field alone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CameraSet {
    pub device: Option<CameraDevice>,
    /// The capture size and rate: a format the person picked.
    pub size: Option<(u32, u32, f64)>,
    pub rotate: Option<u32>,
    pub mirror: Option<bool>,
}

/// A value of a demo step, as it is written.
#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    Num(f64),
    Str(String),
    /// `@name`: a widget path or a symbol.
    Id(String),
    Bool(bool),
    /// `vec2(x, y)`: a point in the app's window.
    Point(f64, f64),
}

impl Lit {
    fn text(&self) -> String {
        match self {
            Lit::Num(n) => source::number(*n),
            Lit::Str(text) => source::string_literal(text),
            Lit::Id(id) => format!("@{id}"),
            Lit::Bool(b) => b.to_string(),
            Lit::Point(x, y) => format!("vec2({}, {})", source::number(*x), source::number(*y)),
        }
    }
}

/// A demo step: the call (`click`, `wait_text`, ...) and its values by name.
/// The call's own arguments (`target`, `text`, `code`, `from`, `to`, `dx`,
/// `dy`, `seconds`, `name`) are written in order, the rest as its options.
#[derive(Clone, Debug, PartialEq)]
pub struct StepSpec {
    pub call: String,
    pub args: Vec<(String, Lit)>,
}

/// The arguments each step call takes in order (`required`, then ones with
/// a default). The names are the app run script's own.
fn step_params(call: &str) -> Option<(&'static [&'static str], &'static [(&'static str, &'static str)])> {
    Some(match call {
        "click" | "wait_visible" | "wait_state" => (&["target"], &[]),
        "type" | "wait_text" => (&["target", "text"], &[]),
        "key" => (&["code"], &[]),
        "drag" => (&["from", "to"], &[]),
        "scroll" => (&["target"], &[("dx", "0.0"), ("dy", "0.0")]),
        "wait_settled" | "pause" => (&[], &[]),
        "hold" => (&["seconds"], &[]),
        "mark" => (&["name"], &[]),
        _ => return None,
    })
}

pub const STEP_CALLS: [&str; 12] = ["click", "type", "key", "drag", "scroll", "wait_visible", "wait_text", "wait_settled", "wait_state", "hold", "pause", "mark"];

impl StepSpec {
    /// The step as it is written in a `demo:` list.
    pub fn text(&self) -> Result<String, String> {
        let (required, defaults) = step_params(&self.call).ok_or_else(|| format!("`{}` is not a demo step (steps: {})", self.call, STEP_CALLS.join(", ")))?;
        let value = |name: &str| self.args.iter().find(|(key, _)| key == name).map(|(_, lit)| lit.text());
        let mut positional = Vec::new();
        for name in required {
            positional.push(value(name).ok_or_else(|| format!("`{}` needs `{name}`", self.call))?);
        }
        // Optional positionals are written up to the last one given.
        let given: Vec<Option<String>> = defaults.iter().map(|(name, _)| value(name)).collect();
        if let Some(last) = given.iter().rposition(Option::is_some) {
            for (index, (_, default)) in defaults.iter().enumerate().take(last + 1) {
                positional.push(given[index].clone().unwrap_or_else(|| default.to_string()));
            }
        }
        let own: Vec<&str> = required.iter().copied().chain(defaults.iter().map(|(name, _)| *name)).collect();
        let options: Vec<String> = self.args.iter().filter(|(key, _)| !own.contains(&key.as_str())).map(|(key, lit)| format!("{key}: {}", lit.text())).collect();
        if !options.is_empty() {
            if matches!(self.call.as_str(), "hold" | "pause" | "mark") {
                return Err(format!("`{}` takes no options", self.call));
            }
            if self.call == "scroll" {
                while positional.len() < 3 {
                    positional.push("0.0".into());
                }
            }
            positional.push(format!("{{{}}}", options.join(" ")));
        }
        Ok(format!("{}({})", self.call, positional.join(", ")))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeckOp {
    /// A new slide after slide `after` (at the end when None).
    AddSlide { after: Option<usize>, name: String, cut: NewCut },
    /// A new slide before the first one.
    AddSlideFirst { name: String, cut: NewCut },
    MoveSlide { from: usize, to: usize },
    /// All slide names, exactly once, in the wanted order. One tree edit.
    ReorderSlides { slides: Vec<String> },
    DeleteSlide { index: usize },
    /// A copy of the slide after it. An edit written in the slide is copied
    /// with fresh node names; a shared `let` edit stays shared.
    DuplicateSlide { index: usize, name: Option<String> },
    SetSlide { index: usize, set: SlideSet },
    /// A picture on top of slide `index`'s picture, for its whole length: an
    /// image, a video (muted) or a library cut, as `NewCut` says.
    AddLayer { index: usize, layer: NewCut },
    /// Change the fields of the slide's (first) `Camera` node.
    SetCamera { index: usize, set: CameraSet },
    /// The slide's demo steps, as a whole (none removes the demo).
    SetDemo { index: usize, steps: Vec<StepSpec> },
    /// Declare a live app in the slide's `apps`. (Placing it in the slide's
    /// picture is an edit op of the slide: `AppLayer`.)
    AddApp { index: usize, name: String, app: String, size: (f64, f64), audio: bool },
    /// The deck's own fields.
    SetDeck { title: Option<String>, transition: Option<Transition>, output: Option<(u32, u32)> },
    /// Rescale existing speaking targets to this whole number of seconds.
    SetDeckTarget { seconds: f64 },
    /// The talk's length (`talk:` seconds); None: back to the sum of the
    /// slides' speaking targets.
    SetTalk { seconds: Option<f64> },
    /// The whole source, as typed (it loads: checked).
    Source { text: String },
}

/// Every problem of a deck, for a tool's refusal.
pub fn describe(diags: &[Diagnostic]) -> String {
    let mut text = diags.iter().take(3).map(ToString::to_string).collect::<Vec<_>>().join("; ");
    if diags.len() > 3 {
        text.push_str(&format!(" (+{} more)", diags.len() - 3));
    }
    text
}

/// A name a slide or a node is written with: `@name` and `let name`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Apply `op` to `source`: the next source and its loaded document.
pub fn apply(source: &str, op: &DeckOp) -> Result<(String, DeckDoc), String> {
    apply_in(source, op, None)
}

pub fn apply_in(source: &str, op: &DeckOp, tree: Option<makepad_story_cut::tree::TreeContext>) -> Result<(String, DeckDoc), String> {
    let load = |text: &str| crate::eval::load(text, tree.clone(), false).map_err(|diags| describe(&diags));
    let doc = load(source)?;
    let written = match op {
        DeckOp::Source { text } => text.clone(),
        DeckOp::AddSlide { after, name, cut } => add_slide(source, &doc, *after, name, cut)?,
        DeckOp::AddSlideFirst { name, cut } => {
            // After the first slide, then in front of it: one result.
            let (after_first, _) = apply_in(source, &DeckOp::AddSlide { after: Some(0), name: name.clone(), cut: cut.clone() }, tree.clone())?;
            apply_in(&after_first, &DeckOp::MoveSlide { from: 1, to: 0 }, tree.clone())?.0
        }
        DeckOp::MoveSlide { from, to } => move_slide(source, &doc, *from, *to)?,
        DeckOp::ReorderSlides { slides } => reorder_slides(source, &doc, slides)?,
        DeckOp::DeleteSlide { index } => delete_slide(source, &doc, *index)?,
        DeckOp::DuplicateSlide { index, name } => duplicate_slide(source, &doc, *index, name.as_deref())?,
        DeckOp::SetSlide { index, set } => set_slide(source, &doc, *index, set)?,
        DeckOp::AddLayer { index, layer } => add_layer(source, &doc, *index, layer)?,
        DeckOp::SetCamera { index, set } => set_camera(source, &doc, *index, set)?,
        DeckOp::SetDemo { index, steps } => set_demo(source, &doc, *index, steps)?,
        DeckOp::AddApp { index, name, app, size, audio } => add_app(source, &doc, *index, name, app, *size, *audio)?,
        DeckOp::SetDeck { title, transition, output } => set_deck(source, title.as_deref(), transition.as_ref(), *output)?,
        DeckOp::SetDeckTarget { seconds } => set_deck_target(source, &doc, *seconds)?,
        DeckOp::SetTalk { seconds } => set_talk(source, &doc, *seconds)?,
    };
    let next = load(&written).map_err(|error| format!("{op:?} wrote a deck that does not load: {error}"))?;
    prove(op, &doc, &next)?;
    Ok((written, next))
}

/// The result says what was asked.
fn prove(op: &DeckOp, before: &DeckDoc, after: &DeckDoc) -> Result<(), String> {
    let names = |doc: &DeckDoc| doc.slides.iter().map(|slide| slide.name.clone()).collect::<Vec<_>>();
    let (old, new) = (names(before), names(after));
    let wrong = |what: &str| Err(format!("{op:?}: {what}"));
    match op {
        DeckOp::Source { .. } | DeckOp::SetDeck { .. } => {}
        DeckOp::ReorderSlides { slides } => {
            if &new != slides { return wrong("the slide order did not take"); }
        }
        DeckOp::SetTalk { seconds } => {
            if old != new || after.talk_seconds != seconds.map(f64::round) {
                return wrong("the talk's length did not take");
            }
        }
        DeckOp::SetDeckTarget { seconds } => {
            let expected = rescaled_targets(before, *seconds)?;
            if old != new || after.slides.iter().map(|slide| slide.target_seconds).collect::<Vec<_>>() != expected {
                return wrong("the speaking targets did not take");
            }
        }
        DeckOp::AddSlide { after: at, name, .. } => {
            let mut expected = old.clone();
            expected.insert(at.map_or(old.len(), |at| at + 1), name.clone());
            if new != expected {
                return wrong(&format!("the slides are {new:?}, not {expected:?}"));
            }
        }
        DeckOp::AddSlideFirst { name, .. } => {
            let mut expected = old.clone();
            expected.insert(0, name.clone());
            if new != expected {
                return wrong(&format!("the slides are {new:?}, not {expected:?}"));
            }
        }
        DeckOp::MoveSlide { from, to } => {
            let mut expected = old.clone();
            let moved = expected.remove(*from);
            expected.insert(*to, moved);
            if new != expected {
                return wrong(&format!("the slides are {new:?}, not {expected:?}"));
            }
        }
        DeckOp::DeleteSlide { index } => {
            let mut expected = old.clone();
            expected.remove(*index);
            if new != expected {
                return wrong(&format!("the slides are {new:?}, not {expected:?}"));
            }
        }
        DeckOp::DuplicateSlide { index, .. } => {
            if new.len() != old.len() + 1 || new[..=*index] != old[..=*index] {
                return wrong("the copy is not after the slide");
            }
        }
        DeckOp::SetSlide { index, set } => {
            let slide = &after.slides[*index];
            let changed = |ok: bool, field: &str| if ok { Ok(()) } else { Err(format!("{op:?}: `{field}` did not take")) };
            if let Some(name) = &set.name {
                changed(&slide.name == name, "name")?;
            }
            if let Some(looping) = &set.play {
                changed(&slide.looping == looping, "play")?;
            }
            if let Some(beats) = &set.beats {
                changed(&slide.beats == beats, "beats")?;
            }
            if let Some(enter) = &set.enter {
                changed(slide.enter.as_ref() == enter.as_ref(), "enter")?;
            }
            if let Some(exit) = &set.exit {
                changed(slide.exit.as_ref() == exit.as_ref(), "exit")?;
            }
            if let Some(notes) = &set.notes {
                changed(&slide.notes == notes, "notes")?;
            }
            if let Some(target) = &set.target_seconds {
                changed(slide.target_seconds == *target, "target_seconds")?;
            }
            if let Some(sketch) = &set.sketch {
                changed(slide.sketch.as_ref() == Some(sketch), "sketch")?;
            }
            if let Some(graphic) = &set.graphic {
                // The tree resolves the path to its file (`slides/x` is `slides/x.splash`).
                changed(slide.graphic.is_some() == graphic.is_some(), "graphic")?;
            }
        }
        DeckOp::AddLayer { index, .. } => {
            let nodes = |doc: &DeckDoc| match &doc.slides[*index].cut {
                SlideCut::Inline(cut) => cut.nodes.len() + cut.graphics.len(),
                SlideCut::Library { .. } | SlideCut::Pending => 0,
            };
            if nodes(after) < nodes(before) + 2 {
                return wrong("the layer is not in the slide's picture");
            }
        }
        DeckOp::SetCamera { index, set } => {
            let SlideCut::Inline(cut) = &after.slides[*index].cut else { return wrong("the slide plays a library cut") };
            let Some(spec) = cut.nodes.iter().find_map(|node| match &node.kind {
                makepad_story_cut::CutNodeKind::Camera(spec) => Some(spec),
                _ => None,
            }) else {
                return wrong("the slide has no camera");
            };
            let ok = set.device.as_ref().is_none_or(|device| &spec.device == device)
                && set.size.is_none_or(|(w, h, fps)| (spec.width, spec.height, spec.fps) == (w, h, fps))
                && set.rotate.is_none_or(|rotate| spec.rotate == rotate)
                && set.mirror.is_none_or(|mirror| spec.mirror == mirror);
            if !ok {
                return wrong("the camera did not take the change");
            }
        }
        DeckOp::SetDemo { index, steps } => {
            if after.slides[*index].demo.len() != steps.len() {
                return wrong("the demo has other steps than asked");
            }
        }
        DeckOp::AddApp { index, name, .. } => {
            if !after.slides[*index].apps.iter().any(|app| &app.name == name) {
                return wrong("the app is not declared");
            }
        }
    }
    if new.len() != after.slides.len() {
        return wrong("slides lost");
    }
    Ok(())
}

// ---------------------------------------------------------------- layout

fn parse(source: &str) -> Result<Syntax, String> { syntax::parse(source).map_err(|e| e.to_string()) }

fn object_at(tree: &Syntax, open: usize) -> Result<(Loc, &Node), String> {
    let mut found = None;
    tree.walk(&mut |loc, node| {
        if matches!(&node.kind, Kind::Object { open: at, .. } if *at == open) { found = Some((loc.clone(), node)); }
    });
    found.ok_or_else(|| format!("no parsed object at byte {open}"))
}

fn fields(source: &str, object: (usize, usize)) -> Result<Vec<source::Field>, String> {
    let tree = parse(source)?;
    let (_, node) = object_at(&tree, object.0)?;
    Ok(node.object().unwrap().1.iter().map(|f| source::Field { name: f.name.clone(), start: f.span.start, value: (f.value.span.start, f.value.span.end) }).collect())
}

fn constructor(source: &str, id: &str) -> Result<(usize, usize), String> {
    let tree = parse(source)?;
    let loc = tree.find_let(id).ok_or_else(|| format!("no parsed let `{id}`"))?;
    match &tree.node(&loc).unwrap().kind {
        Kind::Let { value: Some(value), .. } => match &value.kind {
            Kind::Object { open, close, .. } => Ok((*open, *close)),
            _ => Err(format!("`{id}` is not an object literal")),
        },
        _ => Err(format!("`{id}` has no value")),
    }
}

fn set_field(source: &str, object: (usize, usize), name: &str, text: &str) -> Result<String, String> {
    let tree = parse(source)?;
    let (loc, _) = object_at(&tree, object.0)?;
    tree.set_field(&loc, name, text).map(|e| e.syntax.print()).map_err(|e| e.to_string())
}

fn slide_location(tree: &Syntax, slide: &Slide) -> Result<(Loc, usize), String> {
    let (loc, _) = object_at(tree, slide.span.0)?;
    let parent = loc.parent().ok_or("the slide is not an array element")?;
    if !matches!(tree.node(&parent).map(|n| &n.kind), Some(Kind::Array { .. })) { return Err("edit this computed slide through doc_patch".into()); }
    Ok((parent, *loc.0.last().unwrap()))
}

/// Where the deck is: the start of its `Deck` word and its
/// `{` to `}`.
fn deck_record(source: &str) -> Result<(usize, (usize, usize)), String> {
    let tree = parse(source)?;
    tree.statements().iter().find_map(|node| match &node.kind {
        Kind::Object { ty: Some(ty), open, close, .. } if ty.rsplit('.').next() == Some("Deck") => Some((node.span.start, (*open, *close))),
        _ => None,
    }).ok_or_else(|| "the file has no top-level Deck object".into())
}

/// The whitespace in front of `at` on its line, when that is all there is.
fn indent_of(source: &str, at: usize) -> String {
    let line = source[..at].rfind('\n').map_or(0, |n| n + 1);
    let lead = &source[line..at];
    if lead.bytes().all(|b| b == b' ' || b == b'\t') {
        lead.to_string()
    } else {
        String::new()
    }
}

/// The start of the `Slide` word (or a path ending in it) before the `{`.
fn record_head(source: &str, open: usize) -> Result<usize, String> {
    let tree = parse(source)?;
    Ok(object_at(&tree, open)?.1.span.start)
}

fn slide_object(slide: &Slide) -> (usize, usize) {
    (slide.span.0, slide.span.1 - 1)
}

fn has_word(source: &str, word: &str) -> bool {
    let Ok(tree) = parse(source) else { return false };
    let mut found = false;
    tree.walk(&mut |_, node| match &node.kind {
        Kind::Ident(name) | Kind::Name(name) => found |= name.split('.').any(|part| part == word),
        Kind::Let { names, .. } => found |= names.iter().any(|name| name == word),
        _ => {}
    });
    found
}

fn slide_at<'a>(doc: &'a DeckDoc, index: usize) -> Result<&'a Slide, String> {
    doc.slides.get(index).ok_or_else(|| format!("there is no slide {index}: the deck has {} (0 to {})", doc.slides.len(), doc.slides.len().saturating_sub(1)))
}

fn check_new_name(doc: &DeckDoc, name: &str, except: Option<usize>) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("`{name}` is not a slide name: letters, digits and `_`, not starting with a digit"));
    }
    if doc.slides.iter().enumerate().any(|(index, slide)| slide.name == name && Some(index) != except) {
        return Err(format!("a slide is named `{name}` already"));
    }
    Ok(())
}

/// The first `<stem><n>` from `from` that no word of `source` or of `taken` is.
fn fresh(source: &str, taken: &[String], stem: &str, from: u32) -> String {
    (from..).map(|n| format!("{stem}{n}")).find(|id| !has_word(source, id) && !taken.contains(id)).unwrap()
}

/// `text` with the words in `map` replaced, outside strings and comments, and
/// not as `@symbols` or members (`a.word`).
fn rename_words(text: &str, map: &[(String, String)]) -> Result<String, String> {
    let tree = parse(text)?;
    let mut edits = Vec::new();
    tree.walk(&mut |_, node| match &node.kind {
        Kind::Ident(name) => {
            let first = name.split('.').next().unwrap_or(name);
            if let Some((_, new)) = map.iter().find(|(old, _)| old == first) {
                edits.push(Replace::new(node.span.clone(), format!("{new}{}", &name[first.len()..])));
            }
        }
        Kind::Let { names, pattern_span, .. } if names.len() == 1 => {
            if let Some((_, new)) = map.iter().find(|(old, _)| old == &names[0]) { edits.push(Replace::new(pattern_span.clone(), new)); }
        }
        _ => {}
    });
    tree.apply(&edits).map(|s| s.print()).map_err(|e| e.to_string())
}

/// The names the slide's `Flow{...}` lists as members (its bare entries).
fn members(source: &str, flow: (usize, usize)) -> Result<Vec<String>, String> {
    Ok(fields(source, (flow.0, flow.1 - 1))?.into_iter().filter(|field| field.name.is_none()).map(|field| source[field.value.0..field.value.1].to_string()).filter(|name| valid_name(name)).collect())
}

/// The `let id = ...` statement of node `id`: from `let` to just past its
/// constructor.
fn let_statement(source: &str, id: &str) -> Result<(usize, usize), String> {
    let tree = parse(source)?;
    let loc = tree.find_let(id).ok_or_else(|| format!("no parsed let `{id}`"))?;
    let span = &tree.node(&loc).unwrap().span;
    Ok((span.start, span.end))
}

// ------------------------------------------------------------------ ops

/// `base`, or the first `<base><n>` from 2 that no word of the file is.
fn free(source: &str, base: &str) -> String {
    if has_word(source, base) {
        fresh(source, &[], base, 2)
    } else {
        base.to_string()
    }
}

/// Node names for a slide's new nodes: none that the file or the op has used.
struct Namer<'a> {
    source: &'a str,
    taken: Vec<String>,
}

impl Namer<'_> {
    fn take(&mut self, base: &str) -> String {
        let id = if has_word(self.source, base) || self.taken.iter().any(|t| t == base) { fresh(self.source, &self.taken, base, 2) } else { base.to_string() };
        self.taken.push(id.clone());
        id
    }
}

/// The nodes of a new slide: their `let`s, the names its flow lists, its
/// length, and the fields beyond `name` and `cut`.
struct Built {
    lets: String,
    members: Vec<String>,
    duration: f64,
    fields: Vec<String>,
}

fn flow_text(output: (u32, u32), built: &Built, indent: &str) -> String {
    format!(
        "Flow{{\n{indent}        kind: @cut\n{indent}        output: {{width: {} height: {} fps: 30}}\n{indent}        duration: {}\n{indent}        {}\n{indent}    }}",
        output.0,
        output.1,
        source::number(built.duration),
        built.members.join(", ")
    )
}

/// `Stack{layers: [conn(1, a.out(@frame)), ...]}`, first at the bottom.
fn stack_of(layers: &[&String]) -> String {
    let layers: Vec<String> = layers.iter().enumerate().map(|(at, id)| format!("conn({}, {id}.out(@frame))", at + 1)).collect();
    format!("Stack{{layers: [{}]}}", layers.join(", "))
}

fn still_ctor(id: &str, rev: &str, fit: ImageFit, length: f64) -> String {
    still_source(&format!("asset({}, {})", source::string_literal(id), source::string_literal(rev)), fit, length)
}

fn still_source(src: &str, fit: ImageFit, length: f64) -> String {
    let fit = match fit {
        ImageFit::Fill => "@fill",
        ImageFit::Contain => "@fit",
    };
    format!("Still{{src: {src} span: [0.0, {}] fit: {fit}}}", source::number(length))
}

/// A video of the library from `from` for `length` seconds.
fn clip_ctor(id: &str, rev: &str, from: f64, length: f64, audio: bool) -> String {
    clip_source(&format!("asset({}, {})", source::string_literal(id), source::string_literal(rev)), from, length, audio)
}

fn clip_source(src: &str, from: f64, length: f64, audio: bool) -> String {
    let from = if from > 0.0 { format!(" in: {}", source::number(from)) } else { String::new() };
    format!("Clip{{src: {src}{from} span: [0.0, {}] fit: @fill audio: {audio}}}", source::number(length))
}

/// A `Camera` node over `length` seconds. Only what differs from the
/// defaults is written beyond the capture size and rate.
fn camera_ctor(spec: &CameraSpec, length: f64) -> Result<String, String> {
    let mut fields = Vec::new();
    match &spec.device {
        CameraDevice::First => {}
        CameraDevice::TestPattern => fields.push("device: @test_pattern".into()),
        CameraDevice::Index(at) => fields.push(format!("device: {at}")),
        CameraDevice::Name(name) if !name.trim().is_empty() => fields.push(format!("device: {}", source::string_literal(name.trim()))),
        CameraDevice::Name(_) => return Err("a camera's `device` is a nonempty name, a number, @test_pattern or nil".into()),
    }
    if spec.width == 0 || spec.height == 0 || !(spec.fps > 0.0 && spec.fps <= 240.0) {
        return Err("a camera's `width`, `height` and `fps` are positive (fps up to 240)".into());
    }
    fields.push(format!("width: {} height: {} fps: {}", spec.width, spec.height, source::number(spec.fps)));
    if spec.crop != [0.0, 0.0, 1.0, 1.0] {
        fields.push(format!("crop: [{}]", spec.crop.iter().map(|v| source::number(*v)).collect::<Vec<_>>().join(", ")));
    }
    if spec.rotate != 0 {
        fields.push(format!("rotate: {}", spec.rotate));
    }
    if spec.mirror {
        fields.push("mirror: true".into());
    }
    if spec.adjust != CameraSpec::default().adjust {
        fields.push(format!(
            "adjust: {{brightness: {} contrast: {} saturation: {}}}",
            source::number(spec.adjust.brightness),
            source::number(spec.adjust.contrast),
            source::number(spec.adjust.saturation)
        ));
    }
    if spec.fit != Fit::Fit {
        fields.push(format!("fit: @{}", spec.fit.id()));
    }
    fields.push(format!("span: [0.0, {}]", source::number(length)));
    Ok(format!("Camera{{{}}}", fields.join(" ")))
}

/// A library cut played as a clip (a compound clip).
fn nested_ctor(id: &str, rev: &str, length: f64) -> String {
    format!("Clip{{src: cut({}, {}) span: [0.0, {}] fit: @fit audio: false}}", source::string_literal(id), source::string_literal(rev), source::number(length))
}

/// The most bullets a text slide lays out.
pub const MAX_BULLETS: usize = 12;

fn build(source: &str, name: &str, cut: &NewCut) -> Result<Built, String> {
    let mut namer = Namer { source, taken: Vec::new() };
    let mut lets = String::new();
    let mut members = Vec::new();
    let mut fields = Vec::new();
    let add = |lets: &mut String, members: &mut Vec<String>, id: &String, constructor: String| {
        lets.push_str(&format!("let {id} = {constructor}\n"));
        members.push(id.clone());
    };
    let duration;
    match cut {
        NewCut::Graphic { .. } => return Err("graphic slides use the compact Slide form".into()),
        NewCut::Blank => {
            duration = 6.0;
            let main = namer.take(&format!("{name}_main"));
            let out = namer.take(&format!("{name}_out"));
            add(&mut lets, &mut members, &main, "Stack{layers: []}".into());
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {main}.out(@frame) audio: []}}"));
        }
        NewCut::Title { title, subtitle } => {
            duration = 8.0;
            if title.trim().is_empty() {
                return Err("a title slide needs a `title`".into());
            }
            let headline = namer.take(&format!("{name}_title"));
            let sub = subtitle.as_ref().filter(|text| !text.trim().is_empty()).map(|_| namer.take(&format!("{name}_sub")));
            let main = namer.take(&format!("{name}_main"));
            let out = namer.take(&format!("{name}_out"));
            let anchor = if sub.is_some() { " anchor: vec2(0.5, 0.42)" } else { "" };
            add(&mut lets, &mut members, &headline, format!(
                "Title{{text: {} span: [0.0, 8.0]{anchor} font_size: keys([0.0, 112.0], [4.0, 124.0, @ease_in_out], [8.0, 112.0, @ease_in_out])}}",
                source::string_literal(title)
            ));
            if let (Some(id), Some(text)) = (&sub, subtitle) {
                add(&mut lets, &mut members, id, format!("Title{{text: {} span: [0.0, 8.0] anchor: vec2(0.5, 0.58) font_size: 52.0}}", source::string_literal(text)));
            }
            let mut layers = vec![&headline];
            layers.extend(sub.as_ref());
            add(&mut lets, &mut members, &main, stack_of(&layers));
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {main}.out(@frame)}}"));
        }
        NewCut::Text { title, bullets } => {
            if title.trim().is_empty() {
                return Err("a text slide needs a `title`".into());
            }
            if bullets.len() > MAX_BULLETS {
                return Err(format!("a text slide has at most {MAX_BULLETS} bullets; split it over slides"));
            }
            if bullets.iter().any(|bullet| bullet.trim().is_empty()) {
                return Err("a bullet is not empty".into());
            }
            // Each bullet appears 0.8 s after the one before, then all hold.
            duration = 0.8 + 0.8 * bullets.len() as f64 + 3.0;
            let headline = namer.take(&format!("{name}_title"));
            add(&mut lets, &mut members, &headline, format!(
                "Title{{text: {} span: [0.0, {}] anchor: vec2(0.5, 0.14) font_size: 84.0}}",
                source::string_literal(title),
                source::number(duration)
            ));
            let mut layers = vec![headline.clone()];
            let step = (0.6 / bullets.len().max(1) as f64).min(0.12);
            for (at, bullet) in bullets.iter().enumerate() {
                let id = namer.take(&format!("{name}_b{}", at + 1));
                let start = 0.8 + 0.8 * at as f64;
                add(&mut lets, &mut members, &id, format!(
                    "Title{{text: {} span: [{}, {}] anchor: vec2(0.5, {}) font_size: keys([0.0, 40.0], [0.35, 56.0, @ease_out])}}",
                    source::string_literal(&format!("\u{2022} {bullet}")),
                    source::number(start),
                    source::number(duration - start),
                    source::number(0.32 + step * at as f64)
                ));
                layers.push(id);
            }
            let main = namer.take(&format!("{name}_main"));
            let out = namer.take(&format!("{name}_out"));
            add(&mut lets, &mut members, &main, stack_of(&layers.iter().collect::<Vec<_>>()));
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {main}.out(@frame)}}"));
            fields.push("play: @hold".to_string());
        }
        NewCut::Image { fit, .. } | NewCut::ImagePath { fit, .. } => {
            duration = 6.0;
            let still = namer.take(&format!("{name}_image"));
            let out = namer.take(&format!("{name}_out"));
            let src = match cut { NewCut::Image { id, rev, .. } => format!("asset({}, {})", source::string_literal(id), source::string_literal(rev)), NewCut::ImagePath { path, .. } => source::string_literal(path), _ => unreachable!() };
            add(&mut lets, &mut members, &still, still_source(&src, *fit, 6.0));
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {still}.out(@frame)}}"));
        }
        NewCut::Video { play, seconds, audio, .. } | NewCut::VideoPath { play, seconds, audio, .. } => {
            let (from, length) = match play {
                Some((a, b)) if a.is_finite() && b.is_finite() && *a >= 0.0 && a < b => (*a, b - a),
                Some(_) => return Err("`play: [a, b]` takes two seconds with 0 <= a < b".into()),
                None => (0.0, seconds.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(10.0)),
            };
            duration = length;
            let clip = namer.take(&format!("{name}_clip"));
            let out = namer.take(&format!("{name}_out"));
            let src = match cut { NewCut::Video { id, rev, .. } => format!("asset({}, {})", source::string_literal(id), source::string_literal(rev)), NewCut::VideoPath { path, .. } => source::string_literal(path), _ => unreachable!() };
            add(&mut lets, &mut members, &clip, clip_source(&src, from, length, *audio));
            let audio_out = if *audio { format!(" audio: [conn(1, {clip}.out(@audio))]") } else { String::new() };
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {clip}.out(@frame){audio_out}}}"));
        }
        NewCut::App { .. } => return Err("app slides use the compact Slide form".into()),
        NewCut::Sketch { .. } => return Err("sketch slides use the compact Slide form".into()),
        NewCut::Camera(spec) => {
            duration = 6.0;
            let camera = namer.take(&format!("{name}_camera"));
            let out = namer.take(&format!("{name}_out"));
            add(&mut lets, &mut members, &camera, camera_ctor(spec, duration)?);
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {camera}.out(@frame)}}"));
        }
        NewCut::Cut { id, rev, seconds } => {
            duration = seconds.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(10.0);
            let clip = namer.take(&format!("{name}_cut"));
            let out = namer.take(&format!("{name}_out"));
            add(&mut lets, &mut members, &clip, nested_ctor(id, rev, duration));
            add(&mut lets, &mut members, &out, format!("CutOutput{{frame: {clip}.out(@frame)}}"));
        }
    }
    Ok(Built { lets, members, duration, fields })
}

/// What a new app slide calls its app.
pub const APP_NAME: &str = "app";

/// The source of a new deck: one title slide with the deck's name and
/// a dissolve between slides.
pub fn new_deck_source(title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("a deck needs a title".into());
    }
    let built = build("", "title", &NewCut::Title { title: title.to_string(), subtitle: None })?;
    let source = format!(
        "use mod.flow.*\nuse mod.cut.*\nuse mod.deck.*\n\n{}\nDeck{{\n    title: {}\n    output: {{width: 1920 height: 1080}}\n    transition: transition(0.6, \"\")\n    slides: [\n        Slide{{\n            name: @title\n            cut: {}\n        }}\n    ]\n}}\n",
        built.lets,
        source::string_literal(title),
        flow_text((1920, 1080), &built, "        ")
    );
    DeckDoc::load(&source).map_err(|diags| format!("the new deck does not load: {}", describe(&diags)))?;
    Ok(source)
}

fn add_slide(source: &str, doc: &DeckDoc, after: Option<usize>, name: &str, cut: &NewCut) -> Result<String, String> {
    check_new_name(doc, name, None)?;
    let at = match after {
        Some(index) => slide_at(doc, index)?,
        None => doc.slides.last().ok_or("a deck has at least one slide")?,
    };
    let (deck_start, _) = deck_record(source)?;
    let indent = indent_of(source, record_head(source, at.span.0)?);
    let (lets, fields) = match cut {
        NewCut::Graphic { path } => {
            let tree = doc.tree.as_ref().ok_or("a graphic slide requires a deck tree")?;
            let file = tree.resolve(path)?;
            let cut = makepad_story_cut::CutDoc::from_graphic(&file, tree.clone(), doc.output).map_err(|diags| diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))?;
            (String::new(), vec![format!("graphic: {}", source::string_literal(path)), format!("duration: {}", source::number(cut.duration))])
        }
        NewCut::Sketch { text } => (String::new(), vec![format!("sketch: {}", source::string_literal(text))]),
        NewCut::App { app, size, demo } => {
            if app.is_empty() || !size.0.is_finite() || !size.1.is_finite() || size.0 < 1.0 || size.1 < 1.0 {
                return Err("app needs a registry id and a positive window size".into());
            }
            let mut fields = vec![format!("app: {}", source::string_literal(app)), "duration: 6.0".into()];
            if *size != crate::AppDecl::DEFAULT_SIZE { fields.push(format!("size: [{}, {}]", source::number(size.0), source::number(size.1))); }
            if !demo.is_empty() { fields.push(format!("demo: [{}]", demo.iter().map(StepSpec::text).collect::<Result<Vec<_>, _>>()?.join(", "))); }
            (String::new(), fields)
        }
        _ => {
            let built = build(source, name, cut)?;
            let mut fields = vec![format!("cut: {}", flow_text(doc.output, &built, &indent))];
            fields.extend(built.fields);
            (built.lets, fields)
        }
    };
    let extra: String = fields.iter().map(|field| format!("\n{indent}    {}", field.replace('\n', &format!("\n{indent}    ")))).collect();
    let slide = format!("Slide{{\n{indent}    name: @{name}{extra}\n{indent}}}");
    let tree = parse(source)?;
    let (list, index) = slide_location(&tree, at)?;
    let mut edits = edit::insert_element_edits(source, tree.node(&list).unwrap(), At::After(index), &slide).map_err(|e| e.to_string())?;
    if !lets.is_empty() { edits.push(Replace::new(deck_start..deck_start, format!("{lets}\n"))); }
    tree.apply(&edits).map(|s| s.print()).map_err(|e| e.to_string())
}

/// A layer on top of a slide's picture: its node, a `Stack` of the picture
/// that was the slide's output and the layer, and the output pointed at the
/// stack. Everything else in the file stays as it was.
fn add_layer(source: &str, doc: &DeckDoc, index: usize, layer: &NewCut) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    let SlideCut::Inline(cut) = &slide.cut else {
        return Err(format!("slide {index} (`{}`) plays a library cut; open that cut in CUT to change it, or add a slide", slide.name));
    };
    let flow = slide.cut_span.ok_or_else(|| format!("slide {index}'s cut is not written in the file; edit its source"))?;
    let duration = cut.duration;
    let mut namer = Namer { source, taken: Vec::new() };
    let node = namer.take(&format!("{}_layer", slide.name));
    let stack = namer.take(&format!("{}_layers", slide.name));
    let layer_source = match layer {
        NewCut::Graphic { path } => format!("Graphic{{src: graphic({}) span: [0.0, {}] fit: @fit}}", source::string_literal(path), source::number(duration)),
        NewCut::ImagePath { path, fit } => still_source(&source::string_literal(path), *fit, duration),
        NewCut::VideoPath { path, play, .. } => {
            let from = match play {
                Some((a, b)) if a.is_finite() && b.is_finite() && *a >= 0.0 && a < b => *a,
                Some(_) => return Err("`play: [a, b]` takes two seconds with 0 <= a < b".into()),
                None => 0.0,
            };
            clip_source(&source::string_literal(path), from, duration, false)
        }
        NewCut::Image { id, rev, fit } => still_ctor(id, rev, *fit, duration),
        NewCut::Video { id, rev, play, audio: _, .. } => {
            let from = match play {
                Some((a, b)) if a.is_finite() && b.is_finite() && *a >= 0.0 && a < b => *a,
                Some(_) => return Err("`play: [a, b]` takes two seconds with 0 <= a < b".into()),
                None => 0.0,
            };
            // A layer is picture only: a drop never adds sound to a slide.
            clip_ctor(id, rev, from, duration, false)
        }
        NewCut::Cut { id, rev, .. } => nested_ctor(id, rev, duration),
        NewCut::Camera(spec) => camera_ctor(spec, duration)?,
        _ => return Err("a layer is an image, a video, a library cut or a camera".into()),
    };
    // The output's picture is replaced by a stack of it and the layer.
    let (out_open, out_close) = constructor(source, &cut.output_node).map_err(|_| format!("slide {index}'s output is not a plain `let {} = CutOutput{{...}}`; edit its source", cut.output_node))?;
    let frame = fields(source, (out_open, out_close))?.into_iter().find(|field| field.name.as_deref() == Some("frame")).ok_or("the slide's output has no `frame`")?;
    let below = source[frame.value.0..frame.value.1].to_string();
    // The new nodes go just before the output's `let`, which names them.
    let out_let = let_statement(source, &cut.output_node)?.0;
    let lets = format!("let {node} = {layer_source}\nlet {stack} = Stack{{layers: [conn(1, {below}), conn(2, {node}.out(@frame))]}}\n");
    let tree = parse(source)?;
    let (_, flow_node) = object_at(&tree, flow.0)?;
    let mut edits = edit::insert_element_edits(source, flow_node, At::End, &format!("{node}, {stack}")).map_err(|e| e.to_string())?;
    edits.push(Replace::new(frame.value.0..frame.value.1, format!("{stack}.out(@frame)")));
    edits.push(Replace::new(out_let..out_let, lets));
    tree.apply(&edits).map(|s| s.print()).map_err(|e| e.to_string())
}

/// Change fields of the slide's first `Camera` node, written as a `let`.
fn set_camera(source: &str, doc: &DeckDoc, index: usize, set: &CameraSet) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    let SlideCut::Inline(cut) = &slide.cut else {
        return Err(format!("slide {index} (`{}`) plays a library cut; open that cut in CUT to change it", slide.name));
    };
    let id = cut
        .nodes
        .iter()
        .find_map(|node| matches!(node.kind, makepad_story_cut::CutNodeKind::Camera(_)).then(|| node.id.clone()))
        .ok_or_else(|| format!("slide {index} (`{}`) has no camera: add one with add_layer or a camera slide", slide.name))?;
    let mut values: Vec<(&str, String)> = Vec::new();
    if let Some(device) = &set.device {
        values.push((
            "device",
            match device {
                CameraDevice::First => "nil".into(),
                CameraDevice::TestPattern => "@test_pattern".into(),
                CameraDevice::Index(at) => at.to_string(),
                CameraDevice::Name(name) if !name.trim().is_empty() => source::string_literal(name.trim()),
                CameraDevice::Name(_) => return Err("a camera's `device` is a nonempty name, a number, @test_pattern or nil".into()),
            },
        ));
    }
    if let Some((width, height, fps)) = set.size {
        if width == 0 || height == 0 || !(fps > 0.0 && fps <= 240.0) {
            return Err("a camera's `width`, `height` and `fps` are positive (fps up to 240)".into());
        }
        values.push(("width", width.to_string()));
        values.push(("height", height.to_string()));
        values.push(("fps", source::number(fps)));
    }
    if let Some(rotate) = set.rotate {
        if ![0, 90, 180, 270].contains(&rotate) {
            return Err("`rotate` is 0, 90, 180 or 270 (degrees clockwise)".into());
        }
        values.push(("rotate", rotate.to_string()));
    }
    if let Some(mirror) = set.mirror {
        values.push(("mirror", mirror.to_string()));
    }
    let mut written = source.to_string();
    for (field, value) in values {
        let object = constructor(&written, &id).map_err(|_| format!("`{id}` is not a plain `let {id} = Camera{{...}}`; edit its source"))?;
        written = set_field(&written, object, field, &value)?;
    }
    Ok(written)
}

fn move_slide(source: &str, doc: &DeckDoc, from: usize, to: usize) -> Result<String, String> {
    let tree = parse(source)?;
    let (list, from) = slide_location(&tree, slide_at(doc, from)?)?;
    let (dest, to) = slide_location(&tree, slide_at(doc, to)?)?;
    if list != dest { return Err("the slides belong to different arrays; use doc_patch".into()); }
    tree.move_element(&list, from, to).map(|e| e.syntax.print()).map_err(|e| e.to_string())
}

/// Map a gap in the original list (0..=count) to the moved row's final index.
/// The gaps immediately above and below the row both mean no change.
pub fn slide_drop_index(count: usize, from: usize, boundary: usize) -> Option<usize> {
    if from >= count || boundary > count { return None; }
    let to = boundary - usize::from(boundary > from);
    (to != from).then_some(to)
}

fn reorder_slides(source: &str, doc: &DeckDoc, order: &[String]) -> Result<String, String> {
    let mut names: Vec<_> = doc.slides.iter().map(|slide| slide.name.clone()).collect();
    let expected: std::collections::HashSet<_> = names.iter().collect();
    let wanted: std::collections::HashSet<_> = order.iter().collect();
    if order.len() != names.len() || wanted.len() != order.len() || wanted != expected {
        return Err("the order must contain every slide name exactly once".into());
    }
    if names == order { return Ok(source.to_string()); }
    let mut tree = parse(source)?;
    let (list, _) = slide_location(&tree, &doc.slides[0])?;
    for (index, slide) in doc.slides.iter().enumerate() {
        if slide_location(&tree, slide)? != (list.clone(), index) {
            return Err("edit computed slide arrays through doc_patch".into());
        }
    }
    // Syntax moves carry each slide's comments and separators with it. Only
    // the final source is loaded and installed, regardless of permutation size.
    for (to, name) in order.iter().enumerate() {
        let from = names.iter().position(|value| value == name).unwrap();
        if from != to {
            tree = tree.move_element(&list, from, to).map_err(|e| e.to_string())?.syntax;
            let moved = names.remove(from);
            names.insert(to, moved);
        }
    }
    Ok(tree.print())
}

fn rescaled_targets(doc: &DeckDoc, seconds: f64) -> Result<Vec<Option<f64>>, String> {
    if !seconds.is_finite() || seconds < 1.0 || seconds.fract() != 0.0 {
        return Err("deck target is a positive whole number of seconds".into());
    }
    let longest = doc.slides.iter().enumerate().filter_map(|(index, slide)| slide.target_seconds.map(|target| (index, target)))
        .reduce(|a, b| if b.1 > a.1 { b } else { a }).ok_or("set a speaking target on at least one slide first")?;
    if longest.1 == 0.0 { return Err("set a positive speaking target on at least one slide first".into()); }
    let total: f64 = doc.slides.iter().filter_map(|slide| slide.target_seconds).sum();
    let factor = seconds / total;
    // Scale once, so whole-second totals keep exact half-second rounding.
    // Normalise only when extreme finite inputs would overflow/underflow.
    let ordinary = total.is_finite() && factor.is_finite() && factor > 0.0;
    let normalised: f64 = if ordinary { 1.0 } else {
        doc.slides.iter().filter_map(|slide| slide.target_seconds).map(|target| target / longest.1).sum()
    };
    let exact: Vec<_> = doc.slides.iter().map(|slide| slide.target_seconds.map(|target|
        if ordinary { target * factor } else { (target / longest.1 / normalised) * seconds }
    )).collect();
    let mut rounded: Vec<_> = exact.iter().map(|target| target.map(f64::round)).collect();
    let remainder = seconds - rounded.iter().flatten().sum::<f64>();
    rounded[longest.0] = Some(rounded[longest.0].unwrap() + remainder);
    // For a tiny total, many half-second shares can round up past even the
    // longest share. Reclaim that excess from rounded-up shares in deck order
    // rather than writing a negative timer or refusing a shorter duration.
    let mut excess = (-rounded[longest.0].unwrap()).max(0.0);
    if excess > 0.0 {
        rounded[longest.0] = Some(0.0);
        for (index, target) in rounded.iter_mut().enumerate() {
            if excess == 0.0 { break; }
            if let Some(target) = target {
                if *target > exact[index].unwrap() {
                    *target -= 1.0;
                    excess -= 1.0;
                }
            }
        }
    }
    Ok(rounded)
}

fn set_deck_target(source: &str, doc: &DeckDoc, seconds: f64) -> Result<String, String> {
    let targets = rescaled_targets(doc, seconds)?;
    let tree = parse(source)?;
    let mut edits = Vec::new();
    for (slide, target) in doc.slides.iter().zip(targets) {
        let Some(target) = target else { continue };
        if slide.target_seconds == Some(target) { continue; }
        let (_, node) = object_at(&tree, slide.span.0)?;
        if let Some(field) = node.field("target_seconds") {
            edits.push(Replace::new(field.value.span.clone(), source::number(target)));
        } else {
            edits.extend(edit::add_field_edits(source, node, At::End, "target_seconds", &source::number(target)).map_err(|error| error.to_string())?);
        }
    }
    tree.apply(&edits).map(|tree| tree.print()).map_err(|e| e.to_string())
}

/// Remove the `let` statements of `candidates` that nothing else names, and
/// those they leave unnamed in turn.
fn drop_orphan_lets(mut text: String, mut candidates: Vec<String>) -> Result<String, String> {
    loop {
        let mut removed = false;
        let mut kept = Vec::new();
        for id in candidates {
            let tree = parse(&text)?;
            let Some(loc) = tree.find_let(&id) else { continue };
            let span = &tree.node(&loc).unwrap().span;
            let outside = format!("{}{}", &text[..span.start], &text[span.end..]);
            if has_word(&outside, &id) { kept.push(id); continue; }
            text = tree.delete_statement(&loc).map_err(|e| e.to_string())?.syntax.print();
            removed = true;
        }
        if !removed { return Ok(text); }
        candidates = kept;
    }
}

fn delete_slide(source: &str, doc: &DeckDoc, index: usize) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    if doc.slides.len() == 1 {
        return Err("a deck has at least one slide: this is the only one".into());
    }
    let mut orphans = slide.cut_span.map(|flow| members(source, flow)).transpose()?.unwrap_or_default();
    // `cut: name` lists the flow's `let` too.
    if let Some(field) = fields(source, slide_object(slide))?.into_iter().find(|f| f.name.as_deref() == Some("cut")) {
        let value = &source[field.value.0..field.value.1];
        if valid_name(value) {
            orphans.push(value.to_string());
        }
    }
    let tree = parse(source)?;
    let (list, at) = slide_location(&tree, slide)?;
    let written = tree.remove_element(&list, at).map_err(|e| e.to_string())?.syntax.print();
    drop_orphan_lets(written, orphans)
}

fn duplicate_slide(source: &str, doc: &DeckDoc, index: usize, name: Option<&str>) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    let new_name = match name {
        Some(name) => name.to_string(),
        None => free(source, &format!("{}_copy", slide.name)),
    };
    check_new_name(doc, &new_name, None)?;
    let (deck_start, _) = deck_record(source)?;
    let head = record_head(source, slide.span.0)?;
    let mut text = source[head..slide.span.1].to_string();
    let mut lets = String::new();
    // An edit written inside the slide is copied with fresh node names: a
    // later edit of the copy must not change the original.
    let inside = slide.cut_span.filter(|flow| flow.0 >= slide.span.0 && flow.1 <= slide.span.1);
    if let Some(flow) = inside {
        let ids = members(source, flow)?;
        let mut map: Vec<(String, String)> = Vec::new();
        let mut taken: Vec<String> = Vec::new();
        for id in &ids {
            let stem = id.trim_end_matches(|c: char| c.is_ascii_digit());
            let new = fresh(source, &taken, if stem.is_empty() { id } else { stem }, 2);
            taken.push(new.clone());
            map.push((id.clone(), new));
        }
        for id in &ids {
            let (start, end) = let_statement(source, id)?;
            lets.push_str(&rename_words(&source[start..end], &map)?);
            lets.push('\n');
        }
        text = rename_words(&text, &map)?;
    }
    // The copy's name.
    let copied = parse(&text)?;
    let open = match &copied.statements().first().ok_or("missing copied slide")?.kind { Kind::Object { open, .. } => *open, _ => return Err("the slide is not an object".into()) };
    text = set_field(&text, (open, 0), "name", &format!("@{new_name}"))?;
    let tree = parse(source)?;
    let (list, at) = slide_location(&tree, slide)?;
    let mut edits = edit::insert_element_edits(source, tree.node(&list).unwrap(), At::After(at), &text).map_err(|e| e.to_string())?;
    if !lets.is_empty() { edits.push(Replace::new(deck_start..deck_start, format!("{lets}\n"))); }
    tree.apply(&edits).map(|s| s.print()).map_err(|e| e.to_string())
}

/// Remove `field` of the object, with its line when it is alone on it.
fn remove_field(source: &str, object: (usize, usize), field: &str) -> Result<String, String> {
    let tree = parse(source)?;
    let (loc, node) = object_at(&tree, object.0)?;
    if node.field(field).is_none() { return Ok(source.into()); }
    tree.remove_field(&loc, field).map(|e| e.syntax.print()).map_err(|e| e.to_string())
}

fn transition_text(cut: &Transition) -> String {
    let dials: Vec<String> = cut.dials.iter().map(|(name, value)| format!("{name}: {}", source::number(*value))).collect();
    if dials.is_empty() {
        format!("transition({}, {})", source::number(cut.seconds), source::string_literal(&cut.effect))
    } else {
        format!("transition({}, {}, {{{}}})", source::number(cut.seconds), source::string_literal(&cut.effect), dials.join(" "))
    }
}

/// Set or remove a field of an object whose extent moves with the edit.
fn put_field(source: &str, object: (usize, usize), field: &str, value: Option<String>) -> Result<String, String> {
    match value {
        Some(value) => set_field(source, object, field, &value),
        None => remove_field(source, object, field),
    }
}

fn set_slide(source: &str, doc: &DeckDoc, index: usize, set: &SlideSet) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    if let Some(name) = &set.name {
        check_new_name(doc, name, Some(index))?;
    }
    if let Some(Loop::Range(a, b)) = &set.play {
        if !(a.is_finite() && b.is_finite() && *a >= 0.0 && a < b) {
            return Err("`play: [a, b]` takes two seconds with 0 <= a < b".into());
        }
    }
    if let Some(beats) = &set.beats {
        if beats.iter().any(|t| !t.is_finite() || *t < 0.0 || set.duration.or_else(|| slide.cut.duration()).is_some_and(|duration| *t >= duration))
            || beats.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err("`beats` takes increasing finite seconds >= 0, inside the cut".into());
        }
    }
    if let Some(duration) = set.duration {
        if !duration.is_finite() || duration <= 0.0 { return Err("duration is positive finite seconds".into()); }
        if slide.cut_span.is_some() { return Err("a custom cut owns its duration; edit its Flow".into()); }
        if set.beats.as_ref().unwrap_or(&slide.beats).iter().any(|beat| *beat >= duration) { return Err("all beats must precede the duration".into()); }
    }
    let open = slide.span.0;
    let mut written = source.to_string();
    // The slide's `{` stays where it is as its fields are written.
    let mut write = |field: &str, value: Option<String>| -> Result<(), String> {
        written = put_field(&written, (open, 0), field, value)?;
        Ok(())
    };
    if let Some(duration) = set.duration { write("duration", Some(source::number(duration)))?; }
    if let Some(name) = &set.name {
        write("name", Some(format!("@{name}")))?;
    }
    if let Some(looping) = &set.play {
        write("play", match looping {
            Loop::Whole => None,
            Loop::Hold => Some("@hold".into()),
            Loop::Range(a, b) => Some(format!("[{}, {}]", source::number(*a), source::number(*b))),
        })?;
    }
    if let Some(beats) = &set.beats {
        write("beats", (!beats.is_empty()).then(|| format!("[{}]", beats.iter().map(|t| source::number(*t)).collect::<Vec<_>>().join(", "))))?;
    }
    if let Some(enter) = &set.enter {
        write("enter", enter.as_ref().map(transition_text))?;
    }
    if let Some(exit) = &set.exit {
        write("exit", exit.as_ref().map(transition_text))?;
    }
    if let Some(notes) = &set.notes {
        write("notes", if notes.is_empty() { None } else { Some(source::string_literal(notes)) })?;
    }
    if let Some(target) = &set.target_seconds {
        write("target_seconds", target.map(source::number))?;
    }
    if let Some(sketch) = &set.sketch {
        write("sketch", Some(source::string_literal(sketch)))?;
    }
    if let Some(graphic) = &set.graphic {
        if graphic.is_none() && slide.sketch.is_none() {
            return Err(format!("slide {index} (`{}`) has no sketch: without its graphic it would show nothing", slide.name));
        }
        write("graphic", graphic.as_ref().map(|path| source::string_literal(path)))?;
    }
    Ok(written)
}

fn set_demo(source: &str, doc: &DeckDoc, index: usize, steps: &[StepSpec]) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    let object = slide_object(slide);
    if steps.is_empty() {
        return remove_field(source, object, "demo");
    }
    let texts = steps.iter().map(StepSpec::text).collect::<Result<Vec<_>, _>>()?;
    let multiline = source[object.0..object.1].contains('\n');
    let value = if multiline {
        let indent = fields(source, object)?.first().map(|f| indent_of(source, f.start)).filter(|i| !i.is_empty()).unwrap_or_else(|| "            ".into());
        let inner = format!("{indent}    ");
        format!("[\n{}\n{indent}]", texts.iter().map(|t| format!("{inner}{t}")).collect::<Vec<_>>().join(",\n"))
    } else {
        format!("[{}]", texts.join(", "))
    };
    set_field(source, object, "demo", &value)
}

fn add_app(source: &str, doc: &DeckDoc, index: usize, name: &str, app: &str, size: (f64, f64), audio: bool) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    if !valid_name(name) {
        return Err(format!("`{name}` is not an app name: letters, digits and `_`"));
    }
    if app.is_empty() {
        return Err("`app` is a registry app id".into());
    }
    if !(size.0 >= 1.0 && size.1 >= 1.0) {
        return Err("`size` is `[width, height]` in points".into());
    }
    if slide.apps.iter().any(|decl| decl.name == name) {
        return Err(format!("the slide declares an app `{name}` already"));
    }
    let decl = format!("App{{app: {} size: [{}, {}] audio: {audio}}}", source::string_literal(app), source::number(size.0), source::number(size.1));
    let object = slide_object(slide);
    match fields(source, object)?.into_iter().find(|f| f.name.as_deref() == Some("apps")) {
        Some(field) => {
            let (open, text) = (field.value.0, &source[field.value.0..field.value.1]);
            if !text.starts_with('{') {
                return Err("the slide's `apps` is not a literal `{...}`; edit its source".into());
            }
            set_field(source, (open, 0), name, &decl)
        }
        None => set_field(source, object, "apps", &format!("{{{name}: {decl}}}")),
    }
}

fn set_talk(source: &str, doc: &DeckDoc, seconds: Option<f64>) -> Result<String, String> {
    let (_, object) = deck_record(source)?;
    let tree = parse(source)?;
    let (loc, _) = object_at(&tree, object.0)?;
    match seconds {
        Some(seconds) if seconds.is_finite() && seconds.round() >= 1.0 => tree.set_field(&loc, "talk", &format!("{}", seconds.round() as u64)),
        Some(_) => return Err("a talk lasts at least one second".into()),
        None if doc.talk_seconds.is_some() => tree.remove_field(&loc, "talk"),
        None => return Ok(source.to_string()),
    }.map(|e| e.syntax.print()).map_err(|e| e.to_string())
}

fn set_deck(source: &str, title: Option<&str>, transition: Option<&Transition>, output: Option<(u32, u32)>) -> Result<String, String> {
    let (_, mut object) = deck_record(source)?;
    let mut written = source.to_string();
    let mut values: Vec<(&str, String)> = Vec::new();
    if let Some(title) = title {
        values.push(("title", source::string_literal(title)));
    }
    if let Some(cut) = transition {
        values.push(("transition", transition_text(cut)));
    }
    if let Some((w, h)) = output {
        values.push(("output", format!("{{width: {w} height: {h}}}")));
    }
    for (field, value) in values {
        written = set_field(&written, object, field, &value)?;
        object = deck_record(&written)?.1;
    }
    Ok(written)
}

// ------------------------------------------------------ a slide's cut

/// A slide's cut as an ordinary edit source, and the way back.
///
/// The edit's nodes are `let`s of the deck file, so the edit source
/// is everything before `Deck{` (without the `use mod.deck.*` line
/// an edit does not know) followed by the slide's `Flow{...}`. After the
/// edit ops have changed it, [`Extracted::put_back`] writes the lets and the
/// flow back: every other byte of the file is as it was.
pub struct Extracted {
    original: String,
    /// The start of `Deck`.
    deck_start: usize,
    /// The slide's flow: `{` to just past `}`.
    flow: (usize, usize),
    /// The parsed deck import, including the trivia its statement owns.
    import: Option<ExtractedImport>,
    tail: String,
    /// The `let` the slide's flow is written in, when it is one.
    flow_let: Option<String>,
    text: String,
}

struct ExtractedImport {
    index: usize,
    /// Byte offset from the preceding statement's end (or the file start).
    offset: usize,
    source: String,
}

impl Extracted {
    /// The slide's cut, as an edit source.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The deck with the edit this edit source became.
    pub fn put_back(&self, edited: &str) -> Result<String, String> {
        let tree = parse(edited)?;
        let last = tree.statements().last().ok_or("the edit is empty")?;
        let (open, close) = match &last.kind {
            Kind::Object { ty: Some(ty), open, close, .. } if ty == "Flow" => (*open, *close),
            _ => return Err("the edit lost its final Flow object".into()),
        };
        let mut lets = edited[..last.span.start].trim_end().to_string();
        lets.push_str(&self.tail);
        if let Some(import) = &self.import {
            let tree = parse(&lets)?;
            let before = if import.index == 0 {
                0
            } else {
                tree.statements().get(import.index - 1).ok_or("the edit lost the statement before the deck import")?.span.end
            };
            let at = before + import.offset;
            let end = tree.statements().get(import.index).map_or(lets.len(), |node| node.span.start);
            if at > end || !lets.is_char_boundary(at) {
                return Err("the edit changed the trivia before the deck import".into());
            }
            lets = tree.apply(&[Replace::new(at..at, import.source.clone())]).map_err(|e| e.to_string())?.print();
        }
        let flow = &edited[open..=close];
        let original = parse(&self.original)?;
        let edits = match &self.flow_let {
            None => vec![Replace::new(0..self.deck_start, lets), Replace::new(self.flow.0..self.flow.1, flow)],
            Some(id) => {
                let (open, close) = constructor(&lets, id)?;
                lets = parse(&lets)?.apply(&[Replace::new(open..close + 1, flow)]).map_err(|e| e.to_string())?.print();
                vec![Replace::new(0..self.deck_start, lets)]
            }
        };
        original.apply(&edits).map(|tree| tree.print()).map_err(|e| e.to_string())
    }
}

/// Apply a composition operation to the Cut file of a compact slide.
pub fn edit_graphic_cut(source: &str, doc: &DeckDoc, index: usize, op: &DeckOp) -> Result<String, String> {
    let syntax = parse(source)?;
    let root = syntax.statements().last().ok_or("empty slide cut")?;
    let Kind::Object { open, .. } = &root.kind else { return Err("a slide cut ends in Flow{...}".into()) };
    let mut local = doc.clone();
    let slide = local.slides.get_mut(index).ok_or("no such slide")?;
    slide.cut_span = Some((*open, root.span.end));
    match op {
        DeckOp::AddLayer { layer, .. } => add_layer(source, &local, index, layer),
        DeckOp::SetCamera { set, .. } => set_camera(source, &local, index, set),
        _ => Err("this operation edits the deck root".into()),
    }
}

/// Move an authored cut into a tree file, leaving a compact slide reference.
pub fn reference_cut(source: &str, doc: &DeckDoc, index: usize, path: &str) -> Result<String, String> {
    let slide = slide_at(doc, index)?;
    let flow = slide.cut_span.ok_or("the slide has no inline cut")?;
    let mut orphans = members(source, flow)?;
    if let Some(field) = fields(source, slide_object(slide))?.into_iter().find(|field| field.name.as_deref() == Some("cut")) {
        let value = &source[field.value.0..field.value.1];
        if valid_name(value) { orphans.push(value.into()); }
    }
    let written = remove_field(source, slide_object(slide), "cut")?;
    let written = set_field(&written, slide_object(slide), "graphic", &source::string_literal(path))?;
    drop_orphan_lets(written, orphans)
}

/// Slide `index`'s edit as an edit source.
pub fn extract(source: &str, doc: &DeckDoc, index: usize) -> Result<Extracted, String> {
    let slide = slide_at(doc, index)?;
    let flow = slide.cut_span.ok_or_else(|| format!("slide {index} (`{}`) plays a library cut; open that cut in CUT to change it, or replace the slide's `cut:`", slide.name))?;
    let (deck_start, _) = deck_record(source)?;
    let region = &source[..deck_start];
    let tree = parse(region)?;
    let use_at = tree.statements().iter().position(|node| matches!(&node.kind, Kind::Use { path } if path.ident() == Some("mod.deck.*")));
    let (lets, import) = match use_at {
        Some(index) => {
            let edits = edit::remove_element_edits(region, tree.root(), index).map_err(|e| e.to_string())?;
            let range = edits[0].range.clone();
            let before = if index == 0 { 0 } else { tree.statements()[index - 1].span.end };
            let import = ExtractedImport { index, offset: range.start - before, source: region[range].to_string() };
            (tree.apply(&edits).map_err(|e| e.to_string())?.print(), Some(import))
        }
        None => (region.to_string(), None),
    };
    // The removed import may own the prelude's last line break. Keep only
    // the tail still present, so restoring the import does not double it.
    let tail = lets[lets.trim_end().len()..].to_string();
    let mut flow_let = None;
    if flow.0 < deck_start {
        // The flow is a `let` of the file: find which.
        let tree = parse(region)?;
        for node in tree.statements() {
            if let Kind::Let { names, value: Some(value), .. } = &node.kind {
                if matches!(&value.kind, Kind::Object { open, .. } if *open == flow.0) && names.len() == 1 { flow_let = Some(names[0].clone()); break; }
            }
        }
        if flow_let.is_none() {
            return Err(format!("slide {index}'s edit is not a plain `let name = Flow{{...}}`; edit its source"));
        }
    }
    let text = format!("{}\nFlow{}", lets.trim_end(), &source[flow.0..flow.1]);
    Ok(Extracted { original: source.to_string(), deck_start, flow, import, tail, flow_let, text })
}
