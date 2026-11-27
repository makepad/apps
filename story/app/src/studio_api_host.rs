//! Story's live implementation of the AI control API
//! (`makepad-studio-api`): the tool registry's handlers over the CUT and
//! DECK pages, the generation studio, the library and app runs. The status,
//! document tree, library, effect, kernel and generation tools are
//! `makepad-product-tools`'s, over this window's [`Documents`] (the deck on
//! DECK, else the cut):
//!
//! - **In process** (the chat's video maker): [`StoryApp::studio_call`],
//!   [`StoryApp::studio_result`], [`StoryApp::studio_tools`] and
//!   [`StoryApp::studio_events_since`].
//! - **AI services, MCP and CLI**: the service ports in `cut::chat` call
//!   the same host through the process's makepad-remote `app` service.
//!
//! **CUT is the target**: every mutating tool
//! acts on the cut open in the CUT page, through the page's own
//! operations (`ops::apply`, the same source-preserving patchers), and
//! lands as one step on the page's undo stack, visible at once. The ops run
//! on this module's own worker against the source they were planned on; the
//! result is installed only while the page has no cut of its own in
//! flight and the source is still that one, otherwise the op is planned
//! again on the new source, so a person editing at the same time never
//! loses a change and never sees one overwritten. Changes by hand are
//! logged as `page` events. The page's drafts autosave every change (as
//! they do the person's); `save_cut` publishes a library revision.
//!
//! This file is a child of `video_cut` (declared there with `#[path]`) so
//! it reaches the page's session and ops without widening their
//! visibility; it adds no field to the page or the app: its state is a
//! UI-thread-local, touched only from the UI thread.
//!
//! Main-loop cost: the pump drains at most a few replies per frame from
//! bounded channels (`try_recv`), checks the waiting calls (a handful), and
//! compares the page's revision number; a diff is computed once per change
//! (linear in the cut's lines). Splash evaluation, the library, app runs
//! and publishing run on three long-lived workers.

use super::ops::{self, CutOp, Media, MediaKind, Respan};
use super::patch::{diagnostics, Patcher, Value as PValue};
use makepad_asset_connection as asset_connection;
use makepad_asset_connection::redact;
use super::super::{StoryApp, StoryPage};
use super::open_doc::{DocKind, OpenDoc, TreeDoc};
use super::{Pending as PagePending, VideoCut};
use makepad_asset_client::resolver::{select_file, TierPreference};
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, DeviceTier, FileRole, MediaType};
use makepad_motion::bind::tools as bind_tools;
use makepad_app_shell::tool_host::{self, Change, Documents, History, OpenTree, ToolHost, ToolProduct, Worker, REPLIES_PER_PUMP};
use makepad_product_tools::gen::{self, GenTools, GenWait};
use makepad_product_tools::library::{png_of, placeholder_jpeg, tree_revisions};
use makepad_product_tools::{done, fail, flag, kind_name, num, number, reference, reference_json, text, LibraryJob, LibraryOut, ProductTools};
use makepad_studio_api::{self as api, beats, diff, CallResult, ChangeEvent, DocChange, ErrorKind, Image, Registry, ToolError, ToolOutput, ToolSpec};
use makepad_story_cut::{AssetRef, CutDoc, CutEval, CutNode, CutNodeKind, EffectRef, MediaRef, RunRef, SourceTime, VoiceLine, VoiceSpec};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

thread_local! {
    static HOST: RefCell<Option<Box<Host>>> = const { RefCell::new(None) };
}

/// One step of a cut change, applied in order on the cut worker.
#[derive(Clone, Debug)]
enum Step {
    /// The open tree, and for a deck the deck loaded from it: a change
    /// evaluates again only the slides it touches.
    Tree { tree: Arc<makepad_asset_data::tree::TreeDraft>, path: String, deck: Option<Arc<makepad_story_deck::DeckDoc>> },
    Document(Change),
    Patch(Vec<api::document::Edit>),
    Op(CutOp),
    /// Fix up the block the previous step added: its span and `in`, or a
    /// new voiceover's lines.
    Added { start: Option<f64>, length: Option<f64>, source_in: Option<f64>, lines: Option<Vec<VoiceLine>> },
    /// Point the Fx the previous step added at a library effect revision.
    AddedFx(AssetRef),
    /// Graphics bound to a clip (M4 overlays): one per span, on top of the
    /// output's picture stack.
    Graphics { src: String, clip: String, spans: Vec<(f64, f64)>, params: Vec<(String, String)> },
    /// Remove nodes the page's ops do not know as blocks (Graphics).
    Remove(Vec<String>),
    /// A change to a deck (its slides, their fields and demos).
    Deck(makepad_story_deck::ops::DeckOp),
    /// Steps on one slide's edit, as an ordinary edit: the node tools of
    /// the DECK page.
    Slide { slide: usize, steps: Vec<Step> },
}

struct CutJob {
    ticket: u64,
    /// Which kind of document `base` is.
    kind: DocKind,
    base: String,
    steps: Vec<Step>,
}

struct CutDone {
    ticket: u64,
    result: Result<(String, OpenDoc), String>,
}

/// A change to the document of either kind, on the worker.
fn run_edit(patcher: &mut Result<Patcher, String>, job: CutJob) -> CutDone {
    if let Ok(patcher) = patcher { patcher.tree = None; }
    let result = if let Some(Step::Tree { tree, path, deck }) = job.steps.first() {
        run_tree_steps(patcher, tree.clone(), path, deck.as_ref(), &job.steps[1..])
    } else { match job.kind {
        DocKind::Cut => run_edit_steps(patcher, &job.base, &job.steps).map(|(source, doc)| (source, OpenDoc::Cut(Arc::new(doc)))),
        DocKind::Deck => run_deck_steps(patcher, &job.base, &job.steps).map(|(source, doc)| (source, OpenDoc::Deck(Arc::new(doc)))),
    }};
    CutDone { ticket: job.ticket, result }
}

fn run_tree_steps(patcher: &mut Result<Patcher, String>, mut tree: Arc<makepad_asset_data::tree::TreeDraft>, path: &str, deck: Option<&Arc<makepad_story_deck::DeckDoc>>, steps: &[Step]) -> Result<(String, OpenDoc), String> {
    use super::super::document_tree;
    if tree.files.get(path).is_some_and(|file| file.kind == AssetKind::Cut) {
        patcher.as_mut().map_err(|e| e.clone())?.tree = Some((tree.clone(), path.into()));
        let (source, _) = run_edit_steps(patcher, tree.text(path)?, steps)?;
        let (working, path) = patcher.as_ref().map_err(|e| e.clone())?.tree.clone().unwrap();
        tree = document_tree::apply(&working, &Change::Write { path: path.clone(), text: source.clone(), expected: None })?;
        let doc = document_tree::load(&tree, &path)?;
        return Ok((source, OpenDoc::Tree { doc, tree, path }));
    }
    // A tree may hold several decks: an edit changes the open one.
    let deck_file = document_tree::deck_path(&tree, path);
    // The open deck, when it is this file of this tree: its slides are not evaluated again.
    let loaded_here = |doc: &&Arc<makepad_story_deck::DeckDoc>| doc.tree.as_ref().is_some_and(|context| std::ptr::addr_eq(Arc::as_ptr(&context.resolver), Arc::as_ptr(&tree)) && context.document == deck_file);
    let mut doc = match deck.filter(loaded_here) {
        Some(doc) => doc.clone(),
        None => document_tree::load_deck_at(&tree, &deck_file)?,
    };
    let deck = deck_file;
    for step in steps {
        let before = tree.clone();
        match step {
            Step::Document(change) => tree = document_tree::apply(&tree, change)?,
            Step::Patch(edits) => tree = document_tree::apply(&tree, &Change::Patch { path: deck.clone(), edits: edits.clone() })?,
            Step::Deck(op) => {
                let composition = match op {
                    makepad_story_deck::ops::DeckOp::AddLayer { index, .. } | makepad_story_deck::ops::DeckOp::SetCamera { index, .. } => Some(*index),
                    _ => None,
                };
                if let Some(index) = composition {
                    if let Some(path) = doc.slides.get(index).and_then(|slide| slide.graphic.as_ref()) {
                        if tree.files.get(path).is_none_or(|file| file.kind != AssetKind::Cut) { return Err(format!("{path}: edit this Motion slide with doc_patch")); }
                        let source = makepad_story_deck::ops::edit_graphic_cut(tree.text(path)?, &doc, index, op)?;
                        tree = document_tree::apply(&tree, &Change::Write { path: path.clone(), text: source, expected: None })?;
                        doc = document_tree::reload_deck(&tree, &deck, &before, &doc)?;
                        continue;
                    }
                }
                let (mut source, next) = makepad_story_deck::ops::apply_in(tree.text(&deck)?, op, Some(tree.context(&deck)))?;
                if let makepad_story_deck::ops::DeckOp::DuplicateSlide { index, .. } = op {
                    if let Some(from) = &doc.slides[*index].graphic {
                        let slide = &next.slides[*index + 1];
                        let suffix = if tree.files.get(from).is_some_and(|file| file.kind == AssetKind::Cut) { ".cut.splash" } else { ".splash" };
                        let stem = document_tree::media_stem(&tree, &slide.name, "slides");
                        let to = format!("{stem}{suffix}");
                        tree = document_tree::apply(&tree, &Change::Create { path: to.clone(), from: Some(from.clone()), text: None })?;
                        let syntax = makepad_script::syntax::parse(&source).map_err(|e| e.to_string())?;
                        let mut replacement = None;
                        syntax.walk(&mut |_, node| {
                            if matches!(&node.kind, makepad_script::syntax::Kind::Object { open, .. } if *open == slide.span.0) {
                                if let Some(field) = node.field("graphic") {
                                    replacement = Some(makepad_script::syntax::edit::Replace::new(field.value.span.clone(), makepad_story_cut::source::string_literal(&to)));
                                } else if let Some(field) = node.field("cut") {
                                    replacement = Some(makepad_script::syntax::edit::Replace::new(field.span.clone(), format!("graphic: {}", makepad_story_cut::source::string_literal(&to))));
                                }
                            }
                        });
                        let replacement = replacement.ok_or("duplicate slide is missing its graphic source")?;
                        source = syntax.apply(&[replacement]).map_err(|error| error.to_string())?.print();
                    }
                }
                let added = match op {
                    makepad_story_deck::ops::DeckOp::AddSlide { after, .. } => Some(after.map_or(doc.slides.len(), |index| index + 1)),
                    makepad_story_deck::ops::DeckOp::AddSlideFirst { .. } => Some(0),
                    _ => None,
                };
                if let Some(index) = added.filter(|index| next.slides[*index].cut_span.is_some()) {
                    let extracted = makepad_story_deck::ops::extract(&source, &next, index)?;
                    let syntax = makepad_script::syntax::parse(extracted.text()).map_err(|error| error.to_string())?;
                    let flow = syntax.statements().last().ok_or("new slide has no cut")?;
                    let imports = syntax.statements().iter().filter(|node| matches!(node.kind, makepad_script::syntax::Kind::Use { .. }))
                        .map(|node| &extracted.text()[node.span.clone()]).collect::<Vec<_>>().join("\n");
                    let cut = super::super::still_cache::declarations(extracted.text(), (flow.span.start, flow.span.end));
                    let path = format!("{}.cut.splash", document_tree::media_stem(&tree, &next.slides[index].name, "slides"));
                    tree = document_tree::apply(&tree, &Change::Create { path: path.clone(), from: None, text: Some(format!("{imports}\n{cut}\n")) })?;
                    source = makepad_story_deck::ops::reference_cut(&source, &next, index, &path)?;
                }
                tree = document_tree::apply(&tree, &Change::Write { path: deck.clone(), text: source, expected: None })?;
            }
            Step::Op(CutOp::Source { text }) => tree = document_tree::apply(&tree, &Change::Write { path: deck.clone(), text: text.clone(), expected: None })?,
            Step::Slide { slide, steps } => {
                let slide_doc = doc.slides.get(*slide).ok_or("no such slide")?;
                if let Some(path) = &slide_doc.graphic {
                    if tree.files.get(path).is_none_or(|file| file.kind != AssetKind::Cut) { return Err(format!("{path}: use doc_patch on this Motion graphic")); }
                    patcher.as_mut().map_err(|e| e.clone())?.tree = Some((tree.clone(), path.clone()));
                    let (source, _) = run_edit_steps(patcher, tree.text(path)?, steps)?;
                    tree = patcher.as_ref().map_err(|e| e.clone())?.tree.as_ref().unwrap().0.clone();
                    tree = document_tree::apply(&tree, &Change::Write { path: path.clone(), text: source, expected: None })?;
                } else {
                    let extracted = makepad_story_deck::ops::extract(tree.text(&deck)?, &doc, *slide)?;
                    patcher.as_mut().map_err(|e| e.clone())?.tree = Some((tree.clone(), deck.clone()));
                    let (source, _) = run_edit_steps(patcher, extracted.text(), steps)?;
                    tree = patcher.as_ref().map_err(|e| e.clone())?.tree.as_ref().unwrap().0.clone();
                    tree = document_tree::apply(&tree, &Change::Write { path: deck.clone(), text: extracted.put_back(&source)?, expected: None })?;
                }
            }
            other => return Err(format!("{other:?} cannot edit a deck tree")),
        }
        doc = document_tree::reload_deck(&tree, &deck, &before, &doc)?;
    }
    let path = deck;
    Ok((tree.text(&path)?.to_string(), OpenDoc::Tree { doc: TreeDoc::Deck(doc), tree, path }))
}

/// The steps on a deck. Without steps it is only loaded.
fn run_deck_steps(patcher: &mut Result<Patcher, String>, base: &str, steps: &[Step]) -> Result<(String, makepad_story_deck::DeckDoc), String> {
    use makepad_story_deck::ops::{self as deck_ops, describe};
    let mut source = base.to_string();
    let mut doc = makepad_story_deck::DeckDoc::load(&source).map_err(|diags| describe(&diags))?;
    for step in steps {
        match step {
            Step::Patch(edits) => {
                source = Patcher::document(&source, edits)?;
                doc = makepad_story_deck::DeckDoc::load(&source).map_err(|diags| describe(&diags))?;
            }
            Step::Op(CutOp::Source { text }) => {
                doc = makepad_story_deck::DeckDoc::load(text).map_err(|diags| describe(&diags))?;
                source = text.clone();
            }
            Step::Deck(op) => (source, doc) = deck_ops::apply(&source, op)?,
            Step::Slide { slide, steps } => {
                // The slide's edit is a cut source: the cut steps run on
                // it, and the result is written back into the file.
                let extracted = deck_ops::extract(&source, &doc, *slide)?;
                let (edited, _) = run_edit_steps(patcher, extracted.text(), steps)?;
                source = extracted.put_back(&edited)?;
                doc = makepad_story_deck::DeckDoc::load(&source).map_err(|diags| format!("the change broke the deck: {}", describe(&diags)))?;
            }
            other => return Err(format!("{other:?} is a step of a cut, not of a deck")),
        }
    }
    Ok((source, doc))
}

/// The steps on a cut. Without steps it is only loaded.
fn run_edit_steps(patcher: &mut Result<Patcher, String>, base: &str, steps: &[Step]) -> Result<(String, CutDoc), String> {
    let result = (|| {
        let patcher = patcher.as_mut().map_err(|error| error.clone())?;
        let mut source = base.to_string();
        let mut doc = patcher.load(&source)?;
        let mut before = doc.clone();
        for step in steps {
            let op = match step {
                Step::Patch(edits) => CutOp::Source { text: Patcher::document(&source, edits)? },
                Step::Op(CutOp::Source { text }) => CutOp::Source { text: api::document::write(&source, text)? },
                Step::Op(op) => op.clone(),
                Step::Remove(ids) => CutOp::Source { text: remove_graphics(patcher, &source, ids)? },
                Step::Document(change) => {
                    let (tree, mut path) = patcher.tree.clone().ok_or("open a document tree first")?;
                    let tree = super::super::document_tree::apply(&tree, change)?;
                    if let Change::Rename { from, to } = change { if *from == path { path = to.clone(); } }
                    if tree.text(&path)? != source { source = tree.text(&path)?.into(); }
                    patcher.tree = Some((tree, path));
                    doc = patcher.load(&source)?;
                    continue;
                }
                Step::Deck(_) | Step::Slide { .. } | Step::Tree { .. } => return Err("a change of a deck is not a step of a cut".to_string()),
                Step::Graphics { src, clip, spans, params } => CutOp::Source { text: add_graphics(patcher, &source, &doc, src, clip, spans, params)? },
                Step::AddedFx(effect) => {
                    let fx = doc.nodes.iter().find(|node| before.node(&node.id).is_none() && matches!(node.kind, CutNodeKind::Fx { .. }))
                        .ok_or("the effect that was just added is not in the cut")?;
                    CutOp::Param { node: fx.id.clone(), key: "effect".into(), value: ops::media_value(&MediaRef::Asset(effect.clone())) }
                }
                Step::Added { start, length, source_in, lines } => {
                    let fresh = doc.nodes.iter().find(|node| before.node(&node.id).is_none() && (node.span.is_some() || matches!(node.kind, CutNodeKind::VoiceOver { .. })))
                        .ok_or("the block that was just added is not in the cut")?;
                    match lines {
                        Some(lines) => CutOp::SetLines { node: fresh.id.clone(), lines: lines.clone() },
                        None => {
                            let span = fresh.span.ok_or("the new block has no span")?;
                            CutOp::Respan { spans: vec![Respan { node: fresh.id.clone(), start: start.unwrap_or(span.start), length: length.unwrap_or(span.length), source_in: *source_in }] }
                        }
                    }
                }
            };
            if let Some((tree, path)) = patcher.tree.clone() { super::super::document_tree::prepare_cut(patcher, &tree, &path, &op)?; }
            let (next, next_doc) = ops::apply(patcher, &source, &op)?;
            before = std::mem::replace(&mut doc, next_doc);
            source = next;
            if let Some((tree, path)) = patcher.tree.clone() {
                if tree.files.get(&path).is_some_and(|file| file.kind == AssetKind::Cut) {
                    let tree = super::super::document_tree::apply(&tree, &Change::Write { path: path.clone(), text: source.clone(), expected: None })?;
                    patcher.tree = Some((tree, path));
                }
            }
        }
        Ok((source, doc))
    })();
    result
}

/// Library, publishing and app-run work: blocking, on its own workers.
enum SlowJob {
    /// A shared tool's library work (`makepad_product_tools::library`).
    Library(LibraryJob),
    InspectTree(Arc<makepad_asset_data::tree::TreeDraft>),
    PreviousTree { head: AssetRevisionRef, alias: makepad_asset_data::AssetAlias, path: String, file: Option<makepad_asset_data::AssetId>, root: bool },
    OpenEdit { asset: AssetRevisionRef },
    OpenProject { project: String, path: Option<String> },
    /// A deck of the library, by exact revision or by alias.
    OpenDeck { asset: Option<AssetRevisionRef>, alias: Option<String> },
    ReadRun { render: AssetRevisionRef },
    AppDump { app: String, output: makepad_story_app_run::RunOutput, build: bool },
    ValidateRun { source: String, run: bool },
    RunApp { source: String, run: Option<AssetRevisionRef>, label: String },
    /// Load a Motion document (M1): its diagnostics.
    ValidateMotion { source: String },
    /// Load and publish a Motion document as a new revision of `asset`
    /// (or a new asset).
    PublishMotion { title: String, source: String, asset: Option<AssetId> },
    /// A Motion revision's source.
    MotionSource { asset: AssetRevisionRef },
    /// M2's inspect of a cut, with the library as its resolver.
    /// `motions`: (name, source, played by this cut) of the session's
    /// Motion documents, whose blocking issues the cut's inspect reports
    /// too (a nested cut's documents included).
    Inspect { source: String, name: String, motions: Vec<(String, String, bool)> },
    /// M2's audio analysis of a cut's mix.
    AudioAnalyze { source: String, tree: Option<makepad_story_cut::tree::TreeContext> },
    /// A music asset's beat grid, onsets, energy and sections, in numbers
    /// and in words (M3's `audio_features`).
    MusicAnalysis { asset: AssetRevisionRef },
    /// Frames of a cut through M2's `makepad-edit-tools` on the GPU.
    Grab { source: String, tree: Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>, folder: Option<std::path::PathBuf>, command: Grab },
    /// M4: what an app-run render recorded.
    RunFacts { render: AssetRevisionRef },
    /// M4: place an overlay against a clip's run, publishing the overlay
    /// document first when this session has not.
    BindOverlay { render: AssetRevisionRef, request: bind_tools::OverlayRequest, clip: (f64, f64, SourceTime, f64), overlay_asset: Option<AssetRevisionRef> },
}

/// A picture command of `makepad-edit-tools`.
#[derive(Clone, Debug)]
enum Grab {
    Frame { at: f64, width: u32 },
    Strip { count: u32, from: Option<f64>, to: Option<f64> },
    Onion { from: f64, to: f64, steps: u32 },
}

enum SlowOut {
    Json(Json),
    Png(Json, Vec<u8>),
    Source(String),
    /// A deck's revision and source.
    Deck(super::store::OpenTree),
}

struct SlowDone {
    ticket: u64,
    result: Result<SlowOut, ToolError>,
}

fn slow_dir(name: &str) -> std::path::PathBuf {
    makepad_widgets::makepad_platform::home::app_state_home().join("stage-product").join("studio-api").join(name)
}

fn run_slow(client: &mut Option<AssetClient>, (ticket, job): (u64, SlowJob)) -> SlowDone {
    fn connected(client: &mut Option<AssetClient>) -> Result<&mut AssetClient, ToolError> {
        asset_connection::connected(client, &asset_connection::origin())
            .map_err(|error| ToolError::failed(format!("Stage's library is not reachable: {error}")))
    }
    let result = match job {
        SlowJob::Library(job) => makepad_product_tools::library::run(job, || connected(client)).map(|out| match out {
            LibraryOut::Json(value) => SlowOut::Json(value),
            LibraryOut::Png(value, png) => SlowOut::Png(value, png),
        }),
        SlowJob::InspectTree(tree) => {
            // Every deck of the tree loads, and its links reach a deck and slide.
            let decks = super::super::document_tree::deck_files(&tree);
            let roots = if decks.is_empty() { vec![tree.root.1.clone()] } else { decks };
            let mut errors: Vec<Json> = roots.iter().filter_map(|path| super::super::document_tree::load(&tree, path).err()).map(json::s).collect();
            errors.extend(super::super::document_tree::check_deck_links(&tree).into_iter().map(json::s));
            let mut warnings = Vec::new();
            for path in &roots { inspect_tree_plates(&tree, path, &mut errors, &mut warnings); }
            Ok(SlowOut::Json(json::obj(vec![("path", json::s(tree.root.1.clone())), ("clean", Json::Bool(errors.is_empty() && warnings.is_empty())), ("errors", Json::Arr(errors)), ("warnings", Json::Arr(warnings))])))
        }
        SlowJob::PreviousTree { head, alias, path, file, root } => connected(client).and_then(|client| {
            let revisions = tree_revisions(client, head)?;
            let index = revisions.iter().position(|reference| *reference == head).ok_or_else(|| ToolError::not_found("the tree revision is not in its history"))?;
            let previous = index.checked_sub(1).and_then(|index| revisions.get(index)).ok_or_else(|| ToolError::refused("nothing earlier to undo"))?;
            let mut opened = super::store::tree_revision(client, *previous, alias, false).map_err(ToolError::refused)?;
            let path = if root { opened.draft.root.1.clone() } else if opened.draft.files.contains_key(&path) { path } else {
                opened.draft.files.values().find(|entry| entry.reference.map(|reference| reference.asset_id) == file).map(|entry| entry.path.clone())
                    .ok_or_else(|| ToolError::refused("this cut did not exist in the previous tree revision"))?
            };
            opened.doc = super::super::document_tree::load(&opened.draft, &path).map_err(ToolError::refused)?;
            opened.path = path;
            Ok(opened)
        }).map(SlowOut::Deck),
        SlowJob::OpenEdit { asset } => connected(client).and_then(|client| open_source(client, &asset)).map(SlowOut::Source),
        SlowJob::OpenProject { project, path } => connected(client).and_then(|client| super::store::open_project(client, &project, path.as_deref()).map_err(ToolError::refused)).map(SlowOut::Deck),
        SlowJob::OpenDeck { asset, alias } => connected(client).and_then(|client| open_deck_source(client, asset, alias.as_deref())).map(SlowOut::Deck),
        SlowJob::ReadRun { render } => connected(client).and_then(|client| read_run(client, &render)).map(SlowOut::Json),
        SlowJob::AppDump { app, output, build } => makepad_story_app_run::app_dump(&app, output, build, &slow_dir("dumps").join(&app))
            .map(|dump| SlowOut::Json(json::obj(vec![("dump", json::s(dump))])))
            .map_err(ToolError::failed),
        SlowJob::ValidateRun { source, run } => validate_run(&source, run).map(SlowOut::Json),
        SlowJob::RunApp { source, run, label } => connected(client).and_then(|client| run_app(client, &source, run, &label)).map(SlowOut::Json),
        SlowJob::ValidateMotion { source } => Ok(SlowOut::Json(validate_motion_json(&source))),
        SlowJob::PublishMotion { title, source, asset } => connected(client).and_then(|client| publish_motion(client, &title, &source, asset)).map(SlowOut::Json),
        SlowJob::MotionSource { asset } => connected(client).and_then(|client| text_source(client, &asset, AssetKind::Motion)).map(SlowOut::Source),
        SlowJob::Inspect { source, name, motions } => Ok(SlowOut::Json(inspect_edit(&source, &name, &motions))),
        SlowJob::AudioAnalyze { source, tree } => audio_analyze_edit(&source, tree).map(SlowOut::Json),
        SlowJob::MusicAnalysis { asset } => connected(client).and_then(|client| music_analysis(client, &asset)).map(SlowOut::Json),
        SlowJob::RunFacts { render } => connected(client).and_then(|client| fetch_run(client, &render)).map(|run| SlowOut::Json(bind_tools::run_facts(&run))),
        SlowJob::BindOverlay { render, request, clip, overlay_asset } => connected(client).and_then(|client| bind_overlay_job(client, &render, &request, clip, overlay_asset)).map(SlowOut::Json),
        SlowJob::Grab { source, tree, folder, command } => connected(client).and_then(|client| grab(client, &source, tree.as_ref(), folder.as_deref(), &command)).map(|(info, png)| SlowOut::Png(info, png)),
    };
    SlowDone { ticket, result }
}

// ------------------------------------------------------------------- host

/// What a call is waiting for.
enum Wait {
    Preview { ticket: u64, revision: u64, done: Option<Result<(String, OpenDoc), String>>, rendering: bool },
    /// A change on the cut worker, then its install.
    /// `extra`: fields of an earlier step's answer (a publish) merged into
    /// this one's.
    Cut { ticket: u64, kind: DocKind, recipe: Recipe, base: String, base_revision: Option<u64>, done: Option<Result<(String, OpenDoc), String>>, extra: Option<Json> },
    /// An edit to open (new or from the library), then the page's open.
    Open { ticket: u64, head: Option<AssetRevisionRef>, saved: bool, done: Option<Result<(String, Arc<CutDoc>), String>> },
    /// Library work.
    Slow { ticket: u64, then: Then },
    /// The page's save.
    Save { revision: u64 },
    /// The page's voice takes for a node.
    Speak { node: String },
    /// A generation tool's (`makepad_product_tools::gen`).
    Gen(GenWait<StoryTools>),
    /// An app-run job, until it ends or the deadline.
    AppRun { job: u64, deadline: f64 },
    /// A generative-segments request (G1), until its answer is in; the
    /// edit's source before it, for the change's diff.
    Generate { request: u64, before: Option<(u64, String)>, answer: Option<Result<Json, String>> },
    /// A storyboard tool that waits (SB1: an update run, a look at the
    /// references), until its answer is in.
    Storyboard { request: u64 },
    /// A web export (FB1), until it is done.
    WebExport { id: u64 },
    /// Variants of the open graphic (VR1), until they are in, or a graphic
    /// being opened, until it has loaded.
    Variants(super::motion_edit::variants::Pending),
}

/// What a library reply turns into.
enum Then {
    UndoTree { revision: u64, kind: DocKind },
    Reply,
    Open { asset: AssetRevisionRef },
    /// A deck's source, for the DECK page.
    OpenDeck,
    OpenProject { then: Option<(String, Json)> },
    AppRun { job: u64 },
    /// A Motion document was published under `name`: remember it and move
    /// the open cut's references to its new revision.
    Motion { name: String, title: String, source: String, previous: Option<AssetRevisionRef> },
    /// A Motion document's source to answer `read_motion` with.
    MotionSource { name: String },
    /// An overlay placed (M4): add its graphics to the cut.
    Bind { clip: String, overlay: String },
}

/// How a change is made, so it can be planned again on a newer source.
enum Recipe {
    Steps(Vec<Step>),
    Write(String),
    Edits(Vec<api::document::Edit>),
    Header(Header),
    /// Every `old` reference in the source becomes `new` (a Motion
    /// document's new revision).
    Repoint { old: String, new: String },
}

/// The cut's own fields, written in place in its final `Flow{…}`.
#[derive(Clone, Default)]
struct Header {
    label: Option<String>,
    duration: Option<f64>,
    bpm: Option<f64>,
    /// `(width, height, fps)`.
    output: Option<(u32, u32, f64)>,
}

impl Header {
    fn write(&self, source: &str) -> Result<String, ToolError> {
        use super::source::{final_flow, number as splash_number, plain_literal, set_field, string_literal, field_text};
        let mut written = source.to_string();
        let mut fields: Vec<(&str, String)> = Vec::new();
        if let Some(label) = &self.label {
            fields.push(("label", string_literal(label)));
        }
        if let Some((width, height, fps)) = self.output {
            fields.push(("output", format!("{{width: {width} height: {height} fps: {}}}", splash_number(fps).trim_end_matches(".0"))));
        }
        if let Some(duration) = self.duration {
            fields.push(("duration", splash_number(duration)));
        }
        if let Some(bpm) = self.bpm {
            fields.push(("bpm", splash_number(bpm).trim_end_matches(".0").to_string()));
        }
        for (key, value) in fields {
            let object = final_flow(&written).ok_or_else(|| ToolError::refused("the cut has no final `Flow{…}`: call doc_read"))?;
            if field_text(&written, object, key).is_some_and(|text| !plain_literal(text)) {
                return Err(ToolError::refused(format!("the cut's `{key}` is an expression in its source: change it with doc_patch")));
            }
            written = set_field(&written, object, key, &value);
        }
        Ok(written)
    }
}

impl Recipe {
    fn steps(&self, source: &str) -> Result<Vec<Step>, ToolError> {
        Ok(match self {
            Recipe::Steps(steps) => steps.clone(),
            Recipe::Write(text) => vec![Step::Op(CutOp::Source { text: text.clone() })],
            Recipe::Edits(edits) => vec![Step::Patch(edits.clone())],
            Recipe::Header(header) => vec![Step::Op(CutOp::Source { text: header.write(source)? })],
            Recipe::Repoint { old, new } => {
                let syntax = makepad_script::syntax::parse(source).map_err(|error| ToolError::refused(error.to_string()))?;
                let mut edits = Vec::new();
                syntax.walk(&mut |_, node| {
                    if matches!(node.kind, makepad_script::syntax::Kind::Call { .. }) && node.text(source) == old {
                        edits.push(makepad_script::syntax::edit::Replace::new(node.span.clone(), new));
                    }
                });
                let text = syntax.apply(&edits).map_err(|error| ToolError::refused(error.to_string()))?.print();
                vec![Step::Op(CutOp::Source { text })]
            },
        })
    }
}

enum AppRunState {
    Running { since: f64 },
    Done(Json),
    Failed(String),
}

/// Story's tools on the shell's host.
struct StoryTools;

type Handler = tool_host::Handler<StoryTools>;
type Outcome = tool_host::Outcome<Wait>;
type Waiting = tool_host::Waiting<Wait>;
type Ctx<'a> = tool_host::Ctx<'a, StoryTools>;
type Host = ToolHost<StoryTools>;

/// What Stage's waits need: its workers, the page as last seen, app runs
/// and the Motion documents written.
#[derive(Default)]
struct State {
    edit: Option<Worker<CutJob, CutDone>>,
    slow: Option<Worker<(u64, SlowJob), SlowDone>>,
    runs: Option<Worker<(u64, SlowJob), SlowDone>>,
    /// The page's revision and source as last seen, for `page` events.
    seen: Option<(u64, String)>,
    app_runs: HashMap<u64, AppRunState>,
    /// Motion documents written in this session, by name: their library
    /// revision, title and source.
    motions: HashMap<String, (AssetRevisionRef, String, String)>,
}

impl ToolProduct for StoryTools {
    type App = StoryApp;
    type Wait = Wait;
    type State = State;

    fn register(registry: &mut Registry<Handler>) {
        register(registry);
    }

    fn start(host: &mut Host, cx: &mut Cx) {
        match Worker::new(cx, "studio-api-edit", {
            let mut patcher: Option<Result<Patcher, String>> = None;
            move |job: CutJob| run_edit(patcher.get_or_insert_with(Patcher::new), job)
        }) {
            Ok(worker) => host.state.edit = Some(worker),
            Err(error) => host.error = Some(error),
        }
        for (slot, name) in [(&mut host.state.slow, "studio-api-library"), (&mut host.state.runs, "studio-api-runs")] {
            let mut client = None;
            match Worker::new(cx, name, move |job| run_slow(&mut client, job)) {
                Ok(worker) => *slot = Some(worker),
                Err(error) => host.error = Some(error),
            }
        }
    }

    /// A call naming a project (or a logical file) other than the open one
    /// opens it first, then runs.
    fn run(ctx: &mut Ctx, tool: &'static str, handler: Handler, args: Json) -> Outcome {
        let app = &*ctx.app;
        let render_file = if tool == "render_cut" || tool == "export_web" { text(&args, "file") } else { None };
        let project = text(&args, "project").map(str::to_string).or_else(|| render_file.and_then(|_| project_name(app, target(app)).map(ToString::to_string)));
        if render_file.is_some() && project.is_none() {
            return fail(ToolError::invalid("a logical file needs an open project or a project path"));
        }
        if let Some(project) = project.filter(|_| tool != "open_cut") {
            let path = render_file;
            let same = project_name(app, target(app)).is_some_and(|alias| alias.to_string() == project)
                && path.is_none_or(|path| open_tree_state(app, target(app)).is_some_and(|(tree, current, ..)| tree.context(current).resolve(path).is_ok_and(|file| file.path == current)));
            if !same {
                let path = path.map(str::to_string);
                return slow(ctx, SlowJob::OpenProject { project: project.into(), path }, Then::OpenProject { then: Some((tool.into(), args)) }, false);
            }
        }
        handler(ctx, &args)
    }
}

impl GenTools for StoryTools {
    fn studio(app: &mut StoryApp) -> &mut makepad_flow_studio::FlowStudio {
        &mut app.flow_studio
    }

    fn studio_ref(app: &StoryApp) -> &makepad_flow_studio::FlowStudio {
        &app.flow_studio
    }

    fn gen_wait(wait: GenWait<StoryTools>) -> Wait {
        Wait::Gen(wait)
    }
}

/// The document tools act on the score on the SCORE page, else on the
/// deck while the DECK page is shown, else on the cut.
impl Documents for StoryTools {
    fn open_tree(app: &StoryApp) -> Option<OpenTree<'_>> {
        open_tree_state(app, target(app)).map(|(tree, path, revision, head)| OpenTree { tree, path, revision, head })
    }

    fn root_path(app: &StoryApp) -> String {
        match target(app) {
            DocKind::Deck => "deck.splash".into(),
            DocKind::Cut => "cut.splash".into(),
        }
    }

    fn doc_state(app: &StoryApp) -> Result<(String, u64), ToolError> {
        let kind = target(app);
        doc_state(app, kind).ok_or_else(|| no_doc(kind))
    }

    fn file_uses(app: &StoryApp) -> HashMap<String, Vec<Json>> {
        let mut uses: HashMap<String, Vec<Json>> = HashMap::new();
        let Some((_, deck, ..)) = open_tree_state(app, target(app)) else { return uses };
        for slide in app.deck.session().filter(|_| app.page == StoryPage::Deck).into_iter().flat_map(|session| &session.doc.slides) {
            let path = slide.graphic.as_deref().unwrap_or(deck);
            uses.entry(path.to_string()).or_default().push(json::obj(vec![("name", json::s(slide.name.clone())), ("apps", Json::Arr(slide.apps.iter().map(|app| json::s(app.app.clone())).collect()))]));
        }
        uses
    }

    fn selection(app: &StoryApp) -> Json {
        deck_json(app).get("selection").cloned().unwrap_or(Json::Null)
    }

    fn has_file_draft(app: &StoryApp, path: &str) -> bool {
        match target(app) {
            DocKind::Deck => app.deck.has_file_draft(path),
            DocKind::Cut => super::motion_edit::has_file_draft(&app.video_cut, path),
        }
    }

    fn history(app: &StoryApp, path: Option<&str>) -> Result<History, ToolError> {
        let _ = path;
        let kind = target(app);
        let steps = match kind {
            DocKind::Deck => app.deck.session().map(|session| session.undo.iter().map(|s| (s.source.clone(), s.tree.clone())).chain(std::iter::once((session.source.clone(), session.tree.clone()))).collect()),
            DocKind::Cut => app.video_cut.session.as_ref().map(|session| session.undo.iter().map(|s| (s.source.clone(), s.tree.clone())).chain(std::iter::once((session.source.clone(), session.tree.clone()))).collect()),
        };
        steps.map(History::Steps).ok_or_else(|| no_doc(kind))
    }

    fn change(ctx: &mut Ctx, args: &Json, change: Change) -> Outcome {
        let kind = target(ctx.app);
        if open_tree_state(ctx.app, kind).is_some() {
            return mutate_kind(ctx, args, Recipe::Steps(vec![Step::Document(change)]), kind);
        }
        match change {
            Change::Write { text, .. } => mutate_kind(ctx, args, Recipe::Write(text), kind),
            Change::Patch { edits, .. } => mutate_kind(ctx, args, Recipe::Edits(edits), kind),
            _ => fail(ToolError::refused("this document has only its root file; open a tree to create, rename or delete files")),
        }
    }

    fn undo(ctx: &mut Ctx, redo: bool) -> Outcome {
        undo(ctx, redo)
    }
}

impl ProductTools for StoryTools {
    fn library(ctx: &mut Ctx, job: LibraryJob) -> Outcome {
        slow(ctx, SlowJob::Library(job), Then::Reply, false)
    }

    fn status(ctx: &mut Ctx) -> Vec<(&'static str, Json)> {
        let edit = &ctx.app.video_cut;
        let page = match ctx.app.page {
            StoryPage::Cut => "cut",
            StoryPage::Deck => "deck",
        };
        let open = edit.session.as_ref().map(|session| {
            let doc = &session.doc;
            json::obj(vec![
                ("label", json::s(doc.label.clone())),
                ("document", document_identity(ctx.app, DocKind::Cut)),
                ("revision", Json::Int(session.revision as i64)),
                ("unsaved", Json::Bool(session.dirty())),
                ("output", json::obj(vec![("width", Json::Int(doc.output.width as i64)), ("height", Json::Int(doc.output.height as i64)), ("fps", num(doc.output.fps))])),
                ("duration", num(doc.duration)),
                ("bpm", num(doc.bpm)),
                ("blocks", Json::Int(doc.nodes.iter().filter(|node| node.span.is_some()).count() as i64)),
                ("undo", Json::Int(session.undo.len() as i64)),
            ])
        }).unwrap_or(Json::Null);
        let render = edit.render.as_ref().map(|render| json::obj(vec![("frame", Json::Int(render.progress.0 as i64)), ("frames", Json::Int(render.progress.1 as i64)), ("publishing", Json::Bool(render.publishing))])).unwrap_or(Json::Null);
        vec![("page", json::s(page)), ("cut", open), ("status", json::s(edit.status.clone())), ("render", render)]
    }
}

fn take_host() -> Box<Host> {
    HOST.with(|cell| cell.borrow_mut().take()).unwrap_or_default()
}

fn put_host(host: Box<Host>) {
    HOST.with(|cell| *cell.borrow_mut() = Some(host));
}

impl StoryApp {
    /// The control API's pump: worker replies, waiting calls and the
    /// page's own changes.
    pub(crate) fn pump_studio_api(&mut self, cx: &mut Cx) {
        let mut host = take_host();
        host.ensure_started(cx);
        progress(self, cx, &mut host);
        put_host(host);
    }

    /// Call a tool in process (the Generate loop). Returns the call's id;
    /// its result arrives in [`Self::studio_result`] (at once for most
    /// tools, frames later for changes, generation and app runs).
    pub fn studio_call(&mut self, cx: &mut Cx, name: &str, args: Json, origin: &str) -> u64 {
        let mut host = take_host();
        let id = host.call(self, cx, name, args, origin);
        put_host(host);
        id
    }

    /// A Motion document written in this session, by its name: its latest
    /// library revision (`asset_id`, `revision`).
    pub fn studio_motion(&self, name: &str) -> Option<(String, String)> {
        HOST.with(|cell| cell.borrow().as_ref().and_then(|host| host.state.motions.get(name).map(|(asset, _, _)| (asset.asset_id.to_string(), asset.revision.to_string()))))
    }

    pub fn studio_result(&mut self, id: u64) -> Option<CallResult> {
        HOST.with(|cell| cell.borrow_mut().as_mut().and_then(|host| host.take_result(id)))
    }

    /// How far a call still waiting on a generation job is: its stage and
    /// progress in permille, for the caller's progress line. None for any
    /// other call.
    pub fn studio_progress(&self, id: u64) -> Option<(String, u16)> {
        let job = HOST.with(|cell| cell.borrow().as_ref().and_then(|host| host.waiting.iter().find_map(|waiting| match &waiting.wait {
            Wait::Gen(wait) if waiting.reply == id => wait.job(),
            _ => None,
        })))?;
        gen::progress(&self.flow_studio, job)
    }

    /// The change feed since event `seq` (the Generate panel, a source
    /// view): every change to the open cut, by the API or by hand.
    pub fn studio_events_since(&self, seq: u64) -> Vec<ChangeEvent> {
        HOST.with(|cell| cell.borrow().as_ref().map(|host| host.log.since(seq).to_vec()).unwrap_or_default())
    }

    /// The tools with whether this Stage implements each (for a model's
    /// tool list).
    pub fn studio_tools(&self) -> Vec<(&'static ToolSpec, bool)> {
        Host::catalogue()
    }

    pub(crate) fn stop_studio_api(&mut self) {
        HOST.with(|cell| {
            if let Some(host) = cell.borrow_mut().as_mut() {
                host.state.edit = None;
                host.state.slow = None;
                host.state.runs = None;
            }
        });
    }
}

/// Everything that moves on its own: worker replies, installs, waits.
fn progress(app: &mut StoryApp, cx: &mut Cx, host: &mut Host) {
    if page_idle(&app.video_cut) && !app.video_cut.saving && !app.video_cut.save_requested && !host.waiting.iter().any(|waiting| matches!(waiting.wait, Wait::Cut { kind: DocKind::Cut, .. } | Wait::Slow { then: Then::UndoTree { kind: DocKind::Cut, .. }, .. })) {
        if let Some(redo) = app.video_cut.tree_undo.pop_front() {
            let mut ctx = Ctx { app, cx, host, origin: "page" };
            match undo_document(&mut ctx, redo, DocKind::Cut) {
                Outcome::Wait(wait) => ctx.host.waiting.push(Waiting { reply: 0, tool: "doc_undo", origin: "page".into(), wait }),
                Outcome::Done(result) => if let Err(error) = result { ctx.app.video_cut.status = error.message; ctx.app.video_cut.repaint = true; },
            }
        }
    }
    if !host.waiting.iter().any(|waiting| matches!(waiting.wait, Wait::Cut { kind: DocKind::Deck, .. })) {
        if let Some(operation) = app.deck.next_operation() {
            use super::super::document_tree::Operation;
            let mut ctx = Ctx { app, cx, host, origin: "page" };
            let outcome = match operation {
                Operation::Undo { redo } => undo_document(&mut ctx, redo, DocKind::Deck),
                operation => {
                    let steps = match operation {
                        Operation::Document(change) => vec![Step::Document(change)],
                        Operation::Deck(op) => vec![Step::Deck(op)],
                        Operation::DeleteFile { path } => {
                            let mut steps = Vec::new();
                            if let Some(session) = ctx.app.deck.session() {
                                for (index, slide) in session.doc.slides.iter().enumerate().rev() {
                                    if slide.graphic.as_deref() == Some(path.as_str()) {
                                        steps.push(Step::Deck(makepad_story_deck::ops::DeckOp::DeleteSlide { index }));
                                    }
                                }
                            }
                            steps.push(Step::Document(Change::Delete { path }));
                            steps
                        }
                        Operation::Import { reference, path, op } => vec![Step::Document(Change::Import { reference, path }), Step::Deck(op)],
                        Operation::Undo { .. } => unreachable!(),
                    };
                    mutate_kind(&mut ctx, &json::obj(vec![]), Recipe::Steps(steps), DocKind::Deck)
                }
            };
            match outcome {
                Outcome::Wait(wait) => ctx.host.waiting.push(Waiting { reply: 0, tool: "doc_patch", origin: "page".into(), wait }),
                Outcome::Done(result) => ctx.app.deck.edit_finished(result.err().map(|e| e.message)),
            }
        }
    }
    for worker in [host.state.slow.as_mut(), host.state.runs.as_mut()].into_iter().flatten() {
        worker.flush();
    }
    if let Some(worker) = &mut host.state.edit {
        worker.flush();
        for _ in 0..REPLIES_PER_PUMP {
            let Some(done) = worker.try_recv() else { break };
            for waiting in &mut host.waiting {
                match &mut waiting.wait {
                    Wait::Preview { ticket, done: slot, .. } if *ticket == done.ticket => *slot = Some(done.result.clone()),
                    Wait::Cut { ticket, done: slot, .. } if *ticket == done.ticket => {
                        *slot = Some(done.result.clone());
                    }
                    Wait::Open { ticket, done: slot, .. } if *ticket == done.ticket => {
                        *slot = Some(done.result.clone().and_then(|(source, doc)| doc.into_cut().map(|doc| (source, doc))));
                    }
                    _ => {}
                }
            }
        }
    }
    let mut slow_done = Vec::new();
    for worker in [host.state.slow.as_mut(), host.state.runs.as_mut()].into_iter().flatten() {
        for _ in 0..REPLIES_PER_PUMP {
            match worker.try_recv() {
                Some(done) => slow_done.push(done),
                None => break,
            }
        }
    }
    for done in slow_done {
        let Some(index) = host.waiting.iter().position(|waiting| matches!(waiting.wait, Wait::Slow { ticket, .. } if ticket == done.ticket)) else { continue };
        let waiting = host.waiting.remove(index);
        let Wait::Slow { then, .. } = waiting.wait else { unreachable!() };
        match (then, done.result) {
            (Then::UndoTree { revision, kind }, Ok(SlowOut::Deck(previous))) => {
                let result = if open_tree_state(app, kind).is_none_or(|(_, _, current, _)| current != revision) {
                    Err(ToolError::refused("the tree changed while undo loaded; try again"))
                } else {
                    let before = open_tree_state(app, kind).map(|(tree, ..)| tree.clone());
                    let restored = match kind {
                        DocKind::Deck => app.deck.restore_previous(previous),
                        DocKind::Cut => previous.doc.into_cut().and_then(|doc| {
                            app.video_cut.session.as_mut().unwrap().restore_previous(previous.draft, previous.path, doc, previous.head)?;
                            app.video_cut.document_changed();
                            Ok(())
                        }),
                    };
                    restored.map_err(ToolError::refused).map(|()| {
                        let (tree, _, revision, _) = open_tree_state(app, kind).unwrap();
                        tree_output(host, "doc_undo", &waiting.origin, revision, before.as_deref(), tree)
                    })
                };
                if waiting.reply == 0 {
                    match kind {
                        DocKind::Deck => app.deck.edit_finished(result.as_ref().err().map(|e| e.message.clone())),
                        DocKind::Cut => if let Err(error) = &result { app.video_cut.status = error.message.clone(); app.video_cut.repaint = true; },
                    }
                }
                host.finish(waiting.reply, result);
            }
            (Then::Reply, Ok(SlowOut::Json(value))) => host.finish(waiting.reply, Ok(ToolOutput::json(value))),
            (Then::Reply, Ok(SlowOut::Png(value, png))) => {
                let label = value.get("title").and_then(Json::as_str).unwrap_or("preview").to_string();
                host.finish(waiting.reply, Ok(ToolOutput::json(value).with_image(Image { png, label })));
            }
            (Then::Open { asset }, Ok(SlowOut::Source(source))) => {
                let ticket = host.next();
                if let Some(worker) = &mut host.state.edit {
                    worker.send(CutJob { ticket, kind: DocKind::Cut, base: source, steps: Vec::new() });
                }
                host.waiting.push(Waiting { wait: Wait::Open { ticket, head: Some(asset), saved: true, done: None }, ..waiting });
            }
            (Then::OpenDeck, Ok(SlowOut::Deck(opened))) => {
                let ui = app.ui.clone();
                match app.deck.open_library(cx, &ui, opened) {
                    Ok(()) => host.finish(waiting.reply, Ok(ToolOutput::json(deck_json(app)))),
                    Err(error) => host.finish(waiting.reply, Err(ToolError::refused(error))),
                }
            }
            (Then::OpenProject { then }, Ok(SlowOut::Deck(opened))) => {
                let ui = app.ui.clone();
                let result = match opened.doc.clone() {
                    TreeDoc::Cut(doc) => {
                        if !page_idle(&app.video_cut) || app.video_cut.saving || app.video_cut.save_requested { Err("The current cut is still being saved or edited".into()) }
                        else {
                            let source = opened.draft.text(&opened.path).map(str::to_string);
                            source.map(|source| {
                                if app.page != StoryPage::Cut { app.set_page(cx, StoryPage::Cut); }
                                app.video_cut.finish(cx, &ui, PagePending::Open { source: source.clone(), head: Some(opened.head), saved: true, tree: Some(opened), folder: None }, Ok((source, doc, None)));
                            })
                        }
                    }
                    TreeDoc::Deck(_) if then.is_none() => Err("open_cut requires a cut file path".into()),
                    TreeDoc::Deck(_) => {
                        if app.page != StoryPage::Deck { app.set_page(cx, StoryPage::Deck); }
                        app.deck.open_library(cx, &ui, opened)
                    }
                };
                match result {
                    Ok(()) => match then {
                        Some((tool, args)) => host.dispatch(app, cx, &tool, args, &waiting.origin, waiting.reply),
                        None => host.finish(waiting.reply, Ok(ToolOutput::json(document_identity(app, target(app))))),
                    }
                    Err(error) => host.finish(waiting.reply, Err(ToolError::refused(error))),
                }
            }
            (Then::AppRun { job }, result) => {
                host.state.app_runs.insert(job, match result {
                    Ok(SlowOut::Json(value)) => AppRunState::Done(value),
                    Ok(_) => AppRunState::Failed("unexpected reply".into()),
                    Err(error) => AppRunState::Failed(error.message),
                });
            }
            (Then::Motion { name, title, source, previous }, Ok(SlowOut::Json(value))) => {
                let published = value.get("asset").and_then(|asset| reference(Some(asset), "asset").ok());
                let Some(published) = published else {
                    // Refused by the loader: its diagnostics are the answer.
                    host.finish(waiting.reply, Err(ToolError::refused(motion_errors(&value))));
                    continue;
                };
                host.state.motions.insert(name.clone(), (published, title, source.clone()));
                let old = previous.map(|previous| format!("asset(\"{}\", \"{}\")", previous.asset_id, previous.revision));
                let session = app.video_cut.session.as_ref();
                match (old, session) {
                    (Some(old), Some(session)) if session.source.contains(&old) => {
                        // The cut's Graphics move to the new revision: one undoable change.
                        let new = format!("asset(\"{}\", \"{}\")", published.asset_id, published.revision);
                        let base = session.source.clone();
                        let recipe = Recipe::Repoint { old, new };
                        let steps = recipe.steps(&base).unwrap_or_default();
                        let ticket = host.next();
                        if let Some(worker) = &mut host.state.edit {
                            worker.send(CutJob { ticket, kind: DocKind::Cut, base: base.clone(), steps });
                        }
                        host.waiting.push(Waiting { wait: Wait::Cut { ticket, kind: DocKind::Cut, recipe, base, base_revision: None, done: None, extra: Some(value) }, ..waiting });
                    }
                    _ => {
                        let mut output = ToolOutput::json(value);
                        output.changes.push(DocChange { key: format!("motion:{}", published.asset_id), label: name, before: String::new(), after: source });
                        host.finish(waiting.reply, Ok(output));
                    }
                }
            }
            (Then::Bind { clip, overlay }, Ok(SlowOut::Json(value))) => {
                let Some(asset) = value.get("overlay_asset").and_then(|asset| reference(Some(asset), "overlay_asset").ok()) else {
                    host.finish(waiting.reply, Err(ToolError::failed("the overlay was not published")));
                    continue;
                };
                if let Some(source) = bind_tools::overlay(&overlay).map(|found| found.source) {
                    host.state.motions.insert(format!("overlay_{overlay}"), (asset, format!("Overlay · {overlay}"), source.to_string()));
                }
                let spans: Vec<(f64, f64)> = value.get("spans").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|span| {
                    let span = span.as_arr()?;
                    Some((json_num(span.first()?)?, json_num(span.get(1)?)?))
                }).collect();
                let params: Vec<(String, String)> = value.get("params").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|pair| {
                    let pair = pair.as_arr()?;
                    Some((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()))
                }).collect();
                let Some(session) = app.video_cut.session.as_ref() else {
                    host.finish(waiting.reply, Err(no_edit()));
                    continue;
                };
                let src = format!("asset(\"{}\", \"{}\")", asset.asset_id, asset.revision);
                let recipe = Recipe::Steps(vec![Step::Graphics { src, clip, spans, params }]);
                let base = session.source.clone();
                let steps = recipe.steps(&base).unwrap_or_default();
                let ticket = host.next();
                if let Some(worker) = &mut host.state.edit {
                    worker.send(CutJob { ticket, kind: DocKind::Cut, base: base.clone(), steps });
                }
                let extra = json::obj(vec![("overlay", value.get("overlay_asset").cloned().unwrap_or(Json::Null)), ("spans", value.get("spans").cloned().unwrap_or(Json::Null)), ("notes", value.get("notes").cloned().unwrap_or(Json::Null))]);
                host.waiting.push(Waiting { wait: Wait::Cut { ticket, kind: DocKind::Cut, recipe, base, base_revision: None, done: None, extra: Some(extra) }, ..waiting });
            }
            (Then::MotionSource { name }, Ok(SlowOut::Source(source))) => {
                host.finish(waiting.reply, Ok(ToolOutput::json(json::obj(vec![("name", json::s(name)), ("source", json::s(source))]))));
            }
            (Then::UndoTree { kind, .. }, Err(error)) => {
                if waiting.reply == 0 {
                    match kind {
                        DocKind::Deck => app.deck.edit_finished(Some(error.message.clone())),
                        DocKind::Cut => { app.video_cut.status = error.message.clone(); app.video_cut.repaint = true; }
                    }
                }
                host.finish(waiting.reply, Err(error));
            }
            (_, Err(error)) => {
                if waiting.reply == 0 { app.deck.edit_finished(Some(error.message.clone())); }
                host.finish(waiting.reply, Err(error));
            },
            (_, Ok(_)) => host.finish(waiting.reply, Err(ToolError::failed("unexpected library reply"))),
        }
    }
    let now = Cx::time_now();
    let mut index = 0;
    while index < host.waiting.len() {
        match step_wait(app, cx, host, index, now) {
            Some(result) => {
                let waiting = host.waiting.remove(index);
                if waiting.reply == 0 { app.deck.edit_finished(result.as_ref().err().map(|error| error.message.clone())); }
                host.finish(waiting.reply, result);
            }
            None => index += 1,
        }
    }
    observe_page(app, host);
}

/// The page has no change of its own in flight.
fn page_idle(edit: &VideoCut) -> bool {
    edit.in_flight.is_none() && edit.queue.is_empty()
}

/// One waiting call's progress: `Some` when it is answered.
fn step_wait(app: &mut StoryApp, cx: &mut Cx, host: &mut Host, index: usize, now: f64) -> Option<CallResult> {
    let tool = host.waiting[index].tool;
    let origin = host.waiting[index].origin.clone();
    match &mut host.waiting[index].wait {
        Wait::Preview { revision, done, rendering, .. } => {
            let view = app.ui.widget(cx, ids!(deck_view));
            if app.deck.session().is_none_or(|session| session.revision != *revision) || app.page != StoryPage::Deck {
                if let Some(mut view) = view.borrow_mut::<super::super::deck::view::DeckView>() { view.cancel_preview(); }
                return Some(Err(ToolError::refused("the deck changed during preview; preview the new tree")));
            }
            if !*rendering {
                let result = done.as_ref()?;
                let doc = match result {
                    Ok((_, OpenDoc::Tree { doc: TreeDoc::Deck(doc), .. })) => doc.clone(),
                    Err(error) => return Some(Ok(ToolOutput::json(json::obj(vec![
                        ("clean", Json::Bool(false)), ("frames", Json::Arr(Vec::new())),
                        ("diagnostics", Json::Arr(vec![json::obj(vec![("path", json::s(root_path(app))), ("severity", json::s("error")), ("message", json::s(error.clone()))])])),
                    ])))),
                    _ => return Some(Err(ToolError::failed("preview did not load a deck tree"))),
                };
                let Some(mut view) = view.borrow_mut::<super::super::deck::view::DeckView>() else { return Some(Err(ToolError::failed("the deck view is unavailable"))) };
                if let Err(error) = view.start_preview(cx, doc) { return Some(Err(ToolError::refused(error))); }
                *rendering = true;
                return None;
            }
            view.borrow_mut::<super::super::deck::view::DeckView>().and_then(|mut view| view.take_preview()).map(|result| Ok(ToolOutput::json(result)))
        }
        Wait::Cut { done, recipe, base, base_revision, ticket, extra, kind } => {
            let kind = *kind;
            let result = done.as_ref()?.clone();
            if !doc_idle(app, kind) {
                return None;
            }
            let Some((current, revision)) = doc_state(app, kind) else {
                return Some(Err(ToolError::not_found(format!("the {} was closed before the change landed", kind.noun()))));
            };
            if current != *base || base_revision.is_some_and(|expected| expected != revision) {
                // The person (or another call) changed the document meanwhile.
                if let Some(expected) = base_revision {
                    return Some(Err(ToolError::refused(format!("the {} changed while this call was checked (revision {expected} → {revision}): call doc_read and write again", kind.noun()))));
                }
                let steps = match prepared_steps(app, kind, recipe, &current) {
                    Ok(steps) => steps,
                    Err(error) => return Some(Err(error)),
                };
                let fresh = host.ticket + 1;
                host.ticket = fresh;
                *ticket = fresh;
                *base = current.clone();
                *done = None;
                if let Some(worker) = &mut host.state.edit {
                    worker.send(CutJob { ticket: fresh, kind, base: current, steps });
                }
                return None;
            }
            let extra = extra.take();
            if let Ok((source, _)) = &result {
                if *source == current && !matches!(&result, Ok((_, OpenDoc::Tree { tree, .. })) if open_tree_state(app, kind).map(|(tree, ..)| tree) != Some(tree)) {
                    return Some(Ok(ToolOutput::json(json::obj(vec![("revision", Json::Int(revision as i64)), ("changed", Json::Arr(Vec::new())), ("note", json::s(format!("nothing changed: the {} already is that way", kind.noun())))]))));
                }
            }
            Some(match result {
                Ok((source, doc)) => {
                    let mut output = install(app, host, tool, &origin, source, doc);
                    if let (Some(Json::Obj(extra)), Json::Obj(fields)) = (extra, &mut output.json) {
                        fields.extend(extra);
                    }
                    Ok(output)
                }
                Err(error) => Err(refusal(&error)),
            })
        }
        Wait::Open { done, head, saved, .. } => {
            let result = done.as_ref()?.clone();
            if !page_idle(&app.video_cut) {
                return None;
            }
            let (head, saved) = (*head, *saved);
            Some(match result {
                Ok((source, doc)) => {
                    let before = app.video_cut.session.as_ref().map(|session| session.source.clone()).unwrap_or_default();
                    app.video_cut.crumbs.clear();
                    note_author(&mut app.video_cut, &origin, tool);
                    // The page first: it is built here if it is not yet, and
                    // `finish` writes its widgets.
                    if app.page != StoryPage::Cut {
                        app.set_page(cx, StoryPage::Cut);
                    }
                    let ui = app.ui.clone();
                    app.video_cut.finish(cx, &ui, PagePending::Open { source: source.clone(), head, saved, tree: None, folder: None }, Ok((source.clone(), doc, None)));
                    let revision = app.video_cut.session.as_ref().map_or(0, |session| session.revision);
                    let patch = diff::unified(&before, &source, 2);
                    host.log.push(revision, tool, &origin, format!("{tool}: {}", app.video_cut.session.as_ref().map(|session| session.doc.label.clone()).unwrap_or_default()), patch.clone());
                    host.state.seen = Some((revision, source.clone()));
                    let mut output = ToolOutput::json(json::obj(vec![("revision", Json::Int(revision as i64)), ("diff", json::s(patch)), ("note", json::s(app.video_cut.status.clone()))]));
                    output.changes.push(DocChange { key: "cut.splash".into(), label: tool.into(), before, after: source });
                    Ok(output)
                }
                Err(error) => Err(refusal(&error)),
            })
        }
        Wait::Slow { .. } => None,
        Wait::Save { revision } => {
            let edit = &app.video_cut;
            if edit.saving || edit.save_requested || !page_idle(edit) {
                return None;
            }
            let session = edit.session.as_ref()?;
            if let Some(error) = &edit.save_error {
                return Some(Err(ToolError::failed(format!("not saved: {error}"))));
            }
            if session.dirty() && session.revision == *revision {
                return None;
            }
            Some(match (session.head, &edit.folder) {
                (Some(_), _) => Ok(ToolOutput::json(document_identity(app, DocKind::Cut))),
                (None, Some(folder)) => Ok(ToolOutput::json(json::obj(vec![("path", json::s(session.path.clone())), ("folder", json::s(folder.doc.dir.display().to_string()))]))),
                (None, None) => Err(ToolError::failed(edit.status.clone())),
            })
        }
        Wait::Speak { node } => {
            let edit = &app.video_cut;
            if edit.speaking.iter().any(|(speaking, _)| speaking == node) || !page_idle(edit) {
                return None;
            }
            let node = node.clone();
            Some(Ok(ToolOutput::json(voice_lines_json(edit, Some(&node)))))
        }
        Wait::Gen(wait) => {
            if gen::pending::<StoryTools>(app, wait, now) {
                return None;
            }
            let wait = wait.clone();
            match gen::answer(&mut Ctx { app, cx, host, origin: &origin }, wait)? {
                Outcome::Done(result) => Some(result),
                Outcome::Wait(wait) => {
                    host.waiting[index].wait = wait;
                    None
                }
            }
        }
        Wait::Storyboard { request } => super::storyboard::tool_reply(*request),
        Wait::Variants(pending) => super::motion_edit::variants::tool_reply(&app.video_cut, pending).map(|answer| answer.map(ToolOutput::json).map_err(ToolError::refused)),
        Wait::WebExport { id } => Some(app.video_cut.web_export_api_answer(*id)?.map(ToolOutput::json).map_err(ToolError::failed)),
        Wait::Generate { request, before, answer } => {
            if answer.is_none() {
                *answer = Some(StoryApp::generate_tool_reply(*request)?);
            }
            // A new Generate node is answered once it is in the page's edit
            // (G1 installs it through the page after it replies).
            if let Some(Ok(value)) = answer.as_ref() {
                if let Some(node) = value.get("node").and_then(Json::as_str) {
                    let landed = app.video_cut.session.as_ref().is_some_and(|session| session.doc.generate(node).is_some());
                    if tool == "generate_add" && (!landed || !page_idle(&app.video_cut)) {
                        return None;
                    }
                }
            }
            let answer = answer.take().unwrap();
            let before = before.take();
            Some(match answer {
                Ok(value) => {
                    let mut output = ToolOutput::json(value);
                    // A change G1 wrote into the page: logged like the API's own.
                    if let (Some((revision, before)), Some(session)) = (before, app.video_cut.session.as_ref()) {
                        if session.revision != revision && session.source != before {
                            let patch = diff::unified(&before, &session.source, 2);
                            host.log.push(session.revision, tool, &origin, format!("{} by {}", tool.replace('_', " "), origin_label(&origin)), patch.clone());
                            host.state.seen = Some((session.revision, session.source.clone()));
                            if let Json::Obj(fields) = &mut output.json {
                                fields.push(("revision".into(), Json::Int(session.revision as i64)));
                                fields.push(("diff".into(), json::s(patch)));
                            }
                            output.changes.push(DocChange { key: "cut.splash".into(), label: tool.into(), before, after: session.source.clone() });
                        }
                    }
                    Ok(output)
                }
                Err(error) => Err(ToolError::refused(error)),
            })
        }
        Wait::AppRun { job, deadline } => {
            let (job, deadline) = (*job, *deadline);
            match host.state.app_runs.get(&job) {
                Some(AppRunState::Running { .. }) if now < deadline => None,
                _ => Some(Ok(ToolOutput::json(app_run_json(host, job)))),
            }
        }
    }
}

/// The source and revision of the open document of `kind`.
fn doc_state(app: &StoryApp, kind: DocKind) -> Option<(String, u64)> {
    match kind {
        DocKind::Cut => app.video_cut.session.as_ref().map(|session| (session.source.clone(), session.revision)),
        DocKind::Deck => app.deck.session().map(|session| (session.source.clone(), session.revision)),
    }
}

/// The page that holds a document of `kind` has no change of its own in
/// flight.
fn doc_idle(app: &StoryApp, kind: DocKind) -> bool {
    match kind {
        DocKind::Cut => page_idle(&app.video_cut),
        DocKind::Deck => true,
    }
}

/// The document tool calls act on: the deck while the DECK page is
/// shown, else the edit.
fn target(app: &StoryApp) -> DocKind {
    if app.page == StoryPage::Deck {
        DocKind::Deck
    } else {
        DocKind::Cut
    }
}

/// Put a change into the page: one undo step, the viewer and timeline
/// updated, the change logged.
fn install(app: &mut StoryApp, host: &mut Host, tool: &str, origin: &str, source: String, doc: OpenDoc) -> ToolOutput {
    match doc {
        OpenDoc::Cut(doc) => install_edit(app, host, tool, origin, source, doc),
        OpenDoc::Deck(doc) => install_deck(app, host, tool, origin, source, doc),
        OpenDoc::Tree { doc, tree, path } => {
            let (before_tree, revision) = match doc {
                TreeDoc::Deck(doc) => {
                    let session = app.deck.session_mut().expect("checked by caller");
                    let before = session.tree.clone();
                    session.install_tree(source, doc, Some(tree.clone()));
                    session.path = path;
                    let revision = session.revision;
                    app.deck.document_changed(format!("{} · {}", origin_label(origin), tool.replace('_', " ")));
                    (before, revision)
                }
                TreeDoc::Cut(doc) => {
                    let session = app.video_cut.session.as_mut().expect("checked by caller");
                    let before = session.tree.clone();
                    session.install_tree(source, doc, Some(tree.clone()));
                    session.path = path;
                    let revision = session.revision;
                    app.video_cut.document_changed();
                    (before, revision)
                }
            };
            tree_output(host, tool, origin, revision, before_tree.as_deref(), &tree)

        }
    }
}

fn tree_output(host: &mut Host, tool: &str, origin: &str, revision: u64, before: Option<&makepad_asset_data::tree::TreeDraft>, after: &makepad_asset_data::tree::TreeDraft) -> ToolOutput {
    let paths: std::collections::BTreeSet<_> = before.into_iter().flat_map(|tree| tree.files.keys()).chain(after.files.keys()).collect();
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for path in paths {
        let old = before.and_then(|tree| tree.files.get(path));
        let new = after.files.get(path);
        if old.map(|file| (file.kind, &file.bytes)) == new.map(|file| (file.kind, &file.bytes)) { continue; }
        let old_text = old.filter(|file| !matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio)).and_then(|file| file.text().ok()).unwrap_or("");
        let new_text = new.filter(|file| !matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio)).and_then(|file| file.text().ok()).unwrap_or("");
        patches.push(format!("{path}\n{}", diff::unified(old_text, new_text, 2)));
        changes.push(DocChange { key: path.clone(), label: tool.into(), before: old_text.into(), after: new_text.into() });
    }
    host.log.push(revision, tool, origin, tool.replace('_', " "), patches.join("\n"));
    let mut output = ToolOutput::json(json::obj(vec![("path", json::s(after.root.1.clone())), ("revision", Json::Int(revision as i64)), ("changed", Json::Arr(changes.iter().map(|change| json::s(change.key.clone())).collect())), ("diagnostics", Json::Arr(Vec::new()))]));
    output.changes = changes;
    output
}

/// The change feed entry and the answer of an installed change.
fn installed(host: &mut Host, kind: DocKind, tool: &str, origin: &str, revision: u64, before: String, source: String, changed: Vec<Json>, removed: Vec<Json>) -> ToolOutput {
    let path = match kind { DocKind::Cut => "cut.splash", DocKind::Deck => "deck.splash" };
    let patch = diff::unified(&before, &source, 2);
    host.log.push(revision, tool, origin, format!("{} by {}", tool.replace('_', " "), origin_label(origin)), patch.clone());
    if kind == DocKind::Cut {
        host.state.seen = Some((revision, source.clone()));
    }
    let mut output = ToolOutput::json(json::obj(vec![
        ("path", json::s(path)),
        ("revision", Json::Int(revision as i64)),
        ("changed", Json::Arr(changed)),
        ("removed", Json::Arr(removed)),
        ("diff", json::s(patch)),
    ]));
    output.changes.push(DocChange { key: path.into(), label: tool.into(), before, after: source });
    output
}

fn install_deck(app: &mut StoryApp, host: &mut Host, tool: &str, origin: &str, source: String, doc: Arc<makepad_story_deck::DeckDoc>) -> ToolOutput {
    let status = format!("{} · {}", origin_label(origin), tool.replace('_', " "));
    let session = app.deck.session_mut().expect("checked by the caller");
    let old = session.doc.clone();
    let (before, _) = session.install(source.clone(), doc.clone());
    let revision = session.revision;
    // A slide is changed when its written text is not one the old file had.
    let text = |source: &str, slide: &makepad_story_deck::Slide| source[slide.span.0..slide.span.1].to_string();
    let changed: Vec<Json> = doc.slides.iter().filter(|slide| old.slides.iter().find(|o| o.name == slide.name).map_or(true, |o| text(&before, o) != text(&source, slide))).map(|slide| json::s(slide.name.clone())).collect();
    let removed: Vec<Json> = old.slides.iter().filter(|o| !doc.slides.iter().any(|slide| slide.name == o.name)).map(|o| json::s(o.name.clone())).collect();
    app.deck.document_changed(status);
    installed(host, DocKind::Deck, tool, origin, revision, before, source, changed, removed)
}

fn install_edit(app: &mut StoryApp, host: &mut Host, tool: &str, origin: &str, source: String, doc: Arc<CutDoc>) -> ToolOutput {
    note_author(&mut app.video_cut, origin, tool);
    let edit = &mut app.video_cut;
    let session = edit.session.as_mut().expect("checked by the caller");
    let old_doc = session.doc.clone();
    let (before, _) = session.install(source.clone(), doc.clone());
    let revision = session.revision;
    // What the change made is selected, as after the page's own adds.
    let added: Vec<&CutNode> = doc.nodes.iter().filter(|node| old_doc.node(&node.id).is_none()).collect();
    if let Some(node) = added.iter().find(|node| node.span.is_some() || matches!(node.kind, CutNodeKind::VoiceOver { .. })) {
        edit.selected = Some(if matches!(node.kind, CutNodeKind::VoiceOver { .. }) { format!("{}#0", node.id) } else { node.id.clone() });
    }
    let changed: Vec<Json> = doc.nodes.iter().filter(|node| old_doc.node(&node.id) != Some(*node)).map(|node| json::s(node.id.clone())).collect();
    let removed: Vec<Json> = old_doc.nodes.iter().filter(|node| doc.node(&node.id).is_none()).map(|node| json::s(node.id.clone())).collect();
    edit.status = format!("{} · {}", origin_label(origin), tool.replace('_', " "));
    edit.status_sticky = true;
    edit.document_changed();
    installed(host, DocKind::Cut, tool, origin, revision, before, source, changed, removed)
}

/// Name the change about to land in the page's history (M5's Source view
/// shows it instead of "Cut").
fn note_author(page: &mut VideoCut, origin: &str, tool: &str) {
    super::motion_edit::note_author(page, &format!("{} · {}", origin_label(origin), tool.replace('_', " ")));
}

fn origin_label(origin: &str) -> &'static str {
    match origin {
        "cli" => "CLI",
        "generate" => "Generate",
        "chat" => "AI (chat)",
        "mcp" => "AI (MCP)",
        _ => "API",
    }
}

/// A person's change (or an undo) since the last look: logged as `page`.
fn observe_page(app: &StoryApp, host: &mut Host) {
    let Some(session) = app.video_cut.session.as_ref() else { return };
    match &host.state.seen {
        Some((revision, _)) if *revision == session.revision => {}
        Some((_, source)) if *source == session.source => host.state.seen = Some((session.revision, session.source.clone())),
        seen => {
            let before = seen.as_ref().map(|(_, source)| source.as_str()).unwrap_or("");
            let patch = diff::unified(before, &session.source, 2);
            if seen.is_some() {
                host.log.push(session.revision, "page", "page", "changed in the CUT page".into(), patch);
            }
            host.state.seen = Some((session.revision, session.source.clone()));
        }
    }
}

/// An op's refusal, with the page's diagnostics, and where to look.
fn refusal(error: &str) -> ToolError {
    let hint = if error.contains("no node") || error.contains("not a block") || error.contains("has no span") || error.contains("`") && error.contains("unknown") {
        " (call timeline for the node ids)"
    } else if error.contains(':') && error.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        " (line:column in the cut's source; call doc_read)"
    } else {
        ""
    };
    ToolError::refused(format!("{error}{hint}"))
}

// ------------------------------------------------------------ helpers

fn ids(reference: &AssetRevisionRef) -> AssetRef {
    AssetRef { id: reference.asset_id.to_string(), rev: reference.revision.to_string() }
}

fn no_doc(kind: DocKind) -> ToolError {
    match kind {
        DocKind::Cut => no_edit(),
        DocKind::Deck => ToolError::not_found("no deck is open in the DECK page: call new_deck to make one (or open_deck for a saved one)"),
    }
}

fn no_edit() -> ToolError {
    ToolError::not_found("no cut is open in the CUT page: call new_cut, or open_cut with a cut from search_library kind `cut`")
}

fn session_doc(ctx: &Ctx) -> Result<Arc<CutDoc>, ToolError> {
    ctx.app.video_cut.session.as_ref().map(|session| session.doc.clone()).ok_or_else(no_edit)
}

fn node_exists(doc: &CutDoc, id: &str) -> Result<(), ToolError> {
    if doc.node(id).is_some() || doc.graphic(id).is_some() || doc.generate(id).is_some() {
        return Ok(());
    }
    let timed: Vec<String> = doc.nodes.iter().filter(|node| node.span.is_some()).map(|node| node.id.clone()).chain(doc.graphics.iter().map(|graphic| graphic.id.clone())).chain(doc.generates.iter().map(|gen| gen.id.clone())).collect();
    Err(ToolError::not_found(format!("the cut has no node `{id}`; its blocks are: {}", if timed.is_empty() { "none yet".to_string() } else { timed.join(", ") })))
}

/// Queue a change of the open document. The source tools act on the
/// document of the page shown; the cut's ops on the cut.
fn mutate(ctx: &mut Ctx, args: &Json, recipe: Recipe) -> Outcome {
    let kind = match recipe {
        Recipe::Write(_) | Recipe::Edits(_) => target(ctx.app),
        _ => DocKind::Cut,
    };
    mutate_kind(ctx, args, recipe, kind)
}

fn mutate_kind(ctx: &mut Ctx, args: &Json, recipe: Recipe, kind: DocKind) -> Outcome {
    let Some((base, revision)) = doc_state(ctx.app, kind) else { return fail(no_doc(kind)) };
    // A revision the caller read pins the change to it. Without one, only a
    // whole-text write is pinned to what is open now; a slide op or a patch
    // of named nodes re-applies on whatever another call (a model's parallel
    // calls) landed meanwhile, and is refused only if it no longer applies.
    let base_revision = args.get("base_revision").and_then(Json::as_u64).or_else(|| matches!(recipe, Recipe::Write(_) | Recipe::Edits(_)).then_some(revision));
    if let Some(expected) = base_revision {
        if expected != revision {
            return fail(ToolError::refused(format!("the {} is at revision {revision} now, not {expected} (it changed since you read it): call doc_read and write again", kind.noun())));
        }
    }
    let steps = match prepared_steps(ctx.app, kind, &recipe, &base) {
        Ok(steps) => steps,
        Err(error) => return fail(error),
    };
    let ticket = ctx.host.next();
    match &mut ctx.host.state.edit {
        Some(worker) => worker.send(CutJob { ticket, kind, base: base.clone(), steps }),
        None => return fail(ToolError::failed(ctx.host.error.clone().unwrap_or_else(|| "the cut worker is not running".into()))),
    }
    Outcome::Wait(Wait::Cut { ticket, kind, recipe, base, base_revision, done: None, extra: None })
}

fn open_tree_state(app: &StoryApp, kind: DocKind) -> Option<(&Arc<makepad_asset_data::tree::TreeDraft>, &str, u64, Option<AssetRevisionRef>)> {
    match kind {
        DocKind::Cut => { let session = app.video_cut.session.as_ref()?; Some((session.tree.as_ref()?, &session.path, session.revision, session.head)) }
        DocKind::Deck => { let session = app.deck.session()?; Some((session.tree.as_ref()?, &session.path, session.revision, session.head)) }
    }
}

fn project_name(app: &StoryApp, kind: DocKind) -> Option<&makepad_asset_data::AssetAlias> {
    match kind { DocKind::Cut => app.video_cut.session.as_ref()?.alias.as_ref(), DocKind::Deck => app.deck.session()?.alias.as_ref() }
}

fn document_identity(app: &StoryApp, kind: DocKind) -> Json {
    if let Some((_, path, revision, _)) = open_tree_state(app, kind) {
        return json::obj(vec![("project", project_name(app, kind).map(|alias| json::s(alias.to_string())).unwrap_or(Json::Null)), ("path", json::s(path)), ("revision", Json::Int(revision as i64))]);
    }
    let session = app.video_cut.session.as_ref();
    json::obj(vec![("cut", session.and_then(|session| session.head).map(|head| reference_json(&head)).unwrap_or(Json::Null)), ("revision", Json::Int(session.map_or(0, |session| session.revision) as i64))])
}

fn prepared_steps(app: &StoryApp, kind: DocKind, recipe: &Recipe, base: &str) -> Result<Vec<Step>, ToolError> {
    let mut steps = recipe.steps(base)?;
    if let Some((tree, path, _, _)) = open_tree_state(app, kind) {
        let deck = (kind == DocKind::Deck).then(|| app.deck.session().map(|session| session.doc.clone())).flatten();
        steps.insert(0, Step::Tree { tree: tree.clone(), path: path.into(), deck });
    }
    Ok(steps)
}

/// The cut's node tools. While the DECK page is shown they act on one
/// slide's cut (`slide`), as an ordinary cut, and the result is written back
/// into the deck.
fn ops_recipe(ctx: &mut Ctx, args: &Json, steps: Vec<Step>) -> Outcome {
    if target(ctx.app) == DocKind::Deck {
        return match slide_arg(args) {
            Some(slide) => mutate_kind(ctx, args, Recipe::Steps(vec![Step::Slide { slide, steps }]), DocKind::Deck),
            None => fail(need_slide()),
        };
    }
    if args.get("slide").is_some() {
        return fail(ToolError::invalid("`slide` is for the DECK page: call open_deck first"));
    }
    mutate(ctx, args, Recipe::Steps(steps))
}

fn slide_arg(args: &Json) -> Option<usize> {
    args.get("slide").and_then(Json::as_u64).map(|slide| slide as usize)
}

fn need_slide() -> ToolError {
    ToolError::invalid("the DECK page is shown: name the slide this acts on with `slide` (its 0-based index; doc_read lists the slides in order)")
}

/// The cut the node tools look at: the open cut, or while DECK is shown the
/// cut of slide `slide`.
fn work_doc(ctx: &Ctx, args: &Json) -> Result<Arc<CutDoc>, ToolError> {
    if target(ctx.app) != DocKind::Deck {
        return session_doc(ctx);
    }
    let index = slide_arg(args).ok_or_else(need_slide)?;
    let session = ctx.app.deck.session().ok_or_else(|| no_doc(DocKind::Deck))?;
    let slide = session.doc.slides.get(index).ok_or_else(|| ToolError::not_found("no such slide"))?;
    if let (Some(tree), Some(path)) = (&session.tree, &slide.graphic) {
        return super::super::document_tree::load(tree, path).and_then(TreeDoc::into_cut).map_err(ToolError::refused);
    }
    let extracted = makepad_story_deck::ops::extract(&session.source, &session.doc, index).map_err(ToolError::refused)?;
    match &session.tree { Some(tree) => CutDoc::load_tree(extracted.text(), tree.context(&session.path)), None => CutDoc::load(extracted.text()) }
        .map(Arc::new).map_err(|diags| ToolError::refused(diagnostics(diags)))
}

// ---------------------------------------------------------- DECK tools

fn deck_names(ctx: &Ctx) -> Vec<String> {
    ctx.app.deck.session().map(|session| session.doc.slides.iter().map(|slide| slide.name.clone()).collect()).unwrap_or_default()
}

fn index_arg(ctx: &Ctx, args: &Json, key: &str) -> Result<usize, ToolError> {
    let names = deck_names(ctx);
    let index = args.get(key).and_then(Json::as_u64).ok_or_else(|| ToolError::invalid(format!("`{key}` is a slide's 0-based index (the slides: {})", names.join(", "))))? as usize;
    if index >= names.len() {
        return Err(ToolError::not_found(format!("there is no slide {index}: the deck has {} (0 to {}): {}", names.len(), names.len().saturating_sub(1), names.join(", "))));
    }
    Ok(index)
}

fn parse_transition(value: &Json, what: &str) -> Result<makepad_story_deck::Transition, ToolError> {
    let seconds = number(value, "seconds").filter(|seconds| *seconds >= 0.0).ok_or_else(|| ToolError::invalid(format!("`{what}.seconds` is a number >= 0")))?;
    let effect = value.get("effect").and_then(Json::as_str).unwrap_or("").to_string();
    let mut dials = Vec::new();
    for dial in ["p1", "p2", "p3"] {
        if let Some(n) = number(value, dial) {
            dials.push((dial.to_string(), n));
        }
    }
    Ok(makepad_story_deck::Transition { seconds, effect, dials })
}

/// A field that is a transition, `null` (clear it) or absent.
fn transition_field(args: &Json, key: &str) -> Result<Option<Option<makepad_story_deck::Transition>>, ToolError> {
    match args.get(key) {
        None => Ok(None),
        Some(Json::Null) => Ok(Some(None)),
        Some(value) => parse_transition(value, key).map(|transition| Some(Some(transition))),
    }
}

fn deck_op(ctx: &mut Ctx, args: &Json, op: makepad_story_deck::ops::DeckOp) -> Outcome {
    if ctx.app.deck.session().is_none() {
        return fail(no_doc(DocKind::Deck));
    }
    mutate_kind(ctx, args, Recipe::Steps(vec![Step::Deck(op)]), DocKind::Deck)
}

/// `[a, b]` (or `[w, h]`) of a tool call.
fn pair(args: &Json, key: &str) -> Result<Option<(f64, f64)>, ToolError> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Arr(items)) if items.len() == 2 => match (json_num(&items[0]), json_num(&items[1])) {
            (Some(a), Some(b)) => Ok(Some((a, b))),
            _ => Err(ToolError::invalid(format!("`{key}` is [number, number]"))),
        },
        Some(_) => Err(ToolError::invalid(format!("`{key}` is [number, number]"))),
    }
}

/// The demo steps a tool call lists, as `set_demo` reads them.
fn demo_steps(list: &[Json]) -> Result<Vec<makepad_story_deck::ops::StepSpec>, ToolError> {
    let mut steps = Vec::new();
    for (at, step) in list.iter().enumerate() {
        let Json::Obj(fields) = step else { return Err(ToolError::invalid(format!("`steps[{at}]` is an object {{call: \"click\", target: \"@button\"}}"))) };
        let Some(call) = step.get("call").and_then(Json::as_str) else { return Err(ToolError::invalid(format!("`steps[{at}].call` is the step's name"))) };
        let mut values = Vec::new();
        for (key, value) in fields.iter().filter(|(key, _)| key != "call") {
            match step_lit(value) {
                Some(lit) => values.push((key.clone(), lit)),
                None => return Err(ToolError::invalid(format!("`steps[{at}].{key}` is a string (`\"@widget.path\"` for a widget), number, boolean or [x, y]"))),
            }
        }
        steps.push(makepad_story_deck::ops::StepSpec { call: call.to_string(), args: values });
    }
    Ok(steps)
}

/// The `Camera` a tool call describes: `device` (part of a name, an index,
/// "@test_pattern", or omitted for the first camera), `width`, `height`, `fps`, `crop` [x, y, w, h], `rotate`,
/// `mirror`, `adjust` {brightness, contrast, saturation}, `fit`.
fn camera_spec(args: &Json) -> Result<makepad_story_cut::CameraSpec, ToolError> {
    use makepad_story_cut::{CameraDevice, CameraSpec, Fit};
    let mut spec = CameraSpec::default();
    spec.device = match args.get("device") {
        None | Some(Json::Null) => CameraDevice::First,
        Some(Json::Str(name)) if name.trim() == "@test_pattern" => CameraDevice::TestPattern,
        Some(Json::Str(name)) if !name.trim().is_empty() => CameraDevice::Name(name.trim().to_string()),
        Some(Json::Int(index)) if (0..64).contains(index) => CameraDevice::Index(*index as u32),
        Some(_) => return Err(ToolError::invalid("`device` is part of the camera's name (\"OBSBOT\"), its number (0, 1, ...), or \"@test_pattern\" for generated colour bars. Omit it for the first camera")),
    };
    let whole = |key: &str, slot: &mut u32| -> Result<(), ToolError> {
        match number(args, key) {
            Some(value) if value >= 1.0 && value.fract() == 0.0 => {
                *slot = value as u32;
                Ok(())
            }
            Some(_) => Err(ToolError::invalid(format!("`{key}` is a whole number of pixels"))),
            None if args.get(key).is_none() => Ok(()),
            None => Err(ToolError::invalid(format!("`{key}` is a whole number of pixels"))),
        }
    };
    whole("width", &mut spec.width)?;
    whole("height", &mut spec.height)?;
    if let Some(fps) = number(args, "fps") {
        spec.fps = fps;
    }
    if let Some(value) = args.get("crop") {
        let items = value.as_arr().filter(|items| items.len() == 4).ok_or_else(|| ToolError::invalid("`crop` is [x, y, w, h] as fractions of the picture"))?;
        for (slot, item) in spec.crop.iter_mut().zip(items) {
            *slot = json_num(item).ok_or_else(|| ToolError::invalid("`crop` is [x, y, w, h] as fractions of the picture"))?;
        }
    }
    if let Some(degrees) = number(args, "rotate") {
        spec.rotate = degrees as u32;
        if ![0.0, 90.0, 180.0, 270.0].contains(&degrees) {
            return Err(ToolError::invalid("`rotate` is 0, 90, 180 or 270 (degrees clockwise)"));
        }
    }
    if let Some(mirror) = flag(args, "mirror") {
        spec.mirror = mirror;
    }
    if let Some(adjust) = args.get("adjust") {
        for (key, slot) in [("brightness", &mut spec.adjust.brightness), ("contrast", &mut spec.adjust.contrast), ("saturation", &mut spec.adjust.saturation)] {
            if let Some(value) = number(adjust, key) {
                *slot = value;
            }
        }
    }
    spec.fit = match text(args, "fit") {
        None | Some("fit") => Fit::Fit,
        Some("fill") => Fit::Fill,
        Some("none") => Fit::None,
        Some(_) => return Err(ToolError::invalid("a camera's `fit` is \"fit\" (letterbox, the default), \"fill\" (crop to cover) or \"none\"")),
    };
    Ok(spec)
}

fn add_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::{ImageFit, NewCut, DeckOp, SlideSet};
    let names = deck_names(ctx);
    let after = match args.get("after") {
        None | Some(Json::Null) => None,
        Some(_) => match index_arg(ctx, args, "after") {
            Ok(after) => Some(after),
            Err(error) => return fail(error),
        },
    };
    let name = text(args, "name").map(str::to_string).unwrap_or_else(|| (1..).map(|n| format!("slide{n}")).find(|name| !names.contains(name)).unwrap());
    let kind = text(args, "kind").unwrap_or(if args.get("path").is_some() { "graphic" } else if args.get("sketch").is_some() { "sketch" } else { "blank" });
    let need = |key: &str| text(args, key).map(str::to_string).ok_or_else(|| ToolError::invalid(format!("a `{kind}` slide needs `{key}`")));
    let cut = (|| -> Result<NewCut, ToolError> {
        Ok(match kind {
            "graphic" => NewCut::Graphic { path: need("path")? },
            "sketch" => NewCut::Sketch { text: text(args, "sketch").unwrap_or("").to_string() },
            "blank" => NewCut::Blank,
            "title" => NewCut::Title { title: need("title")?, subtitle: text(args, "subtitle").map(str::to_string) },
            "text" => NewCut::Text {
                title: need("title")?,
                bullets: args.get("bullets").and_then(Json::as_arr).map(|list| list.iter().filter_map(|item| item.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            },
            "image" => {
                let path = need("path")?;
                let fit = match text(args, "fit") {
                    None | Some("fill") => ImageFit::Fill,
                    Some("contain") => ImageFit::Contain,
                    Some(_) => return Err(ToolError::invalid("`fit` is \"fill\" or \"contain\"")),
                };
                NewCut::ImagePath { path, fit }
            }
            "video" => {
                let path = need("path")?;
                NewCut::VideoPath { path, play: pair(args, "play")?, seconds: number(args, "seconds"), audio: flag(args, "audio").unwrap_or(false) }
            }
            "app" => NewCut::App {
                app: need("app")?,
                size: pair(args, "size")?.unwrap_or(makepad_story_deck::AppDecl::DEFAULT_SIZE),
                demo: match args.get("demo").and_then(Json::as_arr) {
                    Some(list) => demo_steps(list)?,
                    None => Vec::new(),
                },
            },
            "camera" => NewCut::Camera(camera_spec(args)?),
            other => return Err(ToolError::invalid(format!("`kind` `{other}` is not a slide kind (kinds: {})", makepad_story_deck::ops::SLIDE_KINDS.join(", ")))),
        })
    })();
    let cut = match cut {
        Ok(cut) => cut,
        Err(error) => return fail(error),
    };
    if ctx.app.deck.session().is_none() {
        return fail(no_doc(DocKind::Deck));
    }
    let mut steps = vec![Step::Deck(DeckOp::AddSlide { after, name, cut })];
    let mut set = SlideSet { duration: args.get("duration").and_then(json_num), notes: args.get("notes").and_then(Json::as_str).map(str::to_string), ..SlideSet::default() };
    match transition_field(args, "enter") {
        Ok(enter) => set.enter = enter,
        Err(error) => return fail(error),
    }
    if set.notes.is_some() || set.enter.is_some() || set.duration.is_some() {
        steps.push(Step::Deck(DeckOp::SetSlide { index: after.map_or(names.len(), |after| after + 1), set }));
    }
    mutate_kind(ctx, args, Recipe::Steps(steps), DocKind::Deck)
}

/// A picture on top of a slide's picture, for the slide's whole length: a
/// tree image, muted video, Motion plate, or live camera.
fn add_layer(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::{ImageFit, NewCut, DeckOp};
    let index = match index_arg(ctx, args, "index") {
        Ok(index) => index,
        Err(error) => return fail(error),
    };
    let layer = (|| -> Result<NewCut, ToolError> {
        let path = || text(args, "path").map(str::to_string).ok_or_else(|| ToolError::invalid("give the layer’s tree path"));
        Ok(match text(args, "kind") {
            Some("graphic") => NewCut::Graphic { path: path()? },
            Some("camera") => NewCut::Camera(camera_spec(args)?),
            Some("image") => {
                let path = path()?;
                let fit = match text(args, "fit") {
                    None | Some("fill") => ImageFit::Fill,
                    Some("contain") => ImageFit::Contain,
                    Some(_) => return Err(ToolError::invalid("`fit` is \"fill\" or \"contain\"")),
                };
                NewCut::ImagePath { path, fit }
            }
            Some("video") => {
                let path = path()?;
                NewCut::VideoPath { path, play: pair(args, "play")?, seconds: None, audio: false }
            }
            _ => return Err(ToolError::invalid("`kind` is \"camera\", \"image\", \"video\" or \"graphic\"")),
        })
    })();
    match layer {
        Ok(layer) => deck_op(ctx, args, DeckOp::AddLayer { index, layer }),
        Err(error) => fail(error),
    }
}

/// A new deck from the template, published and open.
fn new_deck(ctx: &mut Ctx, args: &Json) -> Outcome {
    let title = text(args, "title").unwrap_or("New deck").to_string();
    if ctx.app.page != StoryPage::Deck {
        ctx.app.set_page(ctx.cx, StoryPage::Deck);
    }
    let ui = ctx.app.ui.clone();
    match ctx.app.deck.new_deck(ctx.cx, &ui, &title) {
        Ok(()) => done(deck_json(ctx.app)),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn move_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    let (from, to) = match (index_arg(ctx, args, "from"), index_arg(ctx, args, "to")) {
        (Ok(from), Ok(to)) => (from, to),
        (Err(error), _) | (_, Err(error)) => return fail(error),
    };
    deck_op(ctx, args, makepad_story_deck::ops::DeckOp::MoveSlide { from, to })
}

fn delete_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    match index_arg(ctx, args, "index") {
        Ok(index) => deck_op(ctx, args, makepad_story_deck::ops::DeckOp::DeleteSlide { index }),
        Err(error) => fail(error),
    }
}

fn duplicate_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    match index_arg(ctx, args, "index") {
        Ok(index) => deck_op(ctx, args, makepad_story_deck::ops::DeckOp::DuplicateSlide { index, name: text(args, "name").map(str::to_string) }),
        Err(error) => fail(error),
    }
}

fn set_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::{DeckOp, SlideSet};
    use makepad_story_deck::Loop;
    let index = match index_arg(ctx, args, "index") {
        Ok(index) => index,
        Err(error) => return fail(error),
    };
    let mut set = SlideSet { name: text(args, "name").map(str::to_string), duration: args.get("duration").and_then(json_num), notes: args.get("notes").and_then(Json::as_str).map(str::to_string), ..SlideSet::default() };
    if let Some(value) = args.get("beats") {
        let Some(beats) = value.as_arr().and_then(|items| items.iter().map(json_num).collect::<Option<Vec<_>>>()) else {
            return fail(ToolError::invalid("`beats` is a list of increasing seconds inside the cut; [] clears it"));
        };
        set.beats = Some(beats);
    }
    set.play = match args.get("play") {
        None => None,
        Some(Json::Str(mode)) if mode == "hold" => Some(Loop::Hold),
        Some(Json::Str(mode)) if mode == "loop" => Some(Loop::Whole),
        Some(Json::Arr(range)) if range.len() == 2 => match (json_num(&range[0]), json_num(&range[1])) {
            (Some(a), Some(b)) => Some(Loop::Range(a, b)),
            _ => return fail(ToolError::invalid("`play` is \"loop\", \"hold\" or [from, to] in seconds")),
        },
        Some(_) => return fail(ToolError::invalid("`play` is \"loop\", \"hold\" or [from, to] in seconds")),
    };
    for (key, slot) in [("enter", &mut set.enter), ("exit", &mut set.exit)] {
        match transition_field(args, key) {
            Ok(value) => *slot = value,
            Err(error) => return fail(error),
        }
    }
    set.sketch = text(args, "sketch").map(str::to_string);
    set.graphic = match args.get("graphic") {
        None => None,
        Some(Json::Null) => Some(None),
        Some(Json::Str(path)) if !path.trim().is_empty() => Some(Some(path.trim().to_string())),
        Some(_) => return fail(ToolError::invalid("`graphic` is a tree path (slides/<name>.splash), or null to take it off a sketch slide")),
    };
    set.target_seconds = match args.get("target_seconds") {
        None => None,
        Some(Json::Null) => Some(None),
        Some(value) => match json_num(value).filter(|n| *n > 0.0) {
            Some(n) => Some(Some(n)),
            None => return fail(ToolError::invalid("`target_seconds` is a number of seconds > 0, or null")),
        },
    };
    deck_op(ctx, args, DeckOp::SetSlide { index, set })
}

/// A demo step's value: `"@path"` is a widget path, other strings text,
/// numbers, booleans, `[x, y]` a point.
fn step_lit(value: &Json) -> Option<makepad_story_deck::ops::Lit> {
    use makepad_story_deck::ops::Lit;
    Some(match value {
        Json::Str(text) => match text.strip_prefix('@') {
            Some(id) => Lit::Id(id.to_string()),
            None => Lit::Str(text.clone()),
        },
        Json::Bool(b) => Lit::Bool(*b),
        Json::Arr(point) if point.len() == 2 => Lit::Point(json_num(&point[0])?, json_num(&point[1])?),
        other => Lit::Num(json_num(other)?),
    })
}

fn set_demo(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::DeckOp;
    let index = match index_arg(ctx, args, "index") {
        Ok(index) => index,
        Err(error) => return fail(error),
    };
    let Some(list) = args.get("steps").and_then(Json::as_arr) else { return fail(ToolError::invalid("`steps` is a list of {call, ...} (the calls: click, type, key, drag, scroll, wait_visible, wait_text, wait_settled, wait_state, hold, pause, mark)")) };
    let steps = match demo_steps(list) {
        Ok(steps) => steps,
        Err(error) => return fail(error),
    };
    deck_op(ctx, args, DeckOp::SetDemo { index, steps })
}

fn add_app(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::DeckOp;
    let index = match index_arg(ctx, args, "index") {
        Ok(index) => index,
        Err(error) => return fail(error),
    };
    let (Some(name), Some(app)) = (text(args, "name"), text(args, "app")) else { return fail(ToolError::invalid("`name` (what the slide calls the app) and `app` (a registry id) are needed")) };
    let size = match args.get("size").and_then(Json::as_arr) {
        Some(size) if size.len() == 2 => match (json_num(&size[0]), json_num(&size[1])) {
            (Some(w), Some(h)) => (w, h),
            _ => return fail(ToolError::invalid("`size` is [width, height] in points")),
        },
        _ => (900.0, 600.0),
    };
    let op = DeckOp::AddApp { index, name: name.to_string(), app: app.to_string(), size, audio: flag(args, "audio").unwrap_or(false) };
    if ctx.app.deck.session().is_none() {
        return fail(no_doc(DocKind::Deck));
    }
    // Declared, then placed as a layer on top of the slide's picture: one step.
    let steps = vec![Step::Deck(op), Step::Slide { slide: index, steps: vec![Step::Op(CutOp::AppLayer { app: name.to_string() })] }];
    mutate_kind(ctx, args, Recipe::Steps(steps), DocKind::Deck)
}

fn set_deck(ctx: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_deck::ops::DeckOp;
    let transition = match args.get("transition") {
        None => None,
        Some(value) => match parse_transition(value, "transition") {
            Ok(transition) => Some(transition),
            Err(error) => return fail(error),
        },
    };
    let output = match args.get("output") {
        None => None,
        Some(value) => match (value.get("width").and_then(Json::as_u64), value.get("height").and_then(Json::as_u64)) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u32, h as u32)),
            _ => return fail(ToolError::invalid("`output` is {width, height} in pixels")),
        },
    };
    deck_op(ctx, args, DeckOp::SetDeck { title: text(args, "title").map(str::to_string), transition, output })
}

fn slow(ctx: &mut Ctx, job: SlowJob, then: Then, runs: bool) -> Outcome {
    let ticket = ctx.host.next();
    let worker = if runs { ctx.host.state.runs.as_mut() } else { ctx.host.state.slow.as_mut() };
    match worker {
        Some(worker) => worker.send((ticket, job)),
        None => return fail(ToolError::failed(ctx.host.error.clone().unwrap_or_else(|| "the library worker is not running".into()))),
    }
    Outcome::Wait(Wait::Slow { ticket, then })
}

/// A JSON value as the patcher writes it: numbers, `@ids` and strings,
/// lists, `{keys: [[t, v, ease?], …]}`, objects.
fn patch_value(key: &str, value: &Json) -> Result<PValue, ToolError> {
    Ok(match value {
        Json::Int(value) => PValue::num(*value as f64),
        Json::F64(value) => PValue::num(*value),
        Json::Bool(value) => PValue::num(if *value { 1.0 } else { 0.0 }),
        Json::Null => PValue::nil(),
        Json::Str(text) => match text.strip_prefix('@') {
            Some(id) => PValue::id(id),
            None if matches!(key, "fit" | "blend" | "mode" | "safe" | "engine") => PValue::id(text),
            None => PValue::string(text),
        },
        Json::Arr(items) => PValue::list(items.iter().map(|item| patch_value(key, item)).collect::<Result<_, _>>()?),
        Json::Obj(fields) => match value.get("keys").and_then(Json::as_arr) {
            Some(keys) => {
                let mut points = Vec::new();
                for (index, key_value) in keys.iter().enumerate() {
                    let items = key_value.as_arr().unwrap_or(&[]);
                    let at = |i: usize| items.get(i).and_then(|item| match item {
                        Json::Int(value) => Some(*value as f64),
                        Json::F64(value) => Some(*value),
                        _ => None,
                    });
                    let (Some(t), Some(v)) = (at(0), at(1)) else {
                        return Err(ToolError::invalid(format!("`value.keys[{index}]` must be [t, v] or [t, v, \"ease\"] with numbers")));
                    };
                    let ease = items.get(2).and_then(Json::as_str).map(|ease| ease.trim_start_matches('@').to_string());
                    points.push((t, v, ease));
                }
                let points: Vec<(f64, f64, Option<&str>)> = points.iter().map(|(t, v, ease)| (*t, *v, ease.as_deref())).collect();
                PValue::keys(&points)
            }
            None => {
                let mut out = Vec::new();
                for (name, item) in fields {
                    out.push((name.as_str(), patch_value(name, item)?));
                }
                PValue::object(out)
            }
        },
    })
}

fn voice_lines(args: &Json) -> Result<Vec<VoiceLine>, ToolError> {
    let items = args.get("lines").and_then(Json::as_arr).unwrap_or(&[]);
    items.iter().enumerate().map(|(index, line)| {
        let text = line.get("text").and_then(Json::as_str).unwrap_or("").trim().to_string();
        let at = match (line.get("marker").and_then(Json::as_str).map(str::trim).filter(|name| !name.is_empty()), number(line, "at")) {
            (Some(marker), _) => SourceTime::Marker { name: marker.trim_start_matches('@').to_string(), offset: number(line, "offset").unwrap_or(0.0) },
            (None, Some(at)) => SourceTime::Seconds(at),
            (None, None) => return Err(ToolError::invalid(format!("`lines[{index}]` needs `at` (seconds) or `marker`"))),
        };
        Ok(VoiceLine { text, at, take: None, key: None })
    }).collect()
}

// ------------------------------------------------------------ registration

/// The tools a Story window implements: the session, library, effect and
/// generation tools every product window has, then Story's own (CUT, DECK,
/// the graphic desk and the storyboard).
fn register(registry: &mut Registry<Handler>) {
    // The status, document tree, library, effect and kernel tools every
    // product window has (makepad-product-tools).
    makepad_product_tools::register(registry);
    let shared: &[(&str, Handler)] = &[
        ("reveal_file", reveal_file),
        ("open_file", open_file),
        ("events", events),
        ("authoring_guide", authoring_guide),
    ];
    for (name, handler) in shared {
        registry.register(name, *handler);
    }
    {
        let story: &[(&str, Handler)] = &[
            ("new_cut", new_cut),
            ("open_cut", open_cut),
            ("save_cut", save_cut),
            ("open_deck", open_deck),
            ("new_deck", new_deck),
            ("add_slide", add_slide),
            ("move_slide", move_slide),
            ("delete_slide", delete_slide),
            ("duplicate_slide", duplicate_slide),
            ("set_slide", set_slide),
            ("set_demo", set_demo),
            ("add_app", add_app),
            ("add_layer", add_layer),
            ("set_deck", set_deck),
            ("reorder_slides", reorder_slides),
            ("set_deck_target", set_deck_target),
            ("set_deck_output_language", set_deck_output_language),
            ("deck_output_language", deck_output_language),
            ("add_app_slide", add_app_slide),
            ("preview", preview_deck),
            ("timeline", timeline),
            ("add_clip", add_clip),
            ("add_title", add_title),
            ("add_voiceover", add_voiceover),
            ("set_voice_lines", set_voice_lines),
            ("add_subtitles", add_subtitles),
            ("add_effect", add_effect),
            ("add_transition", add_transition),
            ("move_block", move_block),
            ("trim_block", trim_block),
            ("blade", blade),
            ("delete_block", delete_block),
            ("set_param", set_param),
            ("set_header", set_header),
            ("set_audio", set_audio),
            ("render_cut", render_cut),
            ("render_status", render_status),
            ("export_web", export_web),
            ("motion_templates", motion_templates),
            ("motion_catalog", motion_catalog),
            ("motion_fields", motion_fields),
            ("list_motions", list_motions),
            ("read_motion", read_motion),
            ("validate_motion", validate_motion),
            ("write_motion", write_motion),
            ("patch_motion", patch_motion),
            ("palette", palette),
            ("set_palette", set_palette),
            ("audio_analyze", audio_analyze),
            ("run_facts", run_facts),
            ("bind_overlay", bind_overlay),
            ("frame", frame),
            ("strip", strip),
            ("onion", onion),
            ("analyze_music", analyze_music),
            ("list_apps", list_apps),
            ("app_dump", app_dump),
            ("validate_app_run", validate_app_run),
            ("run_app_run", run_app_run),
            ("app_run_status", app_run_status),
            ("read_app_run", read_app_run),
            ("beat_grid", beat_grid),
            ("snap_to_beats", snap_to_beats),
            ("fit_to_bars", fit_to_bars),
            ("voice_lines", voice_lines_tool),
            ("speak", speak),
            ("inspect", inspect),
        ];
        for (name, handler) in story {
            registry.register(name, *handler);
        }
    }
    // The generation tools: the video maker's New video and storyboard run
    // presets through them.
    gen::register(registry);
    // SB1's storyboard (the board the cut is made from).
    let board: &[(&str, Handler)] = &[
        ("storyboard_read", |ctx, args| storyboard_tool(ctx, "storyboard_read", args)),
        ("storyboard_new", |ctx, args| storyboard_tool(ctx, "storyboard_new", args)),
        ("storyboard_add", |ctx, args| storyboard_tool(ctx, "storyboard_add", args)),
        ("storyboard_update", |ctx, args| storyboard_tool(ctx, "storyboard_update", args)),
        ("storyboard_move", |ctx, args| storyboard_tool(ctx, "storyboard_move", args)),
        ("storyboard_delete", |ctx, args| storyboard_tool(ctx, "storyboard_delete", args)),
        ("storyboard_row", |ctx, args| storyboard_tool(ctx, "storyboard_row", args)),
        ("storyboard_set", |ctx, args| storyboard_tool(ctx, "storyboard_set", args)),
        ("storyboard_link", |ctx, args| storyboard_tool(ctx, "storyboard_link", args)),
        ("storyboard_write", |ctx, args| storyboard_tool(ctx, "storyboard_write", args)),
        ("storyboard_changes", |ctx, args| storyboard_tool(ctx, "storyboard_changes", args)),
        ("storyboard_plan_update", |ctx, args| storyboard_tool(ctx, "storyboard_plan_update", args)),
        ("storyboard_apply_changes", |ctx, args| storyboard_tool(ctx, "storyboard_apply_changes", args)),
        ("storyboard_mark_built", |ctx, args| storyboard_tool(ctx, "storyboard_mark_built", args)),
        ("storyboard_look", |ctx, args| storyboard_tool(ctx, "storyboard_look", args)),
    ];
    debug_assert_eq!(board.len(), super::storyboard::TOOL_NAMES.len());
    // VR1's variants of the open graphic.
    let variants: &[(&str, Handler)] = &[
        ("graphic_open", |ctx, args| variants_tool(ctx, "graphic_open", args)),
        ("variants", |ctx, args| variants_tool(ctx, "variants", args)),
        ("apply_variant", |ctx, args| variants_tool(ctx, "apply_variant", args)),
        ("hover_variant", |ctx, args| variants_tool(ctx, "hover_variant", args)),
        ("variants_status", |ctx, args| variants_tool(ctx, "variants_status", args)),
        ("scene_ask", |ctx, args| variants_tool(ctx, "scene_ask", args)),
        ("history_list", |ctx, args| variants_tool(ctx, "history_list", args)),
        ("history_read", |ctx, args| variants_tool(ctx, "history_read", args)),
        ("history_diff", |ctx, args| variants_tool(ctx, "history_diff", args)),
        ("history_pick", |ctx, args| variants_tool(ctx, "history_pick", args)),
        ("merge_from_history", |ctx, args| variants_tool(ctx, "merge_from_history", args)),
        ("graphic_palette", |ctx, args| variants_tool(ctx, "palette", args)),
        ("palette_moves", |ctx, args| variants_tool(ctx, "palette_moves", args)),
        ("palette_apply", |ctx, args| variants_tool(ctx, "palette_apply", args)),
        ("swaps", |ctx, args| variants_tool(ctx, "swaps", args)),
        ("editor_make", |ctx, args| variants_tool(ctx, "editor_make", args)),
        ("editor_write", |ctx, args| variants_tool(ctx, "editor_write", args)),
        ("editor_paths", |ctx, args| variants_tool(ctx, "editor_paths", args)),
        ("editor_drive", |ctx, args| variants_tool(ctx, "editor_drive", args)),
        ("editor_when", |ctx, args| variants_tool(ctx, "editor_when", args)),
        ("editor_sweep", |ctx, args| variants_tool(ctx, "editor_sweep", args)),
        ("editor_deep", |ctx, args| variants_tool(ctx, "editor_deep", args)),
        ("graphic_source", |ctx, args| variants_tool(ctx, "graphic_source", args)),
        ("editor_animate", |ctx, args| variants_tool(ctx, "editor_animate", args)),
        ("editor_fit", |ctx, args| variants_tool(ctx, "editor_fit", args)),
        ("editor_compare", |ctx, args| variants_tool(ctx, "editor_compare", args)),
        ("swap_variants", |ctx, args| variants_tool(ctx, "swap_variants", args)),
        ("palette_save", palette_save),
        // TL1's edit panel (the whole film's controls).
        ("cut_panel_status", |ctx, args| cut_panel_tool(ctx, "cut_panel_status", args)),
        ("cut_panel_paths", |ctx, args| cut_panel_tool(ctx, "cut_panel_paths", args)),
        ("cut_panel_read", |ctx, args| cut_panel_tool(ctx, "cut_panel_read", args)),
        ("cut_panel_make", |ctx, args| cut_panel_tool(ctx, "cut_panel_make", args)),
        ("cut_panel_write", |ctx, args| cut_panel_tool(ctx, "cut_panel_write", args)),
        ("cut_panel_gesture", |ctx, args| cut_panel_tool(ctx, "cut_panel_gesture", args)),
        ("cut_panel_drive", |ctx, args| cut_panel_tool(ctx, "cut_panel_drive", args)),
        ("cut_panel_recolour", |ctx, args| cut_panel_tool(ctx, "cut_panel_recolour", args)),
        ("cut_panel_palette", |ctx, args| cut_panel_tool(ctx, "cut_panel_palette", args)),
    ];
    {
        for (name, handler) in variants {
            registry.register(name, *handler);
        }
        for (name, handler) in board {
            registry.register(name, *handler);
        }
    }
    // G1's generative segments: this Stage's own tools.
    let generate: &[(&str, Handler)] = &[
        ("generate_add", |ctx, args| generate_tool(ctx, "generate_add", args)),
        ("generate_run", |ctx, args| generate_tool(ctx, "generate_run", args)),
        ("generate_pick", |ctx, args| generate_tool(ctx, "generate_pick", args)),
        ("generate_cancel", |ctx, args| generate_tool(ctx, "generate_cancel", args)),
        ("generate_status", |ctx, args| generate_tool(ctx, "generate_status", args)),
    ];
    {
        for (name, handler) in generate {
            registry.register(name, *handler);
        }
    }
    // GN1's New video: one ask to a whole new cut.
    let new_video: &[(&str, Handler)] = &[
        ("new_video", |ctx, args| new_video_tool(ctx, "new_video", args)),
        ("new_video_status", |ctx, args| new_video_tool(ctx, "new_video_status", args)),
        ("new_video_redo", |ctx, args| new_video_tool(ctx, "new_video_redo", args)),
        ("new_video_cancel", |ctx, args| new_video_tool(ctx, "new_video_cancel", args)),
    ];
    {
        for (name, handler) in new_video {
            registry.register(name, *handler);
        }
        // The chat card's video maker (`cut/generate.rs`).
        registry.register("make_video", |ctx, args| match ctx.app.make_video_call(args) {
            Ok(()) => done(Json::Obj(vec![("started".into(), Json::Bool(true))])),
            Err(error) => fail(ToolError::refused(error)),
        });
        registry.register("make_video_note", |ctx, args| match ctx.app.make_video_note_call(args) {
            Ok(()) => done(Json::Obj(vec![("sent".into(), Json::Bool(true))])),
            Err(error) => fail(ToolError::refused(error)),
        });
    }
}

// ------------------------------------------------------------ session

fn authoring_guide(_: &mut Ctx, args: &Json) -> Outcome {
    use makepad_story_video_agent::guide;
    let topic = args.get("topic").and_then(Json::as_str).unwrap_or("splash");
    let text = match topic {
        "splash" => guide::authoring_guide().to_string(),
        "deck_edit" => guide::compact(guide::DECK_EDIT).to_string(),
        "explainer" => guide::compact_guide().to_string(),
        "deck" => format!("{}\n\n## Worked example: the Motion deck\n\n```splash\n{}\n```", guide::DECK.trim(), guide::DECK_EXAMPLE.trim()),
        "canvas" => guide::CANVAS.to_string(),
        "performance" => guide::PERFORMANCE.to_string(),
        "plot" => guide::PLOT.to_string(),
        "motion3d" => guide::MOTION3D.to_string(),
        other => return fail(ToolError::invalid(format!("`{other}` is not a part of the guide: splash, deck_edit, deck, explainer, canvas, performance, plot or motion3d (score_panel is Stage's)"))),
    };
    let from = args.get("from").and_then(Json::as_u64).unwrap_or(0) as usize;
    let (page, next) = makepad_product_tools::guide_page(&text, from);
    let mut fields = vec![("topic", json::s(topic)), ("text", json::s(page)), ("from", Json::Int(from as i64)), ("length", Json::Int(text.len() as i64))];
    if let Some(next) = next {
        fields.push(("next", Json::Int(next as i64)));
    }
    done(json::obj(fields))
}

/// Check a `path` argument against what Stage wrote.
fn written_place(ctx: &Ctx, args: &Json) -> Result<super::Place, ToolError> {
    let path = args.get("path").and_then(Json::as_str).unwrap_or("");
    ctx.app.video_cut.written().check(path).map_err(ToolError::refused)
}

fn reveal_file(ctx: &mut Ctx, args: &Json) -> Outcome {
    let folder = match written_place(ctx, args) {
        Ok(super::Place::Path(path)) if path.is_dir() => path,
        Ok(super::Place::Path(path)) => match path.parent() {
            Some(parent) => parent.to_path_buf(),
            None => return fail(ToolError::refused("the file has no folder")),
        },
        Ok(super::Place::Url(_)) => return fail(ToolError::refused("an address is opened (open_file), not shown in the Finder")),
        Err(error) => return fail(error),
    };
    ctx.cx.open_url(&folder.to_string_lossy(), OpenUrlInPlace::No);
    done(json::obj(vec![("shown", json::s(folder.display().to_string()))]))
}

fn open_file(ctx: &mut Ctx, args: &Json) -> Outcome {
    let target = match written_place(ctx, args) {
        Ok(super::Place::Path(path)) if path.exists() => path.to_string_lossy().to_string(),
        Ok(super::Place::Path(path)) => return fail(ToolError::not_found(format!("`{}` is not there any more", path.display()))),
        Ok(super::Place::Url(url)) => url,
        Err(error) => return fail(error),
    };
    ctx.cx.open_url(&target, OpenUrlInPlace::No);
    done(json::obj(vec![("opened", json::s(target))]))
}

fn events(ctx: &mut Ctx, args: &Json) -> Outcome {
    observe_page(ctx.app, ctx.host);
    let since = args.get("since").and_then(Json::as_u64).unwrap_or(0);
    let events: Vec<Json> = ctx.host.log.since(since).iter().map(ChangeEvent::to_json).collect();
    done(json::obj(vec![("events", Json::Arr(events)), ("last", Json::Int(ctx.host.log.last() as i64))]))
}

fn undo(ctx: &mut Ctx, redo: bool) -> Outcome {
    let kind = target(ctx.app);
    undo_document(ctx, redo, kind)
}

fn undo_document(ctx: &mut Ctx, redo: bool, kind: DocKind) -> Outcome {
    let tool = if redo { "redo" } else { "undo" };
    if let Some((tree, path, revision, _)) = open_tree_state(ctx.app, kind) {
        let (before_tree, path) = (tree.clone(), path.to_string());
        let (busy, depth, previous) = match kind {
            DocKind::Deck => { let session = ctx.app.deck.session().unwrap(); (ctx.app.deck.history_busy(), if redo { session.redo.len() } else { session.undo.len() }, ctx.app.deck.history_head()) }
            DocKind::Cut => { let session = ctx.app.video_cut.session.as_ref().unwrap(); (ctx.app.video_cut.saving || !page_idle(&ctx.app.video_cut), if redo { session.redo.len() } else { session.undo.len() }, session.history_head.zip(session.alias.clone())) }
        };
        if busy { return fail(ToolError::refused("the tree is being saved or edited; undo when it finishes")); }
        if !redo && depth == 0 {
            if let Some((head, alias)) = previous {
                let file = tree.files.get(&path).and_then(|file| file.reference.or(file.base)).map(|reference| reference.asset_id);
                let root = path == tree.root.1;
                return slow(ctx, SlowJob::PreviousTree { head, alias, path, file, root }, Then::UndoTree { revision, kind }, false);
            }
        }
        if depth == 0 {
            return fail(ToolError::refused(if redo { "nothing to redo" } else { "nothing to undo" }));
        }
        match kind {
            DocKind::Deck => { ctx.app.deck.session_mut().unwrap().step(redo); ctx.app.deck.document_changed(format!("{} · {tool}", origin_label(ctx.origin))); }
            DocKind::Cut => { ctx.app.video_cut.session.as_mut().unwrap().step(redo); ctx.app.video_cut.document_changed(); }
        }
        let (tree, _, revision, _) = open_tree_state(ctx.app, kind).unwrap();
        return Outcome::Done(Ok(tree_output(ctx.host, "doc_undo", ctx.origin, revision, Some(&before_tree), tree)));
    }
    let edit = &mut ctx.app.video_cut;
    let Some(session) = edit.session.as_ref() else { return fail(no_edit()) };
    if !page_idle(edit) {
        return fail(ToolError::refused("the page is applying a change: try again"));
    }
    let (before, depth) = (session.source.clone(), if redo { session.redo.len() } else { session.undo.len() });
    if depth == 0 {
        return fail(ToolError::refused(if redo { "nothing to redo" } else { "nothing to undo" }));
    }
    note_author(edit, ctx.origin, tool);
    edit.undo(redo);
    let session = edit.session.as_ref().unwrap();
    let patch = diff::unified(&before, &session.source, 2);
    ctx.host.log.push(session.revision, tool, ctx.origin, tool.into(), patch.clone());
    ctx.host.state.seen = Some((session.revision, session.source.clone()));
    let mut output = ToolOutput::json(json::obj(vec![("path", json::s("cut.splash")), ("revision", Json::Int(session.revision as i64)), ("diff", json::s(patch))]));
    output.changes.push(DocChange { key: "cut.splash".into(), label: tool.into(), before, after: session.source.clone() });
    Outcome::Done(Ok(output))
}

// ------------------------------------------------------------ project / edit

fn new_cut(ctx: &mut Ctx, args: &Json) -> Outcome {
    // The size: `width` and `height`, else `format`, else the format last
    // used (the New button's).
    let edit = &mut ctx.app.video_cut;
    let (width, height) = match (args.get("width").and_then(Json::as_u64), args.get("height").and_then(Json::as_u64)) {
        (Some(width), Some(height)) => (width as u32, height as u32),
        (None, None) => match text(args, "format") {
            Some(format) => match super::format_size(format) {
                Some(size) => size,
                None => return fail(ToolError::invalid(format!("`format`: `{format}` (16:9, 9:16, 1:1 or 4:5)"))),
            },
            None => edit.new_format.unwrap_or((1920, 1080)),
        },
        _ => return fail(ToolError::invalid("`width` and `height` go together")),
    };
    edit.new_format = Some((width, height));
    if flag(args, "reframe") == Some(true) {
        // The open cut's saved revision, nested in a cut of this format.
        let aspect = super::aspect_name(width, height);
        return match edit.reframe_to(width, height, &aspect) {
            Ok(()) => done(json::obj(vec![("note", json::s(format!("a {aspect} cut nesting the saved revision; check the framing")))])),
            Err(error) => fail(ToolError::refused(error)),
        };
    }
    let label = text(args, "label").map(str::to_string).unwrap_or_else(|| format!("Untitled {}", super::aspect_name(width, height)));
    let header = Header { label: None, duration: number(args, "duration"), bpm: number(args, "bpm"), output: Some((width, height, number(args, "fps").unwrap_or(60.0))) };
    let base = match header.write(&super::template(width, height, &label)) {
        Ok(base) => base,
        Err(error) => return fail(error),
    };
    let ticket = ctx.host.next();
    let Some(worker) = &mut ctx.host.state.edit else { return fail(ToolError::failed("the cut worker is not running")) };
    worker.send(CutJob { ticket, kind: DocKind::Cut, base, steps: Vec::new() });
    Outcome::Wait(Wait::Open { ticket, head: None, saved: false, done: None })
}

fn open_cut(ctx: &mut Ctx, args: &Json) -> Outcome {
    // A folder cut (project: its folder's name, its cut file as path or none) opens in place.
    if let Some(card) = text(args, "project").and_then(|project| super::folder_docs::cut_cards().iter().find(|card| card.doc.name == project && text(args, "path").is_none_or(|path| path == card.path))) {
        let opened = card.doc.read().and_then(|draft| {
            let source = draft.text(&card.path)?.to_string();
            let doc = super::super::document_tree::load(&draft, &card.path)?.into_cut()?;
            Ok((draft, source, doc))
        });
        let (draft, source, doc) = match opened {
            Ok(opened) => opened,
            Err(error) => return fail(ToolError::refused(error)),
        };
        if ctx.app.page != StoryPage::Cut { ctx.app.set_page(ctx.cx, StoryPage::Cut); }
        let ui = ctx.app.ui.clone();
        let label = doc.label.clone();
        let folder = super::FolderOpen { doc: card.doc, draft, path: card.path.clone(), reload: false };
        ctx.app.video_cut.finish(ctx.cx, &ui, PagePending::Open { source: source.clone(), head: None, saved: true, tree: None, folder: Some(folder) }, Ok((source, doc, None)));
        return done(json::obj(vec![("label", json::s(label)), ("path", json::s(card.path.clone())), ("folder", json::s(card.doc.dir.display().to_string()))]));
    }
    if let Some(project) = text(args, "project") {
        if args.get("cut").is_some() { return fail(ToolError::invalid("give a project and path, or a standalone cut")); }
        let Some(path) = text(args, "path") else { return fail(ToolError::invalid("give the cut's file path with its project")); };
        if project_name(ctx.app, DocKind::Cut).is_some_and(|alias| alias.to_string() == project)
            && open_tree_state(ctx.app, DocKind::Cut).is_some_and(|(tree, current, ..)| tree.context(current).resolve(path).is_ok_and(|file| file.path == current)) {
            if ctx.app.page != StoryPage::Cut { ctx.app.set_page(ctx.cx, StoryPage::Cut); }
            return done(document_identity(ctx.app, DocKind::Cut));
        }
        return slow(ctx, SlowJob::OpenProject { project: project.into(), path: Some(path.into()) }, Then::OpenProject { then: None }, false);
    }
    match reference(args.get("cut"), "cut") {
        Ok(asset) => slow(ctx, SlowJob::OpenEdit { asset }, Then::Open { asset }, false),
        Err(error) => fail(error),
    }
}

fn save_cut(ctx: &mut Ctx, _: &Json) -> Outcome {
    let edit = &mut ctx.app.video_cut;
    let Some(session) = edit.session.as_ref() else { return fail(no_edit()) };
    if !session.dirty() && session.head.is_some() && !super::motion_edit::has_drafts(edit) && page_idle(edit) && !edit.saving && !edit.save_requested {
        return done(document_identity(ctx.app, DocKind::Cut));
    }
    if session.tree.is_none() && session.doc.nodes.iter().all(|node| node.span.is_none() && !matches!(node.kind, CutNodeKind::VoiceOver { .. })) {
        return fail(ToolError::refused("nothing to save yet: add a clip first"));
    }
    let revision = session.revision;
    if ctx.app.page != StoryPage::Cut {
        // The save's thumbnail is drawn by the page.
        ctx.app.set_page(ctx.cx, StoryPage::Cut);
    }
    ctx.app.video_cut.save_error = None;
    ctx.app.video_cut.save(ctx.cx);
    Outcome::Wait(Wait::Save { revision })
}

fn open_deck(ctx: &mut Ctx, args: &Json) -> Outcome {
    if ctx.app.page != StoryPage::Deck {
        ctx.app.set_page(ctx.cx, StoryPage::Deck);
    }
    let alias = text(args, "path").map(str::to_string);
    // A folder deck (`<folder>/<deck file>`, or a folder's root deck) opens in place.
    if let Some(path) = &alias {
        let ui = ctx.app.ui.clone();
        if let Some(opened) = ctx.app.deck.open_folder_path(ctx.cx, &ui, path) {
            return match opened {
                Ok(()) => done(deck_json(ctx.app)),
                Err(error) => fail(ToolError::refused(error)),
            };
        }
    }
    if alias.is_some() {
        return slow(ctx, SlowJob::OpenDeck { asset: None, alias }, Then::OpenDeck, false);
    }
    let ui = ctx.app.ui.clone();
    ctx.app.deck.ensure_open();
    if ctx.app.deck.session().is_none() {
        if let Err(error) = ctx.app.deck.open_bundled(ctx.cx, &ui) {
            return fail(ToolError::refused(error));
        }
    }
    let mut json = deck_json(ctx.app);
    // The slide the picture shows now.
    if let (Json::Obj(fields), Some(shown)) = (&mut json, crate::deck::shown_slide(ctx.cx, &ui)) { fields.push(("shown".into(), Json::Int(shown as i64))); }
    done(json)
}

/// The open deck, as `open_deck` answers.
fn deck_json(app: &StoryApp) -> Json {
    match app.deck.session() {
        Some(session) => {
            let mut fields = vec![
                ("title", json::s(session.doc.title.clone())),
                ("slides", Json::Arr(session.doc.slides.iter().map(|slide| json::s(slide.name.clone())).collect())),
                ("revision", Json::Int(session.revision as i64)),
            ];
            if session.tree.is_some() { fields.push(("path", json::s(session.path.clone()))); }
            fields.push(("unsaved", Json::Bool(session.dirty())));
            // A folder deck is open in place: its files are in this folder.
            if let Some(dir) = app.deck.folder_dir() { fields.push(("folder", json::s(dir))); }
            if let Some(element) = app.deck.selected_element() {
                fields.push(("selection", json::obj(vec![
                    ("node", json::s(element.key.clone())), ("source_node", json::s(element.path.clone())), ("file", json::s(element.file.clone())),
                    ("line", Json::Int(element.line as i64)), ("end_line", Json::Int(element.end_line as i64)),
                    ("label", json::s(element.label.clone())), ("context", json::s(element.context())),
                ])));
            }
            json::obj(fields)
        }
        None => Json::Null,
    }
}

fn root_path(app: &StoryApp) -> String {
    makepad_product_tools::documents::root_path::<StoryTools>(app)
}

fn reorder_slides(ctx: &mut Ctx, args: &Json) -> Outcome {
    let Some(order) = args.get("slides").and_then(Json::as_arr) else { return fail(ToolError::invalid("give all slide names in `slides`, in the wanted order")) };
    let Some(slides) = order.iter().map(|name| name.as_str().map(str::to_string)).collect::<Option<Vec<_>>>() else {
        return fail(ToolError::invalid("slides are names"));
    };
    deck_op(ctx, args, makepad_story_deck::ops::DeckOp::ReorderSlides { slides })
}

fn set_deck_target(ctx: &mut Ctx, args: &Json) -> Outcome {
    let Some(seconds) = number(args, "seconds") else {
        return fail(ToolError::invalid("give the deck target in whole seconds"));
    };
    deck_op(ctx, args, makepad_story_deck::ops::DeckOp::SetDeckTarget { seconds })
}

/// The output window in another language: the slides' own texts in it.
fn set_deck_output_language(ctx: &mut Ctx, args: &Json) -> Outcome {
    if ctx.app.page != StoryPage::Deck { return fail(ToolError::refused("open the DECK page first")); }
    let Some(language) = text(args, "language") else { return fail(ToolError::invalid("give `language`: zh, or en for the deck as written")) };
    let ui = ctx.app.ui.clone();
    match ctx.app.deck.set_output_language(ctx.cx, &ui, language) {
        Ok(state) => done(state),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn deck_output_language(ctx: &mut Ctx, _: &Json) -> Outcome {
    if ctx.app.deck.session().is_none() { return fail(no_doc(DocKind::Deck)); }
    done(ctx.app.deck.output_language())
}

fn add_app_slide(ctx: &mut Ctx, args: &Json) -> Outcome {
    let Json::Obj(mut fields) = args.clone() else { return fail(ToolError::invalid("give the app slide's fields")) };
    fields.retain(|(key, _)| key != "kind"); fields.push(("kind".into(), json::s("app")));
    add_slide(ctx, &Json::Obj(fields))
}

fn preview_deck(ctx: &mut Ctx, _: &Json) -> Outcome {
    if ctx.app.page != StoryPage::Deck { return fail(ToolError::refused("open a deck first")); }
    if ctx.host.waiting.iter().any(|waiting| matches!(waiting.wait, Wait::Preview { .. })) { return fail(ToolError::refused("a preview is already rendering")); }
    let Some(session) = ctx.app.deck.session() else { return fail(no_doc(DocKind::Deck)) };
    let Some(tree) = session.tree.clone() else { return fail(ToolError::refused("no deck tree open")) };
    let (base, revision) = (session.source.clone(), session.revision);
    let ticket = ctx.host.next();
    let Some(worker) = &mut ctx.host.state.edit else { return fail(ToolError::failed("the document worker is unavailable")) };
    let path = session.path.clone();
    worker.send(CutJob { ticket, kind: DocKind::Deck, base, steps: vec![Step::Tree { tree, path, deck: None }] });
    Outcome::Wait(Wait::Preview { ticket, revision, done: None, rendering: false })
}

fn source_json(src: &MediaRef, nested: bool, tree: Option<&makepad_asset_data::tree::TreeDraft>) -> Json {
    if let Some(tree) = tree {
        let reference = match src {
            MediaRef::Asset(asset) => Some((&asset.id, &asset.rev)),
            MediaRef::Run(run) => Some((&run.render_id, &run.render)),
            MediaRef::MissingOptional(_) => None,
        };
        if let Some((id, revision)) = reference {
            let path = tree.files.values().find(|file| file.reference.is_some_and(|reference| reference.asset_id.to_string() == *id && reference.revision.to_string() == *revision)).map(|file| file.path.as_str());
            return json::obj(vec![("path", path.map(json::s).unwrap_or(Json::Null))]);
        }
    }
    match src {
        MediaRef::MissingOptional(path) => json::obj(vec![("kind", json::s("missing_optional")), ("path", json::s(path.clone()))]),
        MediaRef::Asset(asset) => json::obj(vec![("kind", json::s(if nested { "cut" } else { "asset" })), ("asset_id", json::s(asset.id.clone())), ("revision", json::s(asset.rev.clone()))]),
        MediaRef::Run(run) => json::obj(vec![("kind", json::s("app_run")), ("asset_id", json::s(run.id.clone())), ("revision", json::s(run.rev.clone())), ("render", json::obj(vec![("asset_id", json::s(run.render_id.clone())), ("revision", json::s(run.render.clone()))]))]),
    }
}

fn source_time_json(time: &SourceTime) -> Json {
    match time {
        SourceTime::Seconds(seconds) => num(*seconds),
        SourceTime::Marker { name, offset } => json::obj(vec![("marker", json::s(name.clone())), ("offset", num(*offset))]),
    }
}

fn timeline(ctx: &mut Ctx, args: &Json) -> Outcome {
    let doc = match work_doc(ctx, args) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let tree = open_tree_state(ctx.app, target(ctx.app)).map(|(tree, ..)| tree.as_ref());
    let mut blocks: Vec<(f64, Json)> = Vec::new();
    for node in &doc.nodes {
        let mut fields: Vec<(&str, Json)> = vec![("node", json::s(node.id.clone())), ("type", json::s(node.kind.type_name()))];
        if let Some(lane) = ops::lane_of(node) {
            fields.push(("lane", json::s(match lane {
                ops::Lane::Video => "video",
                ops::Lane::Audio => "audio",
                ops::Lane::Captions => "captions",
                ops::Lane::Voice => "voice",
            })));
        }
        fields.push(("track", Json::Int(node.track as i64)));
        if let Some(span) = node.span {
            fields.push(("span", Json::Arr(vec![num(span.start), num(span.length)])));
        }
        match &node.kind {
            CutNodeKind::Clip { src, source_in, speed, nested, .. } => {
                fields.push(("src", source_json(src, *nested, tree)));
                fields.push(("in", source_time_json(source_in)));
                if *speed != 1.0 {
                    fields.push(("speed", num(*speed)));
                }
            }
            CutNodeKind::Still { src, .. } => fields.push(("src", source_json(src, false, tree))),
            CutNodeKind::AudioClip { src, source_in, .. } => {
                fields.push(("src", source_json(src, false, tree)));
                fields.push(("in", source_time_json(source_in)));
            }
            CutNodeKind::Title { text, .. } => fields.push(("text", json::s(text.clone()))),
            CutNodeKind::Fx { input, effect, .. } => {
                fields.push(("effect", match effect {
                    EffectRef::Preset(name) => json::s(name),
                    EffectRef::Asset(asset) => source_json(&MediaRef::Asset(asset.clone()), false, tree),
                }));
                if let Some(input) = input {
                    fields.push(("on", json::s(input.node.clone())));
                }
            }
            CutNodeKind::Transition { a, b, effect, .. } => {
                fields.push(("effect", json::s(effect.clone())));
                fields.push(("a", json::s(a.as_ref().map(|wire| wire.node.clone()).unwrap_or_default())));
                fields.push(("b", json::s(b.as_ref().map(|wire| wire.node.clone()).unwrap_or_default())));
            }
            CutNodeKind::VoiceOver { voice, lines, .. } => {
                fields.push(("voice", json::s(voice.id.clone())));
                fields.push(("lines", Json::Arr(lines.iter().map(|line| json::obj(vec![("text", json::s(line.text.clone())), ("at", source_time_json(&line.at)), ("spoken", Json::Bool(line.take.is_some() && !line.stale(voice)))])).collect())));
            }
            CutNodeKind::Subtitles { source, style, .. } => {
                fields.push(("style", json::s(style.clone())));
                if let Some(source) = source {
                    fields.push(("voice", json::s(source.node.clone())));
                }
            }
            _ => {
                if node.span.is_none() {
                    continue;
                }
            }
        }
        let start = node.span.map_or(f64::MAX, |span| span.start);
        blocks.push((start, json::obj(fields)));
    }
    // Motion graphics (and the overlays bound to app-run clips).
    for graphic in &doc.graphics {
        let mut fields: Vec<(&str, Json)> = vec![("node", json::s(graphic.id.clone())), ("type", json::s("Graphic")), ("lane", json::s("video")), ("track", Json::Int(graphic.track as i64))];
        if let Some(span) = graphic.span {
            fields.push(("span", Json::Arr(vec![num(span.start), num(span.length)])));
        }
        fields.push(("src", match &graphic.src {
            makepad_story_cut::graphic::GraphicSrc::Asset(asset) => json::obj(vec![("kind", json::s("motion")), ("asset_id", json::s(asset.id.clone())), ("revision", json::s(asset.rev.clone()))]),
            makepad_story_cut::graphic::GraphicSrc::Builtin(name) => json::obj(vec![("kind", json::s("builtin")), ("name", json::s(name.clone()))]),
            makepad_story_cut::graphic::GraphicSrc::Tree(tree) => json::obj(vec![("kind", json::s("motion")), ("path", json::s(tree.document.clone()))]),
        }));
        if let Some(bind) = &graphic.bind {
            fields.push(("bind", json::s(format!("{bind:?}"))));
        }
        blocks.push((graphic.span.map_or(f64::MAX, |span| span.start), json::obj(fields)));
    }
    // Generative segments (G1): a band over their input.
    for gen in &doc.generates {
        blocks.push((gen.span.start, json::obj(vec![
            ("node", json::s(gen.id.clone())),
            ("type", json::s("Generate")),
            ("span", Json::Arr(vec![num(gen.span.start), num(gen.span.length)])),
            ("preset", json::s(gen.preset.clone())),
            ("prompt", json::s(gen.prompt.clone())),
            ("takes", Json::Int(gen.takes.len() as i64)),
            ("note", json::s("generate_status has its state and takes")),
        ])));
    }
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = json::obj(vec![
        ("label", json::s(doc.label.clone())),
        ("output", json::obj(vec![("width", Json::Int(doc.output.width as i64)), ("height", Json::Int(doc.output.height as i64)), ("fps", num(doc.output.fps))])),
        ("duration", num(doc.duration)),
        ("bpm", num(doc.bpm)),
        ("blocks", Json::Arr(blocks.into_iter().map(|(_, block)| block).collect())),
    ]);
    // On the CUT page: where the playhead is, and the folder a cut open in place lives in.
    if let (Json::Obj(fields), true) = (&mut out, ctx.app.page == StoryPage::Cut) {
        fields.push(("playhead".into(), num(ctx.app.video_cut.playhead)));
        if let Some(folder) = &ctx.app.video_cut.folder { fields.push(("folder".into(), json::s(folder.doc.dir.display().to_string()))); }
    }
    done(out)
}

fn add_clip(ctx: &mut Ctx, args: &Json) -> Outcome {
    let asset = match text(args, "path") {
        Some(path) => {
            if args.get("media").is_some() { return fail(ToolError::invalid("give a project path or library media, not both")); }
            let Some((tree, from, ..)) = open_tree_state(ctx.app, target(ctx.app)) else { return fail(ToolError::refused("open the project before adding a path")); };
            match tree.context(from).resolve(path).and_then(|file| file.reference.ok_or_else(|| format!("{}: save the file before adding it", file.path))) {
                Ok(reference) => reference,
                Err(error) => return fail(ToolError::refused(error)),
            }
        }
        None => match reference(args.get("media"), "media") { Ok(asset) => asset, Err(error) => return fail(error) },
    };
    let (kind, src) = match text(args, "kind").unwrap_or("video") {
        "video" => (MediaKind::Video, MediaRef::Asset(ids(&asset))),
        "image" => (MediaKind::Image, MediaRef::Asset(ids(&asset))),
        "audio" => (MediaKind::Audio, MediaRef::Asset(ids(&asset))),
        "cut" => (MediaKind::Cut, MediaRef::Asset(ids(&asset))),
        _ => {
            let render = match reference(args.get("render"), "render") {
                Ok(render) => render,
                Err(_) => return fail(ToolError::invalid("an app_run clip needs `render`: the render (Video) revision it plays, from search_library kind `app_run` or run_app_run")),
            };
            (MediaKind::AppRun, MediaRef::Run(RunRef { id: asset.asset_id.to_string(), rev: asset.revision.to_string(), render_id: render.asset_id.to_string(), render: render.revision.to_string() }))
        }
    };
    let start = number(args, "start").unwrap_or(0.0);
    let length = number(args, "length");
    let track = args.get("track").and_then(Json::as_u64).map(|track| track as u32);
    let mut steps = vec![Step::Op(CutOp::Add { media: Media { kind, src, seconds: length }, start, track })];
    if length.is_some() || number(args, "in").is_some() {
        steps.push(Step::Added { start: None, length, source_in: number(args, "in"), lines: None });
    }
    ops_recipe(ctx, args, steps)
}

fn add_title(ctx: &mut Ctx, args: &Json) -> Outcome {
    let start = number(args, "start").unwrap_or(0.0);
    let mut steps = vec![Step::Op(CutOp::AddTitle { start, text: text(args, "text").unwrap_or("").to_string() })];
    if let Some(length) = number(args, "length") {
        steps.push(Step::Added { start: None, length: Some(length), source_in: None, lines: None });
    }
    ops_recipe(ctx, args, steps)
}

fn add_voiceover(ctx: &mut Ctx, args: &Json) -> Outcome {
    let lines = match voice_lines(args) {
        Ok(lines) => lines,
        Err(error) => return fail(error),
    };
    let start = match lines.first().map(|line| &line.at) {
        Some(SourceTime::Seconds(start)) => *start,
        _ => 0.0,
    };
    let voice = text(args, "voice").map(str::to_string).unwrap_or_else(|| {
        let default = VoiceSpec::default().id;
        let voices = &ctx.app.video_cut.voices;
        if voices.is_empty() || voices.contains(&default) { default } else { voices[0].clone() }
    });
    ops_recipe(ctx, args, vec![Step::Op(CutOp::AddVoiceOver { start, voice }), Step::Added { start: None, length: None, source_in: None, lines: Some(lines) }])
}

fn set_voice_lines(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    let doc = match work_doc(ctx, args) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let Some(CutNodeKind::VoiceOver { lines: current, .. }) = doc.node(&node).map(|node| &node.kind) else {
        return fail(ToolError::not_found(format!("`{node}` is not a VoiceOver (call timeline)")));
    };
    let mut lines = match voice_lines(args) {
        Ok(lines) => lines,
        Err(error) => return fail(error),
    };
    // A line whose words did not change keeps its take.
    for line in &mut lines {
        if let Some(old) = current.iter().find(|old| old.text == line.text) {
            line.take = old.take.clone();
            line.key = old.key.clone();
        }
    }
    ops_recipe(ctx, args, vec![Step::Op(CutOp::SetLines { node, lines })])
}

fn add_subtitles(ctx: &mut Ctx, args: &Json) -> Outcome {
    let voice = text(args, "voice").unwrap_or("").to_string();
    ops_recipe(ctx, args, vec![Step::Op(CutOp::AddSubtitles { voice })])
}

/// `preset` or `asset:<id>:<rev>`, checked against the bundled list.
fn effect_arg(effect: &str, transition: bool) -> Result<Result<String, AssetRef>, ToolError> {
    if let Some(rest) = effect.strip_prefix("asset:") {
        let (id, rev) = rest.split_once(':').ok_or_else(|| ToolError::invalid("a library effect is `asset:<asset_id>:<revision>`"))?;
        return Ok(Err(AssetRef { id: id.to_string(), rev: rev.to_string() }));
    }
    // A transition without a preset is the dissolve.
    if transition && effect.is_empty() {
        return Ok(Ok(String::new()));
    }
    let Some((pack, name, source)) = makepad_content_packs::effects::resolve(effect) else {
        if makepad_content_packs::effect_packs().is_empty() {
            return Err(ToolError::new(ErrorKind::NotAvailable, makepad_content_packs::effects::missing(&format!("`{effect}`"))));
        }
        return Err(ToolError::not_found(format!("there is no effect preset `{effect}`: list_effects kind `{}` has the ids", if transition { "transition" } else { "effect" })));
    };
    if pack.is_transition(name) != transition {
        return Err(ToolError::invalid(format!("`{name}` is {}, not {}", if transition { "an effect" } else { "a transition" }, if transition { "a transition" } else { "an effect" })));
    }
    if let Some(why) = pack.seek_refusal(source, false) {
        return Err(ToolError::refused(format!("`{name}` can't be used in a cut: {why}")));
    }
    Ok(Ok(name.to_string()))
}

fn add_effect(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    if let Err(error) = work_doc(ctx, args).and_then(|doc| node_exists(&doc, &node)) {
        return fail(error);
    }
    match effect_arg(text(args, "effect").unwrap_or(""), false) {
        Ok(Ok(preset)) => ops_recipe(ctx, args, vec![Step::Op(CutOp::Effect { node, effect: preset })]),
        // A library effect: wrap with a placeholder preset, then point the
        // new Fx at the revision (one step).
        Ok(Err(asset)) => ops_recipe(ctx, args, vec![Step::Op(CutOp::Effect { node, effect: placeholder_effect() }), Step::AddedFx(asset)]),
        Err(error) => fail(error),
    }
}

/// A bundled effect a cut can seek, to wrap a block before the Fx is
/// pointed at a library effect.
fn placeholder_effect() -> String {
    makepad_content_packs::effect_packs().iter().find_map(|pack| {
        pack.presets().iter().find(|(name, source)| !pack.is_transition(name) && pack.seek_refusal(source, false).is_none()).map(|(name, _)| name.to_string())
    }).unwrap_or_default()
}

fn add_transition(ctx: &mut Ctx, args: &Json) -> Outcome {
    if args.get("motion").is_some_and(|motion| !motion.is_null()) {
        return fail(ToolError::new(ErrorKind::NotAvailable, "a Motion document as a transition is not available yet: M1 (Graphic transition node) has not landed; use a transition `effect`"));
    }
    let (a, b) = (text(args, "a").unwrap_or("").to_string(), text(args, "b").unwrap_or("").to_string());
    let doc = match work_doc(ctx, args) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    if let Err(error) = node_exists(&doc, &a).and_then(|_| node_exists(&doc, &b)) {
        return fail(error);
    }
    let effect = match effect_arg(text(args, "effect").unwrap_or(super::transitions::DEFAULT_PRESET), true) {
        Ok(Ok(preset)) => preset,
        Ok(Err(_)) => return fail(ToolError::new(ErrorKind::NotAvailable, "library transitions (`asset:…`) are not available yet: the cut model's Transition takes preset names only (request to the video_cut owner)")),
        Err(error) => return fail(error),
    };
    let length = number(args, "length").unwrap_or(super::transitions::DEFAULT_TRANSITION);
    let (sa, sb) = (ops::timed_block(&doc, &a).and_then(|node| node.span).or_else(|| doc.node(&a).and_then(|node| node.span)), ops::timed_block(&doc, &b).and_then(|node| node.span).or_else(|| doc.node(&b).and_then(|node| node.span)));
    let meets = sa.zip(sb).is_some_and(|(sa, sb)| (sa.end() - sb.start).abs() <= 1.0 / doc.output.fps.max(1.0) + 1e-6);
    let op = if meets {
        match ctx.app.video_cut.plan_cut(&effect, &a, &b, length) {
            Ok(plan) => plan.op,
            Err(error) => return fail(ToolError::refused(error)),
        }
    } else if sa.zip(sb).is_some_and(|(sa, sb)| ops::overlap(sa, sb).is_some()) {
        CutOp::Transition { a, b, effect }
    } else {
        return fail(ToolError::refused(format!("`{a}` and `{b}` neither meet at a cut on one track nor overlap: a transition goes where one ends and the other starts, or over their overlap")));
    };
    ops_recipe(ctx, args, vec![Step::Op(op)])
}

fn move_block(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    let doc = match work_doc(ctx, args) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    if let Err(error) = node_exists(&doc, &node) {
        return fail(error);
    }
    let track = args.get("track").and_then(Json::as_u64).map(|track| track as u32).unwrap_or_else(|| doc.node(&node).map_or(0, |node| node.track));
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Move { node, start: number(args, "start").unwrap_or(0.0), track })])
}

fn trim_block(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Trim { node, start: number(args, "start").unwrap_or(0.0), length: number(args, "length").unwrap_or(1.0) })])
}

fn blade(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Split { node, t: number(args, "at").unwrap_or(0.0) })])
}

fn delete_block(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    let doc = match work_doc(ctx, args) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    if let Err(error) = node_exists(&doc, &node) {
        return fail(error);
    }
    if doc.graphic(&node).is_some() {
        return ops_recipe(ctx, args, vec![Step::Remove(vec![node])]);
    }
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Delete { node })])
}

fn set_param(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    let key = text(args, "key").unwrap_or("").to_string();
    if let Err(error) = work_doc(ctx, args).and_then(|doc| node_exists(&doc, &node)) {
        return fail(error);
    }
    let value = match patch_value(&key, args.get("value").unwrap_or(&Json::Null)) {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Param { node, key, value })])
}

fn set_header(ctx: &mut Ctx, args: &Json) -> Outcome {
    let output = args.get("output").filter(|output| !output.is_null()).map(|output| {
        (number(output, "width").unwrap_or(1920.0) as u32, number(output, "height").unwrap_or(1080.0) as u32, number(output, "fps").unwrap_or(60.0))
    });
    let header = Header { label: text(args, "label").map(str::to_string), duration: number(args, "duration"), bpm: number(args, "bpm"), output };
    if header.label.is_none() && header.duration.is_none() && header.bpm.is_none() && header.output.is_none() {
        return fail(ToolError::invalid("give at least one of `label`, `duration`, `bpm`, `output`"));
    }
    mutate(ctx, args, Recipe::Header(header))
}

fn set_audio(ctx: &mut Ctx, args: &Json) -> Outcome {
    let node = text(args, "node").unwrap_or("").to_string();
    let doc = match session_doc(ctx) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let shape = match doc.node(&node).map(|node| &node.kind) {
        Some(CutNodeKind::AudioClip { shape, .. } | CutNodeKind::Clip { shape, .. } | CutNodeKind::VoiceOver { shape, .. }) => shape.clone(),
        Some(_) => return fail(ToolError::invalid(format!("`{node}` has no sound (an AudioClip, a Clip or a VoiceOver has)"))),
        None => return fail(node_exists(&doc, &node).err().unwrap()),
    };
    let mut steps = Vec::new();
    if number(args, "fade_in").is_some() || number(args, "fade_out").is_some() {
        steps.push(Step::Op(CutOp::Fade { node: node.clone(), fade_in: number(args, "fade_in").unwrap_or(shape.fade_in), fade_out: number(args, "fade_out").unwrap_or(shape.fade_out) }));
    }
    if let Some(under) = args.get("duck_under").and_then(Json::as_arr) {
        let under: Vec<String> = under.iter().filter_map(Json::as_str).map(|id| id.trim_start_matches('@').to_string()).collect();
        steps.push(Step::Op(CutOp::Duck { node: node.clone(), under: (!under.is_empty()).then_some(under), depth_db: number(args, "depth_db") }));
    } else if let Some(depth) = number(args, "depth_db") {
        let Some(duck) = shape.duck else { return fail(ToolError::invalid("`depth_db` needs `duck_under` (the source is not ducked yet)")) };
        steps.push(Step::Op(CutOp::Duck { node: node.clone(), under: Some(duck.under), depth_db: Some(depth) }));
    }
    if steps.is_empty() {
        return fail(ToolError::invalid("give `fade_in`, `fade_out`, `duck_under` or `depth_db`"));
    }
    ops_recipe(ctx, args, steps)
}

/// `render_cut`: the Render card's settings (the dialog's, now in the
/// chat). They are remembered: the Render button (cmd-E) renders with the
/// last ones used. `path` writes the file there, `publish` also publishes
/// it; with neither the render folder's `render.mp4` is the result.
fn render_cut(ctx: &mut Ctx, args: &Json) -> Outcome {
    let draft = match args.get("quality").and_then(Json::as_str) {
        None | Some("final") => false,
        Some("draft") => true,
        Some(other) => return fail(ToolError::invalid(format!("`quality`: `{other}` (draft or final)"))),
    };
    let high = match args.get("bitrate").and_then(Json::as_str) {
        None | Some("standard") => false,
        Some("high") => true,
        Some(other) => return fail(ToolError::invalid(format!("`bitrate`: `{other}` (standard or high)"))),
    };
    let loudness = match args.get("loudness").and_then(Json::as_str) {
        None => 0,
        Some("social") => 1,
        Some("podcast") => 2,
        Some("broadcast") => 3,
        Some(other) => return fail(ToolError::invalid(format!("`loudness`: `{other}` (social, podcast or broadcast)"))),
    };
    let edit = &mut ctx.app.video_cut;
    let Some(session) = edit.session.as_ref() else { return fail(no_edit()) };
    if edit.render.is_some() {
        return fail(ToolError::refused("a render is already under way: see render_status"));
    }
    if (session.head.is_none() && edit.folder.is_none()) || session.dirty() || super::motion_edit::has_drafts(edit) || !page_idle(edit) || edit.saving || edit.save_requested {
        return fail(ToolError::refused("render a saved revision: call save_cut first"));
    }
    let (path, publish) = (text(args, "path"), flag(args, "publish").unwrap_or(false));
    edit.render_ui.export_draft = draft;
    edit.render_ui.export_high = high;
    edit.render_ui.export_fps = number(args, "fps");
    edit.render_ui.export_blend = flag(args, "blend").unwrap_or(false);
    edit.render_ui.export_path = path.unwrap_or_default().to_string();
    edit.render_ui.export_publish = publish;
    edit.loudness = loudness;
    edit.burn_off = flag(args, "burn_captions") == Some(false);
    edit.start_render();
    if edit.render.is_none() {
        return fail(ToolError::failed(edit.status.clone()));
    }
    // Where it goes: `path` (the button's own destination), the library
    // only, or the render folder's file with nothing copied or published.
    // A folder cut's render is its file: never published.
    match (path, publish && edit.folder.is_none()) {
        (Some(_), _) => {}
        (None, true) => edit.render_ui.export_to = None,
        (None, false) => {
            edit.render_ui.export_to = None;
            edit.render_ui.api_file = true;
        }
    }
    let identity = document_identity(ctx.app, DocKind::Cut);
    done(json::obj(vec![("started", Json::Bool(true)), ("quality", json::s(if draft { "draft" } else { "final" })), ("document", identity)]))
}

/// Web export (FB1): waits for the export, answered with its
/// files, local URL and diagnostics.
fn export_web(ctx: &mut Ctx, args: &Json) -> Outcome {
    if args.get("stop").and_then(Json::as_bool).unwrap_or(false) {
        let running = ctx.app.video_cut.web_export_running();
        ctx.app.video_cut.stop_web_export();
        return done(json::obj(vec![("stopped", Json::Bool(running))]));
    }
    match ctx.app.video_cut.web_export_api_start(ctx.cx, args) {
        Ok(id) => Outcome::Wait(Wait::WebExport { id }),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn render_status(ctx: &mut Ctx, _: &Json) -> Outcome {
    let edit = &ctx.app.video_cut;
    let mut fields = vec![];
    if let Some((project, path)) = &edit.render_project {
        fields.push(("document", json::obj(vec![("project", json::s(project)), ("path", json::s(path))])));
    }
    match &edit.render {
        Some(render) => {
            fields.push(("state", json::s(if render.publishing { "publishing" } else { "rendering" })));
            fields.push(("frame", Json::Int(render.progress.0 as i64)));
            fields.push(("frames", Json::Int(render.progress.1 as i64)));
        }
        None => {
            let finished = edit.last_render.is_some() || edit.render_done.is_some();
            fields.push(("state", json::s(if edit.render_failed.is_some() { "failed" } else if finished { "done" } else { "none" })));
            if let Some(error) = &edit.render_failed {
                fields.push(("error", json::s(error.clone())));
            }
            // Where the finished render is (an exported or API-rendered file).
            if let (None, Some(file)) = (&edit.render_failed, &edit.render_ui.last_file) {
                fields.push(("file", json::s(file.clone())));
            }
            if let Some(render) = &edit.last_render {
                if edit.render_project.is_some() { fields.push(("published", Json::Bool(true))); }
                else { fields.push(("render", reference_json(render))); }
            }
        }
    }
    done(json::obj(fields))
}

// ------------------------------------------------------------ motion (D2's library)

fn motion_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources").join("motion")
}

fn motion_templates(_: &mut Ctx, args: &Json) -> Outcome {
    let dir = motion_dir();
    let mut entries: Vec<(String, String)> = std::fs::read_dir(&dir).map(|read| read.flatten().filter_map(|entry| {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("splash") {
            return None;
        }
        Some((path.file_stem()?.to_str()?.to_string(), std::fs::read_to_string(&path).ok()?))
    }).collect()).unwrap_or_default();
    entries.sort();
    if let Some(name) = text(args, "name") {
        return match entries.iter().find(|(stem, _)| stem == name) {
            Some((_, source)) => done(json::obj(vec![("name", json::s(name)), ("source", json::s(source.clone()))])),
            None => fail(ToolError::not_found(format!("no template `{name}`; call motion_templates with no name for the list"))),
        };
    }
    let list: Vec<Json> = entries.iter().map(|(stem, source)| {
        // The first comment block is the template's note.
        let note: String = source.lines().take_while(|line| line.trim_start().starts_with("//")).map(|line| line.trim_start().trim_start_matches('/').trim()).collect::<Vec<_>>().join(" ");
        json::obj(vec![("name", json::s(stem.clone())), ("note", json::s(note))])
    }).collect();
    let note = if list.is_empty() { "the motion library is empty in this build (D2 is writing it)" } else { "" };
    done(json::obj(vec![("templates", Json::Arr(list)), ("note", json::s(note))]))
}

// ------------------------------------------------------------ effects

// ------------------------------------------------------------ library

fn analyze_music(ctx: &mut Ctx, args: &Json) -> Outcome {
    match reference(args.get("music"), "music") {
        Ok(asset) => slow(ctx, SlowJob::MusicAnalysis { asset }, Then::Reply, false),
        Err(error) => fail(error),
    }
}

/// Decode a music asset and analyse it: the summary a model directs to,
/// with each section's character in words.
fn music_analysis(client: &mut AssetClient, asset: &AssetRevisionRef) -> Result<Json, ToolError> {
    use makepad_story_edit_render::MediaResolver;
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let reference = MediaRef::Asset(AssetRef { id: asset.asset_id.to_string(), rev: asset.revision.to_string() });
    let file = store.resolve(&reference).map_err(|error| ToolError::failed(format!("the music does not resolve: {error}")))?;
    let bytes = std::fs::read(&file.path).map_err(|error| ToolError::failed(format!("{}: {error}", file.path.display())))?;
    // Generated music and voice come as WAV, which `decode_any` does not read.
    if bytes.starts_with(b"RIFF") {
        let pcm = makepad_stage_audio_media::decode::parse_wav(&bytes, usize::MAX)
            .map_err(|error| ToolError::failed(format!("the music does not decode: {error}")))?;
        let mono: Vec<f32> = pcm.frames.iter().map(|[l, r]| (*l as f32 + *r as f32) * (0.5 / 32768.0)).collect();
        let features = makepad_story_audio_features::analyze_music(&mono, pcm.sample_rate as f64);
        return Ok(makepad_story_audio_features::json::music_summary(&features));
    }
    let audio = makepad_audio_decode::decode_any(&bytes).map_err(|error| ToolError::failed(format!("the music does not decode: {error:?}")))?;
    let channels = audio.channels.max(1) as usize;
    let left = audio.channel(0);
    let mono: Vec<f32> = if channels > 1 {
        let right = audio.channel(1);
        left.iter().zip(&right).map(|(l, r)| (l + r) * 0.5).collect()
    } else {
        left
    };
    let features = makepad_story_audio_features::analyze_music(&mono, audio.rate as f64);
    Ok(makepad_story_audio_features::json::music_summary(&features))
}

/// A deck of the library: by exact revision, or by alias (its head).
fn open_deck_source(client: &mut AssetClient, asset: Option<AssetRevisionRef>, alias: Option<&str>) -> Result<super::store::OpenTree, ToolError> {
    let reference = match (asset, alias) {
        (Some(asset), _) => asset,
        (None, Some(alias)) => {
            let alias: makepad_asset_data::AssetAlias = alias.parse().map_err(|_| ToolError::invalid(format!("`{alias}` is not an alias (namespace/path, e.g. stage/examples/tour/deck)")))?;
            let found = client.resolve_alias(&alias).map_err(|error| ToolError::not_found(redact(error)))?;
            AssetRevisionRef { asset_id: found.asset_id, revision: found.head_revision }
        }
        (None, None) => return Err(ToolError::invalid("a `deck` or an `alias` is needed")),
    };
    let alias = alias.map(str::parse).transpose().map_err(|_| ToolError::invalid("invalid project path"))?;
    super::store::open_tree(client, reference, alias, false).map_err(ToolError::refused)
}

fn open_source(client: &mut AssetClient, asset: &AssetRevisionRef) -> Result<String, ToolError> {
    let manifest = client.resolve_ref(asset).map_err(|error| ToolError::not_found(redact(error)))?;
    if manifest.kind != AssetKind::Cut {
        return Err(ToolError::invalid(format!("that asset is {}, not a cut", kind_name(manifest.kind))));
    }
    let file = select_file(&manifest, FileRole::Source, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).map_err(|error| ToolError::failed(redact(error)))?;
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|error| ToolError::failed(redact(error)))?;
    String::from_utf8(bytes).map_err(|_| ToolError::failed("the cut's source is not UTF-8"))
}

// ------------------------------------------------------------ app runs (L6)

fn list_apps(_: &mut Ctx, _: &Json) -> Outcome {
    let apps: Vec<Json> = crate::deck::app_choices().map(|entry| json::obj(vec![
        ("id", json::s(entry.id)),
        ("label", json::s(entry.label)),
        ("package", json::s(entry.package)),
        ("built", Json::Bool(makepad_app_registry::is_built(entry))),
    ])).collect();
    done(json::obj(vec![("apps", Json::Arr(apps))]))
}

fn app_dump(ctx: &mut Ctx, args: &Json) -> Outcome {
    let output = makepad_story_app_run::RunOutput {
        width: args.get("width").and_then(Json::as_u64).unwrap_or(1920) as u32,
        height: args.get("height").and_then(Json::as_u64).unwrap_or(1080) as u32,
        dpi_scale: number(args, "dpi_scale").unwrap_or(2.0),
        fps: 60,
    };
    slow(ctx, SlowJob::AppDump { app: text(args, "app").unwrap_or("").to_string(), output, build: flag(args, "build").unwrap_or(true) }, Then::Reply, true)
}

fn validate_app_run(ctx: &mut Ctx, args: &Json) -> Outcome {
    let source = args.get("source").and_then(Json::as_str).unwrap_or("").to_string();
    let run = flag(args, "run").unwrap_or(false);
    if !run {
        return Outcome::Done(validate_run(&source, false).map(ToolOutput::json));
    }
    slow(ctx, SlowJob::ValidateRun { source, run }, Then::Reply, true)
}

fn run_problems(diagnostics: Vec<makepad_story_app_run::Diagnostic>) -> Vec<Json> {
    diagnostics.into_iter().map(|diagnostic| json::s(format!("{}:{}: {}", diagnostic.line, diagnostic.col, diagnostic.message))).collect()
}

fn validate_run(source: &str, run: bool) -> Result<Json, ToolError> {
    let doc = match makepad_story_app_run::AppRunDoc::load(source) {
        Ok(doc) => doc,
        Err(diagnostics) => return Ok(json::obj(vec![("ok", Json::Bool(false)), ("errors", Json::Arr(run_problems(diagnostics)))])),
    };
    let mut fields = vec![
        ("ok", Json::Bool(true)),
        ("app", json::s(doc.app.clone())),
        ("output", json::obj(vec![("width", Json::Int(doc.output.width as i64)), ("height", Json::Int(doc.output.height as i64)), ("fps", Json::Int(doc.output.fps as i64))])),
        ("warnings", Json::Arr(doc.output.problems().into_iter().map(json::s).collect())),
    ];
    if run {
        let mut options = makepad_story_app_run::RunOptions::new(slow_dir("validate").join(&doc.app));
        options.validate = true;
        let result = makepad_story_app_run::execute(source, options).map_err(ToolError::failed)?;
        fields[0].1 = Json::Bool(result.ok());
        fields.push(("marks", json::obj(result.marks().iter().map(|(name, frame)| (name.as_str(), json::obj(vec![("frame", Json::Int(*frame as i64)), ("seconds", num(*frame as f64 / makepad_story_app_run::VALIDATE_FPS as f64))]))).collect())));
        fields.push(("failures", Json::Arr(result.failures.iter().cloned().map(json::s).collect())));
        fields.push(("run_warnings", Json::Arr(result.warnings.iter().cloned().map(json::s).collect())));
    }
    Ok(json::obj(fields))
}

fn run_app_run(ctx: &mut Ctx, args: &Json) -> Outcome {
    let source = args.get("source").and_then(Json::as_str).unwrap_or("").to_string();
    if let Err(diagnostics) = makepad_story_app_run::AppRunDoc::load(&source) {
        return fail(ToolError::refused(format!("the script does not load:\n{}", run_problems(diagnostics).iter().filter_map(Json::as_str).collect::<Vec<_>>().join("\n"))));
    }
    let run = match args.get("run").filter(|run| !run.is_null()) {
        Some(run) => match reference(Some(run), "run") {
            Ok(run) => Some(run),
            Err(error) => return fail(error),
        },
        None => None,
    };
    let job = ctx.host.next();
    ctx.host.state.app_runs.insert(job, AppRunState::Running { since: Cx::time_now() });
    let ticket = ctx.host.next();
    let Some(worker) = ctx.host.state.runs.as_mut() else { return fail(ToolError::failed("the app-run worker is not running")) };
    worker.send((ticket, SlowJob::RunApp { source, run, label: text(args, "label").unwrap_or("").to_string() }));
    // The run finishes in the background: its reply lands in `app_runs`.
    ctx.host.waiting.push(Waiting { reply: 0, tool: "run_app_run", origin: String::new(), wait: Wait::Slow { ticket, then: Then::AppRun { job } } });
    done(json::obj(vec![("job", Json::Int(job as i64)), ("note", json::s("building and running; follow with app_run_status"))]))
}

fn run_app(client: &mut AssetClient, source: &str, run: Option<AssetRevisionRef>, label: &str) -> Result<Json, ToolError> {
    let doc = makepad_story_app_run::AppRunDoc::load(source).map_err(|diagnostics| ToolError::refused(run_problems(diagnostics).iter().filter_map(Json::as_str).collect::<Vec<_>>().join("\n")))?;
    let mut options = makepad_story_app_run::RunOptions::new(slow_dir("runs").join(format!("{}-{}", doc.app, makepad_story_app_run::unpublished_revision(source))));
    options.thumbnail = true;
    let mut result = makepad_story_app_run::execute(source, options).map_err(ToolError::failed)?;
    if !result.ok() {
        return Err(ToolError::failed(format!("the run failed: {}", result.failures.join("; "))));
    }
    let label = if label.trim().is_empty() { doc.label.clone() } else { label.to_string() };
    let published = makepad_story_app_run::publish_run(client, "stage", run, source, &label, &mut result).map_err(|failure| ToolError::failed(format!("{failure:?}")))?;
    let fps = result.output.fps.max(1) as f64;
    Ok(json::obj(vec![
        ("run", reference_json(&published.run)),
        ("render", reference_json(&published.render)),
        ("frames", Json::Int(result.frames as i64)),
        ("marks", json::obj(result.marks().iter().map(|(name, frame)| (name.as_str(), num(*frame as f64 / fps))).collect())),
        ("warnings", Json::Arr(result.warnings.iter().cloned().map(json::s).collect())),
    ]))
}

fn app_run_json(host: &Host, job: u64) -> Json {
    match host.state.app_runs.get(&job) {
        Some(AppRunState::Running { since }) => json::obj(vec![("job", Json::Int(job as i64)), ("state", json::s("running")), ("secs", num(Cx::time_now() - since))]),
        Some(AppRunState::Done(value)) => json::obj(vec![("job", Json::Int(job as i64)), ("state", json::s("done")), ("result", value.clone())]),
        Some(AppRunState::Failed(error)) => json::obj(vec![("job", Json::Int(job as i64)), ("state", json::s("failed")), ("error", json::s(error.clone()))]),
        None => json::obj(vec![("job", Json::Int(job as i64)), ("state", json::s("unknown"))]),
    }
}

fn app_run_status(ctx: &mut Ctx, args: &Json) -> Outcome {
    let job = args.get("job").and_then(Json::as_u64).unwrap_or(0);
    if !ctx.host.state.app_runs.contains_key(&job) {
        return fail(ToolError::not_found(format!("no app-run job {job}")));
    }
    Outcome::Wait(Wait::AppRun { job, deadline: Cx::time_now() + number(args, "wait").unwrap_or(0.0) })
}

fn read_app_run(ctx: &mut Ctx, args: &Json) -> Outcome {
    match reference(args.get("render"), "render") {
        Ok(render) => slow(ctx, SlowJob::ReadRun { render }, Then::Reply, false),
        Err(error) => fail(error),
    }
}

fn read_run(client: &mut AssetClient, render: &AssetRevisionRef) -> Result<Json, ToolError> {
    let manifest = client.resolve_ref(render).map_err(|error| ToolError::not_found(redact(error)))?;
    let file = manifest.files.iter().find(|file| file.role == FileRole::Source && file.media == MediaType::Text)
        .or_else(|| manifest.files.iter().find(|file| file.media == MediaType::Text))
        .ok_or_else(|| ToolError::not_found("that render has no run.json (is it an app run's render?)"))?;
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|error| ToolError::failed(redact(error)))?;
    let record = json::parse_depth(&bytes, 64).map_err(|error| ToolError::failed(format!("run.json: {error}")))?;
    let fps = record.get("output").and_then(|output| output.get("fps")).and_then(Json::as_u64).unwrap_or(60).max(1) as f64;
    let marks = match record.get("marks") {
        Some(Json::Obj(marks)) => marks.iter().filter_map(|(name, frame)| Some((name.as_str(), json::obj(vec![("frame", frame.clone()), ("seconds", num(frame.as_u64()? as f64 / fps))])))).collect(),
        _ => Vec::new(),
    };
    let mut fields = vec![("marks", json::obj(marks))];
    for key in ["output", "build", "calls", "failures"] {
        if let Some(value) = record.get(key) {
            fields.push((key, value.clone()));
        }
    }
    if let Some(Json::Arr(track)) = record.get("focus") {
        fields.push(("focus_entries", Json::Int(track.len() as i64)));
    }
    Ok(json::obj(fields))
}

// ------------------------------------------------------------ audio

fn grid(doc: &CutDoc, args: &Json) -> beats::Grid {
    beats::Grid { bpm: doc.bpm.max(1.0), offset: 0.0, beats_per_bar: args.get("beats_per_bar").and_then(Json::as_u64).unwrap_or(4).max(1) as u32 }
}

fn beat_grid(ctx: &mut Ctx, args: &Json) -> Outcome {
    let doc = match session_doc(ctx) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let grid = grid(&doc, args);
    let beats = grid.beats(doc.duration);
    done(json::obj(vec![
        ("bpm", num(grid.bpm)),
        ("source", json::s("the cut's bpm, beat 0 at 0 s (a music track's baked grid is not read yet)")),
        ("beat_seconds", num(grid.beat_len())),
        ("bar_seconds", num(grid.step(beats::Unit::Bar))),
        ("downbeats", Json::Arr(beats.iter().filter(|(_, down)| *down).map(|(t, _)| num(*t)).collect())),
        ("beats", Json::Int(beats.len() as i64)),
    ]))
}

fn snap_to_beats(ctx: &mut Ctx, args: &Json) -> Outcome {
    let doc = match session_doc(ctx) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let unit = beats::Unit::from_id(text(args, "to").unwrap_or("beat")).unwrap_or(beats::Unit::Beat);
    let edges = beats::Edges::from_id(text(args, "edges").unwrap_or("both")).unwrap_or(beats::Edges::Both);
    let wanted: Option<Vec<String>> = args.get("nodes").and_then(Json::as_arr).map(|nodes| nodes.iter().filter_map(Json::as_str).map(str::to_string).collect());
    if let Some(wanted) = &wanted {
        for id in wanted {
            if let Err(error) = node_exists(&doc, id) {
                return fail(error);
            }
        }
    }
    let blocks: Vec<(String, f64, f64)> = doc.nodes.iter()
        .filter(|node| ops::lane_of(node).is_some_and(|lane| matches!(lane, ops::Lane::Video | ops::Lane::Audio)))
        .filter(|node| wanted.as_ref().is_none_or(|wanted| wanted.contains(&node.id)))
        .filter_map(|node| node.span.map(|span| (node.id.clone(), span.start, span.length))).collect();
    let moved = beats::snap_spans(&blocks, &grid(&doc, args), unit, edges);
    if moved.is_empty() {
        return done(json::obj(vec![("revision", Json::Int(ctx.app.video_cut.session.as_ref().map_or(0, |session| session.revision) as i64)), ("note", json::s("every edge is already on the grid"))]));
    }
    let spans = moved.into_iter().map(|(node, start, length)| {
        // A moved start moves the source `in` with it, as a trim does.
        let old = doc.node(&node).and_then(|node| node.span).map_or(start, |span| span.start);
        let source_in = match doc.node(&node).map(|node| &node.kind) {
            Some(CutNodeKind::Clip { source_in: SourceTime::Seconds(at), speed, .. }) => Some((at + (start - old) * speed).max(0.0)),
            Some(CutNodeKind::AudioClip { source_in: SourceTime::Seconds(at), .. }) => Some((at + (start - old)).max(0.0)),
            _ => None,
        };
        Respan { node, start, length, source_in }
    }).collect();
    ops_recipe(ctx, args, vec![Step::Op(CutOp::Respan { spans })])
}

fn fit_to_bars(ctx: &mut Ctx, args: &Json) -> Outcome {
    let doc = match session_doc(ctx) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let grid = grid(&doc, args);
    let bar = grid.step(beats::Unit::Bar);
    // Never shorter than the content: the loader refuses a span past the end.
    let content_end = doc.nodes.iter().filter_map(|node| node.span).map(|span| span.end()).fold(0.0, f64::max);
    let at_least = (content_end / bar - 1e-6).ceil().max(1.0);
    let duration = match args.get("bars").and_then(Json::as_u64) {
        Some(bars) if (bars as f64) < at_least => return fail(ToolError::refused(format!("{bars} bars ({:.2} s) end before the last block ({content_end:.2} s): at least {at_least} bars", bars as f64 * bar))),
        Some(bars) => bars as f64 * bar,
        None => grid.whole_bars(doc.duration).0.max(at_least as u32) as f64 * bar,
    };
    mutate(ctx, args, Recipe::Header(Header { duration: Some(duration), ..Header::default() }))
}

fn voice_lines_json(edit: &VideoCut, only: Option<&str>) -> Json {
    let Some(session) = edit.session.as_ref() else { return json::obj(vec![("voices", Json::Arr(Vec::new()))]) };
    let (eval, _): (CutEval, _) = edit.eval(&session.doc);
    let voices: Vec<Json> = session.doc.nodes.iter().filter(|node| only.is_none_or(|only| only == node.id)).filter_map(|node| {
        let CutNodeKind::VoiceOver { voice, lines, .. } = &node.kind else { return None };
        let placed = eval.voice_lines(&node.id);
        Some(json::obj(vec![
            ("node", json::s(node.id.clone())),
            ("voice", json::s(voice.id.clone())),
            ("lines", Json::Arr(lines.iter().enumerate().map(|(index, line)| {
                let place = placed.iter().find(|placed| placed.index == index);
                json::obj(vec![
                    ("text", json::s(line.text.clone())),
                    ("start", place.map(|place| num(place.start)).unwrap_or(Json::Null)),
                    ("end", place.map(|place| num(place.end)).unwrap_or(Json::Null)),
                    ("spoken", Json::Bool(line.take.is_some() && !line.stale(voice))),
                    ("words", Json::Arr(place.map(|place| place.words.iter().map(|word| json::obj(vec![("text", json::s(word.text.clone())), ("start", num(word.start)), ("end", num(word.end))])).collect()).unwrap_or_default())),
                ])
            }).collect())),
        ]))
    }).collect();
    json::obj(vec![("voices", Json::Arr(voices))])
}

fn voice_lines_tool(ctx: &mut Ctx, args: &Json) -> Outcome {
    if ctx.app.video_cut.session.is_none() {
        return fail(no_edit());
    }
    done(voice_lines_json(&ctx.app.video_cut, text(args, "node")))
}

fn speak(ctx: &mut Ctx, args: &Json) -> Outcome {
    let doc = match session_doc(ctx) {
        Ok(doc) => doc,
        Err(error) => return fail(error),
    };
    let node = match text(args, "node") {
        Some(node) => node.to_string(),
        None => match doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::VoiceOver { .. })) {
            Some(node) => node.id.clone(),
            None => return fail(ToolError::not_found("the cut has no VoiceOver: add_voiceover first")),
        },
    };
    if !ctx.app.video_cut.is_voice(&node) {
        return fail(ToolError::not_found(format!("`{node}` is not a VoiceOver (call timeline)")));
    }
    note_author(&mut ctx.app.video_cut, ctx.origin, "speak");
    ctx.app.video_cut.speak(&node, None, text(args, "voice"));
    Outcome::Wait(Wait::Speak { node })
}

// ------------------------------------------------------------ feedback

fn inspect(ctx: &mut Ctx, args: &Json) -> Outcome {
    match text(args, "target").unwrap_or(target(ctx.app).key()) {
        "deck" | "@deck" => match ctx.app.deck.session() {
            Some(session) => match session.tree.clone() {
                Some(tree) => slow(ctx, SlowJob::InspectTree(tree), Then::Reply, false),
                None => fail(ToolError::refused("no deck tree open")),
            },
            None => fail(no_doc(DocKind::Deck)),
        },
        "cut" | "@cut" => match ctx.app.video_cut.session.as_ref() {
            Some(session) => {
                if let Some(tree) = session.tree.clone() { return slow(ctx, SlowJob::InspectTree(tree), Then::Reply, false); }
                let source = session.source.clone();
                let motions = ctx.host.state.motions.iter()
                    .map(|(name, (asset, _, motion))| (name.clone(), motion.clone(), source.contains(&asset.asset_id.to_string())))
                    .collect();
                slow(ctx, SlowJob::Inspect { source, name: "cut.splash".into(), motions }, Then::Reply, false)
            }
            None => fail(no_edit()),
        },
        name => {
            let Some((_, _, source)) = ctx.host.state.motions.get(name.trim_start_matches("motion:")) else {
                return fail(ToolError::not_found(format!("no Motion document `{name}` in this session (list_motions)")));
            };
            // The full checks (safe area, overflow, overlap, holds), as on the cut.
            if makepad_motion::MotionDoc::load(source).is_ok() {
                let job = SlowJob::Inspect { source: source.clone(), name: format!("{}.splash", name.trim_start_matches("motion:")), motions: Vec::new() };
                return slow(ctx, job, Then::Reply, false);
            }
            let report = validate_motion_json(source);
            let list = |key: &str| Json::Arr(report.get(key).and_then(Json::as_arr).unwrap_or(&[]).iter().map(|diag| {
                json::s(format!("{}:{} {}{}", diag.get("line").and_then(Json::as_u64).unwrap_or(0), diag.get("col").and_then(Json::as_u64).unwrap_or(0),
                    diag.get("message").and_then(Json::as_str).unwrap_or(""), diag.get("hint").and_then(Json::as_str).map(|hint| format!(" — {hint}")).unwrap_or_default()))
            }).collect());
            done(json::obj(vec![("errors", list("errors")), ("warnings", list("warnings")), ("clean", report.get("ok").cloned().unwrap_or(Json::Bool(false)))]))
        }
    }
}

// ------------------------------------------------------------ motion (M1)

fn motion_diag_json(diag: &makepad_motion::Diagnostic) -> Json {
    json::obj(vec![
        ("line", Json::Int(diag.loc.line as i64)),
        ("col", Json::Int(diag.loc.col as i64)),
        ("path", json::s(diag.path.clone())),
        ("message", json::s(diag.message.clone())),
        ("hint", diag.hint.clone().map(json::s).unwrap_or(Json::Null)),
    ])
}

/// A Motion document's load report: `ok`, every error and warning with its
/// place and fix, and what the document declares.
fn validate_motion_json(source: &str) -> Json {
    match makepad_motion::MotionDoc::load(source) {
        Ok(doc) => json::obj(vec![
            ("ok", Json::Bool(true)),
            ("errors", Json::Arr(Vec::new())),
            ("warnings", Json::Arr(doc.warnings.iter().map(motion_diag_json).collect())),
            ("label", json::s(doc.label.clone())),
            ("size", Json::Arr(vec![num(doc.size[0]), num(doc.size[1])])),
            ("duration", num(doc.duration)),
            ("inputs", Json::Arr(doc.inputs.iter().map(|input| json::obj(vec![("name", json::s(input.name.clone())), ("type", json::s(input.ty.id())), ("doc", json::s(input.doc.clone()))])).collect())),
            ("layers", Json::Int(doc.all_layers().len() as i64)),
            ("animated", Json::Bool(doc.all_layers().iter().any(|layer| layer.animated))),
        ]),
        Err(diags) => json::obj(vec![("ok", Json::Bool(false)), ("errors", Json::Arr(diags.iter().map(motion_diag_json).collect()))]),
    }
}

/// The load errors of a refused document, as the model reads them.
fn motion_errors(value: &Json) -> String {
    let errors: Vec<String> = value.get("errors").and_then(Json::as_arr).unwrap_or(&[]).iter().map(|error| {
        let at = format!("{}:{}", error.get("line").and_then(Json::as_u64).unwrap_or(0), error.get("col").and_then(Json::as_u64).unwrap_or(0));
        let hint = error.get("hint").and_then(Json::as_str).map(|hint| format!(" — {hint}")).unwrap_or_default();
        format!("{at} {}{hint}", error.get("message").and_then(Json::as_str).unwrap_or(""))
    }).collect();
    format!("the Motion document does not load (nothing was published):\n{}", errors.join("\n"))
}

fn publish_motion(client: &mut AssetClient, title: &str, source: &str, asset: Option<AssetId>) -> Result<Json, ToolError> {
    if makepad_motion::modules::is_module(source) {
        return publish_module(client, title, source, asset);
    }
    let report = validate_motion_json(source);
    if report.get("ok").and_then(Json::as_bool) != Some(true) {
        return Ok(report);
    }
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let mut bundle = PublishBundle::new("stage", AssetKind::Motion, title.to_string(), files, placeholder_jpeg(title)?, PublishRights::generated_cc0());
    bundle.asset_id = asset;
    bundle.generator = "stage-studio-api".into();
    bundle.tags = vec!["motion".into()];
    let published = client.publish_bundle(&bundle).map_err(|error| ToolError::failed(redact(error)))?;
    let reference = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
    let mut fields = vec![
        ("asset", reference_json(&reference)),
        ("use", json::s(format!("Graphic{{src: asset(\"{}\", \"{}\") span: [start, length]}}", reference.asset_id, reference.revision))),
    ];
    if let Json::Obj(report) = report {
        fields.extend(report.into_iter().filter(|(key, _)| key != "ok" && key != "errors").map(|(key, value)| (match key.as_str() {
            "warnings" => "warnings",
            "label" => "label",
            "size" => "size",
            "duration" => "duration",
            "inputs" => "inputs",
            "layers" => "layers",
            _ => "animated",
        }, value)));
    }
    Ok(json::obj(fields))
}

/// A Splash module (a source without `Motion{`, ending in an object of its
/// exports): checked as a document imports it, published as a Motion asset
/// tagged `module`, answered with its import line.
fn publish_module(client: &mut AssetClient, title: &str, source: &str, asset: Option<AssetId>) -> Result<Json, ToolError> {
    let info = match makepad_motion::modules::check(source, Default::default()) {
        Ok(info) => info,
        Err(errors) => return Ok(json::obj(vec![("ok", Json::Bool(false)), ("errors", Json::Arr(errors.into_iter().map(json::s).collect()))])),
    };
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let mut bundle = PublishBundle::new("stage", AssetKind::Motion, title.to_string(), files, placeholder_jpeg(title)?, PublishRights::generated_cc0());
    bundle.asset_id = asset;
    bundle.generator = "stage-studio-api".into();
    bundle.tags = vec!["motion".into(), makepad_motion::modules::MODULE_TAG.into()];
    let published = client.publish_bundle(&bundle).map_err(|error| ToolError::failed(redact(error)))?;
    let reference = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
    Ok(json::obj(vec![
        ("asset", reference_json(&reference)),
        ("use", json::s(format!("let {} = import(asset(\"{}\", \"{}\"))", title.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("_"), reference.asset_id, reference.revision))),
        ("module", json::s(info.doc)),
        ("exports", Json::Arr(info.exports.into_iter().map(json::s).collect())),
    ]))
}

/// A text asset's `Source` file (a cut's or a Motion document's).
fn text_source(client: &mut AssetClient, asset: &AssetRevisionRef, kind: AssetKind) -> Result<String, ToolError> {
    let manifest = client.resolve_ref(asset).map_err(|error| ToolError::not_found(redact(error)))?;
    if manifest.kind != kind {
        return Err(ToolError::invalid(format!("that asset is {}, not {}", kind_name(manifest.kind), kind_name(kind))));
    }
    let file = select_file(&manifest, FileRole::Source, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).map_err(|error| ToolError::failed(redact(error)))?;
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|error| ToolError::failed(redact(error)))?;
    String::from_utf8(bytes).map_err(|_| ToolError::failed("the source is not UTF-8"))
}

fn motion_catalog(_: &mut Ctx, args: &Json) -> Outcome {
    let types: Vec<&str> = makepad_motion::layer_types().collect();
    let layers: Vec<&str> = match text(args, "layer") {
        Some(layer) => match types.iter().find(|name| name.eq_ignore_ascii_case(layer)) {
            Some(name) => vec![name],
            None => return fail(ToolError::not_found(format!("no layer type `{layer}`; the types are {}", types.join(", ")))),
        },
        None => types,
    };
    let entries: Vec<Json> = layers.iter().map(|layer| json::obj(vec![("type", json::s(*layer)), ("fields", Json::Arr(makepad_motion::catalog::layer_fields(layer).into_iter().map(json::s).collect()))])).collect();
    done(json::obj(vec![
        ("document", json::s("use mod.motion.*\nMotion{size: vec2(w, h) duration: seconds fps bpm background inputs: {name: input(@text|@number|@color|@vec2|@bool|@image|@svg|@video|@model, default, \"doc\")} layers: [ … ]}")),
        ("layers", Json::Arr(entries)),
        ("fonts", Json::Arr(makepad_doc_fonts::bundled().map(json::s).collect())),
        // Families Stage installs on demand (its Resources panel), named like the
        // bundled ones; a document naming one that is not installed draws it in the default face.
        ("installable_fonts", Json::Arr(makepad_doc_fonts::downloadable().map(|family| json::obj(vec![
            ("name", json::s(family.name)),
            ("installed", Json::Bool(makepad_doc_fonts::installed(family))),
            ("role", json::s(family.role)),
        ])).collect())),
        ("shader_reserved_uniforms", Json::Arr(makepad_motion::RESERVED_UNIFORMS.iter().map(|name| json::s(*name)).collect())),
        ("prelude", json::s(makepad_motion::PRELUDE.trim())),
        ("note", json::s("every property is a value, keys([t, v], [t, v, @ease]), or sugar (tween, sequence, loop, stagger, see the prelude); validate_motion checks a document and names the fix for every error")),
    ]))
}

/// The fields a type takes, from the readers (the field registry).
fn motion_fields(_: &mut Ctx, args: &Json) -> Outcome {
    use makepad_motion::field_registry::{describe, lookup, type_names};
    let name = text(args, "type").filter(|name| !name.is_empty());
    let field = text(args, "field").filter(|field| !field.is_empty());
    let Some(name) = name else {
        return done(json::obj(vec![("types", Json::Arr(type_names().into_iter().map(json::s).collect()))]));
    };
    let found = lookup(name);
    let has = |t: &makepad_motion::field_registry::TypeInfo| field.is_none_or(|field| t.field(field).is_some());
    if !found.iter().any(has) {
        return fail(ToolError::not_found(describe(Some(name), field).err().unwrap_or_else(|| format!("no field `{}` on {name}", field.unwrap_or_default()))));
    }
    let types: Vec<Json> = found
        .iter()
        .filter(|t| has(t))
        .map(|t| {
            let fields: Vec<Json> = t
                .fields
                .iter()
                .filter(|f| field.is_none_or(|field| f.name == field))
                .map(|f| {
                    let mut entry = vec![("name", json::s(&f.name)), ("takes", json::s(&f.ty))];
                    if let Some(default) = &f.default {
                        entry.push(("default", json::s(default)));
                    }
                    if let Some([lo, hi]) = f.range {
                        entry.push(("range", Json::Arr(vec![Json::F64(lo), Json::F64(hi)])));
                    }
                    if let Some(unit) = f.unit {
                        entry.push(("unit", json::s(unit)));
                    }
                    if let Some(doc) = f.doc {
                        entry.push(("doc", json::s(doc)));
                    }
                    json::obj(entry)
                })
                .collect();
            json::obj(vec![("type", json::s(&t.name)), ("family", json::s(t.family.name())), ("fields", Json::Arr(fields))])
        })
        .collect();
    done(json::obj(vec![("types", Json::Arr(types))]))
}

fn list_motions(ctx: &mut Ctx, _: &Json) -> Outcome {
    let source = ctx.app.video_cut.session.as_ref().map(|session| session.source.clone()).unwrap_or_default();
    let mut motions: Vec<Json> = ctx.host.state.motions.iter().map(|(name, (asset, title, source_text))| {
        let used = source.contains(&format!("asset(\"{}\", \"{}\")", asset.asset_id, asset.revision));
        let report = validate_motion_json(source_text);
        json::obj(vec![
            ("name", json::s(name.clone())),
            ("title", json::s(title.clone())),
            ("asset", reference_json(asset)),
            ("size", report.get("size").cloned().unwrap_or(Json::Null)),
            ("duration", report.get("duration").cloned().unwrap_or(Json::Null)),
            ("inputs", report.get("inputs").cloned().unwrap_or(Json::Null)),
            ("used_in_cut", Json::Bool(used)),
        ])
    }).collect();
    motions.sort_by_key(|motion| motion.get("name").and_then(Json::as_str).unwrap_or("").to_string());
    done(json::obj(vec![("motions", Json::Arr(motions))]))
}

/// `asset:<id>:<rev>` to a revision.
fn asset_arg(text: &str) -> Option<AssetRevisionRef> {
    let (id, rev) = text.strip_prefix("asset:")?.split_once(':')?;
    Some(AssetRevisionRef { asset_id: id.parse().ok()?, revision: rev.parse().ok()? })
}

fn read_motion(ctx: &mut Ctx, args: &Json) -> Outcome {
    let name = text(args, "name").unwrap_or("").to_string();
    if let Some((_, _, source)) = ctx.host.state.motions.get(&name) {
        return done(json::obj(vec![("name", json::s(name.clone())), ("source", json::s(source.clone()))]));
    }
    match asset_arg(&name) {
        Some(asset) => slow(ctx, SlowJob::MotionSource { asset }, Then::MotionSource { name }, false),
        None => fail(ToolError::not_found(format!("no Motion document `{name}` in this session (list_motions); a library one is `asset:<id>:<revision>`"))),
    }
}

fn validate_motion(ctx: &mut Ctx, args: &Json) -> Outcome {
    slow(ctx, SlowJob::ValidateMotion { source: args.get("source").and_then(Json::as_str).unwrap_or("").to_string() }, Then::Reply, false)
}

fn motion_name(args: &Json) -> Result<String, ToolError> {
    let name = text(args, "name").unwrap_or("");
    if name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(ToolError::invalid(format!("`name` is a short id of letters, digits, `_` or `-` (got `{name}`)")));
    }
    Ok(name.to_string())
}

fn publish_motion_call(ctx: &mut Ctx, name: String, title: Option<String>, source: String) -> Outcome {
    let previous = ctx.host.state.motions.get(&name).map(|(asset, title, _)| (*asset, title.clone()));
    let title = title.or_else(|| previous.as_ref().map(|(_, title)| title.clone())).unwrap_or_else(|| name.replace('_', " "));
    let job = SlowJob::PublishMotion { title: title.clone(), source: source.clone(), asset: previous.as_ref().map(|(asset, _)| asset.asset_id) };
    slow(ctx, job, Then::Motion { name, title, source, previous: previous.map(|(asset, _)| asset) }, false)
}

fn write_motion(ctx: &mut Ctx, args: &Json) -> Outcome {
    let name = match motion_name(args) {
        Ok(name) => name,
        Err(error) => return fail(error),
    };
    let source = args.get("source").and_then(Json::as_str).unwrap_or("").to_string();
    publish_motion_call(ctx, name, text(args, "title").map(str::to_string), source)
}

fn patch_motion(ctx: &mut Ctx, args: &Json) -> Outcome {
    let name = match motion_name(args) {
        Ok(name) => name,
        Err(error) => return fail(error),
    };
    let Some((_, _, source)) = ctx.host.state.motions.get(&name) else {
        return fail(ToolError::not_found(format!("no Motion document `{name}` in this session: write_motion it first (list_motions)")));
    };
    let find = args.get("find").and_then(Json::as_str).unwrap_or("");
    let count = source.matches(find).count();
    if count != 1 {
        return fail(ToolError::refused(if count == 0 { "`find` does not occur in the document: read_motion and copy the passage exactly".to_string() } else { format!("`find` occurs {count} times: include more text so it is unique") }));
    }
    let patched = source.replacen(find, args.get("replace").and_then(Json::as_str).unwrap_or(""), 1);
    publish_motion_call(ctx, name, None, patched)
}

// ----------------------------------------------------------------- palette

/// A colour as one literal says it (`#ff4d12`, `color(4.0, 1.2, 0.3)`).
fn colour_of(text: &str) -> Option<[f64; 4]> {
    let found = makepad_motion::palette::of(text);
    (found.len() == 1 && found[0].sites.len() == 1 && text[found[0].sites[0].clone()].trim() == text.trim()).then(|| found[0].rgba)
}

/// A session Motion document's source, by `file` (its short name).
fn palette_file<'a>(ctx: &'a Ctx, args: &Json) -> Result<(String, &'a str), ToolError> {
    let name = text(args, "file").unwrap_or("").to_string();
    match ctx.host.state.motions.get(&name) {
        Some((_, _, source)) => Ok((name, source.as_str())),
        None => Err(ToolError::not_found(format!("no Motion document `{name}` in this session: write_motion it first (list_motions)"))),
    }
}

fn palette(ctx: &mut Ctx, args: &Json) -> Outcome {
    let (name, source) = match palette_file(ctx, args) {
        Ok(found) => found,
        Err(error) => return fail(error),
    };
    let line = |at: usize| Json::Int(source[..at].matches('\n').count() as i64 + 1);
    let swatches = makepad_motion::palette::of(source).into_iter().map(|sw| {
        let mut fields = vec![
            ("color", json::s(source[sw.sites[0].clone()].trim())),
            ("rgba", Json::Arr(sw.rgba.iter().map(|&v| Json::F64(v)).collect())),
            ("hdr", Json::Bool(sw.hdr)),
            ("group", Json::Int(sw.group as i64)),
            ("lines", Json::Arr(sw.sites.iter().map(|site| line(site.start)).collect())),
        ];
        if let Some(n) = &sw.name {
            fields.push(("name", json::s(n.clone())));
        }
        json::obj(fields)
    }).collect();
    done(json::obj(vec![("file", json::s(name)), ("swatches", Json::Arr(swatches))]))
}

fn set_palette(ctx: &mut Ctx, args: &Json) -> Outcome {
    let (name, source) = match palette_file(ctx, args) {
        Ok((name, source)) => (name, source.to_string()),
        Err(error) => return fail(error),
    };
    let Some(Json::Obj(pairs)) = args.get("map") else {
        return fail(ToolError::invalid("`map` is an object: each colour (as written, or a swatch's name) to its new colour"));
    };
    let swatches = makepad_motion::palette::of(&source);
    let mut map = Vec::new();
    for (from, to) in pairs {
        let Some(old) = swatches.iter().find(|sw| sw.name.as_deref() == Some(from.as_str())).map(|sw| sw.rgba).or_else(|| colour_of(from)) else {
            return fail(ToolError::invalid(format!("`{from}` is not a colour (`#rrggbb`, `#rrggbbaa`, `color(r, g, b[, a])`) or a swatch's name")));
        };
        if !swatches.iter().any(|sw| sw.rgba.iter().zip(&old).all(|(a, b)| (a - b).abs() < 1e-6)) {
            return fail(ToolError::not_found(format!("`{from}` is not written in `{name}` (palette lists its colours)")));
        }
        let Some(new) = to.as_str().and_then(colour_of) else {
            return fail(ToolError::invalid(format!("the new colour for `{from}` is not a colour (`#rrggbb`, `#rrggbbaa`, `color(r, g, b[, a])`)")));
        };
        map.push((old, new));
    }
    let recoloured = makepad_motion::palette::set(&source, &map);
    if recoloured == source {
        return fail(ToolError::refused("nothing changed: those are its colours already"));
    }
    publish_motion_call(ctx, name, None, recoloured)
}

// ------------------------------------------------------------ import_media

// ------------------------------------------------------------ feedback (M2)

fn issue_text(issue: &makepad_story_edit_tools::Issue) -> String {
    let at = if issue.line > 0 { format!("{}:{} ", issue.line, issue.col) } else { String::new() };
    let node = issue.node.as_ref().map(|node| format!("`{node}`: ")).unwrap_or_default();
    let hint = issue.hint.as_ref().map(|hint| format!(" — {hint}")).unwrap_or_default();
    format!("{at}{node}{} [{}]{hint}", issue.message, issue.code)
}

/// M2's inspect of a cut, resolving media through the library.
/// The layout checks of every Motion plate the deck's slides use, loaded
/// in the tree (its imports by path): what a person sees as wrong on a slide
/// (text over text, outside the frame or the safe area, too small), with the
/// file, line and the fix, so the deck chat can act on inspect.
fn inspect_tree_plates(tree: &Arc<makepad_asset_data::tree::TreeDraft>, deck: &str, errors: &mut Vec<Json>, warnings: &mut Vec<Json>) {
    let Ok(Json::Arr(rows)) = tree.text(deck).map_err(|_| ()).and_then(|source| api::document::outline(source).map_err(|_| ())) else { return };
    let mut plates: Vec<String> = Vec::new();
    for row in &rows {
        for graphic in row.get("graphics").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(Json::as_str) {
            let path = if tree.files.contains_key(graphic) { graphic.to_string() } else { format!("{graphic}.splash") };
            if !plates.contains(&path) && tree.files.get(&path).is_some_and(|file| file.kind == AssetKind::Motion) { plates.push(path); }
        }
    }
    for path in plates {
        let Ok(source) = tree.text(&path) else { continue };
        let context = makepad_motion::MotionContext { modules: makepad_motion::modules::Modules::from_tree(tree.context(&path)), ..Default::default() };
        let doc = match makepad_motion::MotionDoc::load_with(source, &[], context) {
            Ok(doc) => doc,
            Err(diags) => {
                errors.extend(diags.iter().map(|diag| json::s(format!("{path}:{diag}"))));
                continue;
            }
        };
        let places: HashMap<String, (u32, u32)> = doc.all_layers().iter().map(|layer| (layer.path.clone(), (layer.loc.line, layer.loc.col))).collect();
        let times = makepad_motion::inspect::sample_times(&doc, 10.0);
        for issue in makepad_motion::inspect::layout_checks(&doc, &times, makepad_motion::inspect::DEFAULT_MIN_TEXT_PX) {
            let (line, col) = places.get(&issue.path).copied().unwrap_or((0, 0));
            let other = issue.other.as_ref().map(|other| format!(" (with `{other}`)")).unwrap_or_default();
            warnings.push(json::s(format!("{path}:{line}:{col}: warning[{}] `{}`{other} {:.2}-{:.2} s: {} — fix: {}", issue.code, issue.path, issue.from, issue.to, issue.message, issue.hint)));
        }
    }
}

fn inspect_edit(source: &str, name: &str, motions: &[(String, String, bool)]) -> Json {
    use makepad_story_edit_tools::{inspect_text, InspectOptions, Severity};
    let mut resolver = super::store::LazyStoreMedia::new();
    let report = inspect_text(source, name, Some(&mut resolver), &InspectOptions::default());
    let pick = |severity: Severity| Json::Arr(report.issues.iter().filter(|issue| issue.severity == severity).map(|issue| json::s(issue_text(issue))).collect());
    // The cut's own checks do not look inside its Motion documents: their
    // errors and layout warnings (safe area, overflow, overlap) count here,
    // so a clean edit means clean documents too.
    let mut nested_errors = Vec::new();
    let mut nested_warnings = Vec::new();
    for (motion, motion_source, played) in motions {
        let nested = inspect_text(motion_source, &format!("{motion}.splash"), Some(&mut resolver), &InspectOptions::default());
        for issue in &nested.issues {
            let line = json::s(format!("in Motion `{motion}`: {}", issue_text(issue)));
            match issue.severity {
                // A document this cut does not play (a nested cut's, or a
                // draft) holds the finish back only by its layout warnings.
                Severity::Error if *played => nested_errors.push(line),
                Severity::Warning if ["safe_zone", "text.overflow", "layer.outside_frame", "text.final_clipped", "text.final_outside_safe", "text.overlap"].contains(&issue.code.as_str()) => nested_warnings.push(line),
                _ => {}
            }
        }
    }
    let with = |list: Json, extra: Vec<Json>| match list {
        Json::Arr(mut items) => {
            items.extend(extra);
            Json::Arr(items)
        }
        other => other,
    };
    let clean = report.ok() && nested_errors.is_empty();
    json::obj(vec![
        ("errors", with(pick(Severity::Error), nested_errors)),
        ("warnings", with(pick(Severity::Warning), nested_warnings)),
        ("info", pick(Severity::Info)),
        ("clean", Json::Bool(clean)),
    ])
}

fn audio_analyze_edit(source: &str, tree: Option<makepad_story_cut::tree::TreeContext>) -> Result<Json, ToolError> {
    use makepad_story_edit_tools::{analyze, AudioOptions, Document};
    let mut doc = Document::from_source(source, tree.as_ref().map_or("cut.splash", |tree| tree.document.as_str())).map_err(ToolError::refused)?;
    doc.tree = tree;
    let mut resolver = super::store::LazyStoreMedia::new();
    let report = analyze(&doc, &mut resolver, &HashMap::new(), &AudioOptions::default()).map_err(ToolError::failed)?;
    Ok(report.to_json())
}

fn audio_analyze(ctx: &mut Ctx, _: &Json) -> Outcome {
    match ctx.app.video_cut.session.as_ref() {
        Some(session) => {
            let source = session.source.clone();
            let tree = session.tree.as_ref().map(|tree| tree.context(&session.path));
            slow(ctx, SlowJob::AudioAnalyze { source, tree }, Then::Reply, true)
        }
        None => fail(no_edit()),
    }
}

/// The document whose frames a picture tool draws: the cut, or a Motion
/// document by the name it was written under (M2's tools draw it alone).
fn picture_source(ctx: &Ctx, args: &Json) -> Result<(String, Option<(Arc<makepad_asset_data::tree::TreeDraft>, String)>), ToolError> {
    match text(args, "target").unwrap_or("cut") {
        "cut" | "@cut" => ctx.app.video_cut.session.as_ref().map(|session| (session.source.clone(), session.tree.clone().map(|tree| (tree, session.path.clone())))).ok_or_else(no_edit),
        other => ctx.host.state.motions.get(other).map(|(_, _, source)| (source.clone(), None)).ok_or_else(|| {
            let mut names: Vec<&str> = ctx.host.state.motions.keys().map(String::as_str).collect();
            names.sort();
            ToolError::not_found(format!("no motion document `{other}`; this session wrote: {}", if names.is_empty() { "none".to_string() } else { names.join(", ") }))
        }),
    }
}

fn grab_call(ctx: &mut Ctx, args: &Json, command: Grab) -> Outcome {
    match picture_source(ctx, args) {
        Ok((source, tree)) => {
            // The open folder cut's files are read in its folder.
            let folder = tree.as_ref().and(ctx.app.video_cut.folder.as_ref()).map(|folder| folder.doc.dir.clone());
            slow(ctx, SlowJob::Grab { source, tree, folder, command }, Then::Reply, true)
        }
        Err(error) => fail(error),
    }
}

fn frame(ctx: &mut Ctx, args: &Json) -> Outcome {
    let width = args.get("width").and_then(Json::as_u64).unwrap_or(640).clamp(16, 1024) as u32;
    grab_call(ctx, args, Grab::Frame { at: number(args, "at").unwrap_or(0.0), width })
}

fn strip(ctx: &mut Ctx, args: &Json) -> Outcome {
    let (from, to) = (number(args, "from"), number(args, "to"));
    if let (Some(from), Some(to)) = (from, to) {
        if to <= from {
            return fail(ToolError::invalid(format!("`to` ({to}) must be after `from` ({from})")));
        }
    }
    grab_call(ctx, args, Grab::Strip { count: args.get("count").and_then(Json::as_u64).unwrap_or(12).max(1) as u32, from, to })
}

fn onion(ctx: &mut Ctx, args: &Json) -> Outcome {
    let (from, to) = (number(args, "from").unwrap_or(0.0), number(args, "to").unwrap_or(0.0));
    if to <= from {
        return fail(ToolError::invalid(format!("`to` ({to}) must be after `from` ({from})")));
    }
    grab_call(ctx, args, Grab::Onion { from, to, steps: args.get("steps").and_then(Json::as_u64).unwrap_or(6).max(2) as u32 })
}

/// How to start `makepad-edit-tools`, and its name for errors: STAGE_EDIT_TOOLS when it names a
/// file; under the JIT the loader runs it from this checkout (`$RELAX_LOADER run`, as the app
/// registry starts a JIT app), so it is the same source as this Story; natively the one next to
/// Stage. Never a binary found in a build folder: that can be another revision's build.
fn cut_tools_command() -> Result<(std::process::Command, String), ToolError> {
    let name = if cfg!(windows) { "makepad-edit-tools.exe" } else { "makepad-edit-tools" };
    if let Some(path) = std::env::var_os("STAGE_EDIT_TOOLS").map(std::path::PathBuf::from).filter(|path| path.is_file()) {
        return Ok((std::process::Command::new(&path), path.display().to_string()));
    }
    if let Some(loader) = std::env::var_os("RELAX_LOADER").map(std::path::PathBuf::from).filter(|path| path.is_file()) {
        let mut command = std::process::Command::new(&loader);
        // No one attends the tool: a compile error or a panic ends it, with its text on stderr.
        command.args(["run", "--unattended", "--quiet", "makepad-story-edit-tools", "--bin", "makepad-edit-tools"]);
        if !cfg!(debug_assertions) {
            command.arg("--release");
        }
        command.arg("--").current_dir(env!("CARGO_MANIFEST_DIR"));
        return Ok((command, format!("{name} under {}", loader.display())));
    }
    let beside = std::env::current_exe().map(|exe| exe.with_file_name(name)).map_err(|error| ToolError::failed(format!("this program's path: {error}")))?;
    if beside.is_file() {
        return Ok((std::process::Command::new(&beside), beside.display().to_string()));
    }
    Err(ToolError::new(ErrorKind::NotAvailable, format!(
        "the picture tools are not here: no {name} at {}, and STAGE_EDIT_TOOLS names none (build it: cargo build --release -p makepad-story-edit-tools)",
        beside.display()
    )))
}

/// The cut's render job in `dir` (`render.job`): the cut and every file it
/// reads, where they lie (its folder, the store's cache); what is nowhere
/// yet (an unsaved edit) is written beside it.
fn cut_job(client: &mut AssetClient, source: &str, tree: Option<&(Arc<makepad_asset_data::tree::TreeDraft>, String)>, folder: Option<&std::path::Path>, dir: &std::path::Path) -> Result<std::path::PathBuf, ToolError> {
    use makepad_story_edit_render::job::Job;
    let mut store = makepad_story_edit_render::StoreMedia::new(client.lane_clone());
    let job = match tree {
        Some((tree, path)) => Job::from_tree(Some(&mut store), tree, path, source, folder, dir),
        None => Job::from_cut(&mut store, None, source, dir),
    }.map_err(ToolError::failed)?;
    let listing = dir.join("render.job");
    job.write(&listing).map_err(ToolError::failed)?;
    Ok(listing)
}

/// Draw frames with `makepad-edit-tools` (its own hidden window on the
/// GPU, parent-death safe) and return its report and picture, no larger
/// than 1024 px a side.
fn grab(client: &mut AssetClient, source: &str, tree: Option<&(Arc<makepad_asset_data::tree::TreeDraft>, String)>, folder: Option<&std::path::Path>, command: &Grab) -> Result<(Json, Vec<u8>), ToolError> {
    let (mut tools, binary) = cut_tools_command()?;
    let dir = slow_dir("grab");
    // A Motion document is drawn alone (the tools wrap it in a cut); a cut
    // through its render job.
    let target: Vec<String> = if tree.is_none() && source.contains("Motion{") && !source.contains("@cut") {
        let file = dir.join("doc.motion.splash");
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&file, source)).map_err(|error| ToolError::failed(format!("{}: {error}", file.display())))?;
        vec![file.display().to_string()]
    } else {
        vec!["--job".into(), cut_job(client, source, tree, folder, &dir)?.display().to_string()]
    };
    let out = dir.join("out.png");
    let _ = std::fs::remove_file(&out);
    let mut args: Vec<String> = Vec::new();
    let label = match command {
        Grab::Frame { at, width } => {
            args.push("frame".into());
            args.extend(target.iter().cloned());
            args.extend(["--at".into(), at.to_string(), "--width".into(), width.to_string()]);
            format!("frame of the cut at {at:.2} s")
        }
        Grab::Strip { count, from, to } => {
            args.push("strip".into());
            args.extend(target.iter().cloned());
            args.extend(["--count".into(), count.to_string(), "--width".into(), "240".into()]);
            if let Some(from) = from {
                args.extend(["--from".into(), from.to_string()]);
            }
            if let Some(to) = to {
                args.extend(["--to".into(), to.to_string()]);
            }
            format!("strip of the cut, {count} frames")
        }
        Grab::Onion { from, to, steps } => {
            args.push("onion".into());
            args.extend(target.iter().cloned());
            args.extend(["--from".into(), from.to_string(), "--to".into(), to.to_string(), "--steps".into(), steps.to_string(), "--width".into(), "640".into()]);
            format!("motion trail of the cut, {from:.2}–{to:.2} s")
        }
    };
    args.extend(["--out".into(), out.display().to_string(), "--json".into()]);
    let output = tools.args(&args).env("MAKEPAD_HIDE_WINDOWS", "1").stdin(std::process::Stdio::null()).output()
        .map_err(|error| ToolError::failed(format!("{binary}: {error}")))?;
    // stdout is the report and nothing else: the tool's log lines go to stderr.
    let report = json::parse_depth(String::from_utf8_lossy(&output.stdout).trim().as_bytes(), 32).unwrap_or(Json::Null);
    if !output.status.success() || report.get("ok").and_then(Json::as_bool) != Some(true) {
        // Always a reason: the tool's own error, else its last warnings and
        // errors, else how it ended.
        let tail = String::from_utf8_lossy(&output.stderr).lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with("[I]")).rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / ");
        let error = report.get("error").and_then(Json::as_str).map(str::to_string).filter(|error| !error.trim().is_empty())
            .or_else(|| Some(tail).filter(|text| !text.is_empty()))
            .unwrap_or_else(|| format!("{binary} ended with {} and said nothing", output.status));
        return Err(ToolError::failed(format!("the picture was not drawn: {error}")));
    }
    let png = std::fs::read(&out).map_err(|error| ToolError::failed(format!("{}: {error}", out.display())))?;
    let png = fit_png(png, 1024)?;
    let mut fields = vec![("title", json::s(label))];
    if let Some(frames) = report.get("frames") {
        fields.push(("times", Json::Arr(frames.as_arr().unwrap_or(&[]).iter().filter_map(|frame| frame.get("t").cloned()).collect())));
    }
    // The picture's numbers (clipped and near-black shares, mean luma; a
    // run's worst frames), so a too-dark or blown-out frame shows without
    // looking.
    if let Some(exposure) = report.get("exposure").filter(|exposure| !exposure.is_null()) {
        fields.push(("exposure", exposure.clone()));
    }
    // Each 3D scene's nodes: share of the scene's picture and screen box,
    // so an object off screen, hidden or too small shows without looking.
    if let Some(coverage) = report.get("coverage") {
        fields.push(("coverage", coverage.clone()));
    }
    if let Ok(image) = decode_image_from_data(&png) {
        fields.push(("width", Json::Int(image.width as i64)));
        fields.push(("height", Json::Int(image.height as i64)));
    }
    Ok((json::obj(fields), png))
}

/// A PNG no larger than `edge` a side (area-averaged), for a model.
fn fit_png(png: Vec<u8>, edge: usize) -> Result<Vec<u8>, ToolError> {
    let image = decode_image_from_data(&png).map_err(|error| ToolError::failed(format!("the picture does not decode: {error:?}")))?;
    if image.width <= edge && image.height <= edge {
        return Ok(png);
    }
    let scale = edge as f64 / image.width.max(image.height) as f64;
    let (w, h) = (((image.width as f64 * scale).round() as usize).max(1), ((image.height as f64 * scale).round() as usize).max(1));
    let mut out = vec![0u32; w * h];
    for y in 0..h {
        let (y0, y1) = (y * image.height / h, ((y + 1) * image.height / h).max(y * image.height / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * image.width / w, ((x + 1) * image.width / w).max(x * image.width / w + 1));
            let mut sum = [0u64; 4];
            for sy in y0..y1.min(image.height) {
                for sx in x0..x1.min(image.width) {
                    let pixel = image.data[sy * image.width + sx];
                    for (channel, shift) in [24, 16, 8, 0].into_iter().enumerate() {
                        sum[channel] += ((pixel >> shift) & 0xff) as u64;
                    }
                }
            }
            let count = ((y1.min(image.height) - y0) * (x1.min(image.width) - x0)).max(1) as u64;
            out[y * w + x] = sum.iter().zip([24, 16, 8, 0]).fold(0u32, |acc, (value, shift)| acc | (((value / count) as u32) << shift));
        }
    }
    png_of(&out, w, h)
}

// ------------------------------------------------------------ app-run overlays (M4)

fn json_num(value: &Json) -> Option<f64> {
    match value {
        Json::Int(value) => Some(*value as f64),
        Json::F64(value) => Some(*value),
        _ => None,
    }
}

/// The run clip `clip` plays: its render, span, `in` and speed.
fn run_clip(ctx: &Ctx, clip: &str) -> Result<(AssetRevisionRef, (f64, f64, SourceTime, f64)), ToolError> {
    let doc = session_doc(ctx)?;
    node_exists(&doc, clip)?;
    let node = doc.node(clip).unwrap();
    let CutNodeKind::Clip { src: MediaRef::Run(run), source_in, speed, .. } = &node.kind else {
        let runs: Vec<String> = doc.nodes.iter().filter(|node| matches!(node.kind, CutNodeKind::Clip { src: MediaRef::Run(_), .. })).map(|node| node.id.clone()).collect();
        return Err(ToolError::invalid(format!("`{clip}` does not play an app run; the app-run clips are: {}", if runs.is_empty() { "none (add_clip kind app_run)".to_string() } else { runs.join(", ") })));
    };
    let span = node.span.ok_or_else(|| ToolError::invalid(format!("`{clip}` has no span")))?;
    let render = AssetRevisionRef {
        asset_id: run.render_id.parse().map_err(|_| ToolError::invalid("the clip's render id does not parse"))?,
        revision: run.render.parse().map_err(|_| ToolError::invalid("the clip's render revision does not parse"))?,
    };
    Ok((render, (span.start, span.length, source_in.clone(), *speed)))
}

/// A render's run.json as M4's run data.
fn fetch_run(client: &mut AssetClient, render: &AssetRevisionRef) -> Result<makepad_motion::bind::run::RunData, ToolError> {
    let manifest = client.resolve_ref(render).map_err(|error| ToolError::not_found(redact(error)))?;
    let file = manifest.files.iter().find(|file| file.role == FileRole::Source && file.media == MediaType::Text)
        .or_else(|| manifest.files.iter().find(|file| file.media == MediaType::Text || file.media == MediaType::Json))
        .ok_or_else(|| ToolError::not_found("that render has no run.json (is it an app run's render?)"))?;
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|error| ToolError::failed(redact(error)))?;
    makepad_motion::bind::run::RunData::parse(&bytes).map_err(|error| ToolError::failed(format!("run.json: {error}")))
}

fn run_facts(ctx: &mut Ctx, args: &Json) -> Outcome {
    match run_clip(ctx, text(args, "clip").unwrap_or("")) {
        Ok((render, _)) => slow(ctx, SlowJob::RunFacts { render }, Then::Reply, false),
        Err(error) => fail(error),
    }
}

fn bind_overlay(ctx: &mut Ctx, args: &Json) -> Outcome {
    let request = match bind_tools::request_from_json(args) {
        Ok(request) => request,
        Err(error) => return fail(ToolError::invalid(error)),
    };
    if bind_tools::overlay(&request.overlay).is_none() {
        return fail(ToolError::invalid(format!("no overlay `{}`; the overlays are: {}", request.overlay, bind_tools::OVERLAYS.iter().map(|overlay| overlay.id).collect::<Vec<_>>().join(", "))));
    }
    let (render, clip) = match run_clip(ctx, &request.clip) {
        Ok(found) => found,
        Err(error) => return fail(error),
    };
    let overlay = bind_tools::overlay(&request.overlay).unwrap().id.to_string();
    let overlay_asset = ctx.host.state.motions.get(&format!("overlay_{overlay}")).map(|(asset, _, _)| *asset);
    let clip_id = request.clip.clone();
    slow(ctx, SlowJob::BindOverlay { render, request, clip, overlay_asset }, Then::Bind { clip: clip_id, overlay }, false)
}

/// Place the overlay against the run (M4's `place`), and publish its
/// document once.
fn bind_overlay_job(client: &mut AssetClient, render: &AssetRevisionRef, request: &bind_tools::OverlayRequest, clip: (f64, f64, SourceTime, f64), overlay_asset: Option<AssetRevisionRef>) -> Result<Json, ToolError> {
    let run = fetch_run(client, render)?;
    let (start, length, source_in, speed) = clip;
    let source_in = match source_in {
        SourceTime::Seconds(seconds) => seconds,
        SourceTime::Marker { name, offset } => run.mark(&name).map(|at| at + offset).ok_or_else(|| ToolError::refused(format!("the clip starts at marker `{name}`, which the run does not have")))?,
    };
    let timing = makepad_motion::bind::when::ClipTiming { start, length, source_in, speed };
    let placement = bind_tools::place(request, &run, timing).map_err(ToolError::refused)?;
    let overlay = bind_tools::overlay(&request.overlay).ok_or_else(|| ToolError::invalid("unknown overlay"))?;
    let asset = match overlay_asset {
        Some(asset) => asset,
        None => {
            let published = publish_motion(client, &format!("Overlay · {}", overlay.id), overlay.source, None)?;
            reference(published.get("asset"), "asset").map_err(|_| ToolError::failed(motion_errors(&published)))?
        }
    };
    Ok(json::obj(vec![
        ("overlay_asset", reference_json(&asset)),
        ("spans", Json::Arr(placement.spans.iter().map(|(start, length)| Json::Arr(vec![num(*start), num(*length)])).collect())),
        ("params", Json::Arr(placement.params.iter().map(|(key, value)| Json::Arr(vec![json::s(key.clone()), json::s(value.clone())])).collect())),
        ("notes", Json::Arr(placement.notes.into_iter().map(json::s).collect())),
    ]))
}

/// The cut with one bound Graphic per span (M4's `graphic_splash`): the
/// nodes before the final `Flow{…}`, listed in it, and layered on top of
/// the picture stack the output shows.
fn add_graphics(patcher: &Patcher, source: &str, doc: &CutDoc, src: &str, clip: &str, spans: &[(f64, f64)], params: &[(String, String)]) -> Result<String, String> {
    use super::source::{constructor, field_text, final_flow, set_field};
    let stack = match doc.node(&doc.output_node).map(|node| &node.kind) {
        Some(CutNodeKind::Output { frame: Some(wire), .. }) if matches!(doc.node(&wire.node).map(|node| &node.kind), Some(CutNodeKind::Stack { .. })) => wire.node.clone(),
        _ => return Err("the cut's output does not show a picture Stack to put overlays on".into()),
    };
    let mut ids: Vec<String> = Vec::new();
    for _ in spans {
        let taken = |id: &str| ids.iter().any(|taken| taken == id);
        let id = (1..).map(|n| format!("overlay{n}")).find(|id| patcher.fresh(source, id) && !taken(id)).unwrap();
        ids.push(id);
    }
    let mut written = source.to_string();
    // Layers on top of the stack.
    let object = constructor(&written, &stack)?;
    let layers = field_text(&written, object, "layers").ok_or("the picture stack has no `layers`")?.trim().to_string();
    let inner = layers.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')).ok_or("the picture stack's `layers` is not a list")?.trim().to_string();
    // Entries are numbered `conn(n, …)`; plain entries count in order.
    let numbered: Vec<u64> = inner.match_indices("conn(").filter_map(|(at, _)| inner[at + 5..].split(',').next()?.trim().parse::<u64>().ok()).collect();
    let next = numbered.iter().copied().max().unwrap_or(0).max(inner.matches(".out(").count() as u64);
    let added: Vec<String> = ids.iter().enumerate().map(|(index, id)| format!("mod.flow.conn({}, mod.flow.wire(@{id}, @frame))", next + 1 + index as u64)).collect();
    let list = if inner.is_empty() { format!("[{}]", added.join(", ")) } else { format!("[{inner}, {}]", added.join(", ")) };
    written = set_field(&written, object, "layers", &list);
    // The nodes, before the final `Flow{`, and their ids in it.
    let (open, close) = final_flow(&written).ok_or("a cut ends with one `Flow{...}`")?;
    let close_line = written[..close].rfind('\n').map_or(close, |at| at + 1);
    written.insert_str(close_line, &format!("    {}\n", ids.join(", ")));
    let flow_line = written[..open].rfind('\n').map_or(0, |at| at + 1);
    // Each on the lowest video track above the clip that is free over its
    // span (graphics and blocks alike), so the timeline shows every one.
    let base = doc.node(clip).map_or(0, |node| node.track) + 1;
    let mut placed: Vec<(u32, f64, f64)> = doc.nodes.iter().filter(|node| ops::lane_of(node) == Some(ops::Lane::Video))
        .filter_map(|node| node.span.map(|span| (node.track, span.start, span.end())))
        .chain(doc.graphics.iter().filter_map(|graphic| graphic.span.map(|span| (graphic.track, span.start, span.end()))))
        .collect();
    let nodes: Vec<String> = ids.iter().zip(spans).map(|(id, span)| {
        let (start, end) = (span.0, span.0 + span.1);
        let track = (base..).find(|track| !placed.iter().any(|(busy, a, b)| busy == track && *a < end && start < *b)).unwrap();
        placed.push((track, start, end));
        let line = bind_tools::graphic_splash(id, src, clip, *span, params);
        match line.rfind('}') {
            Some(at) => format!("{} track: {track}}}", &line[..at]),
            None => line,
        }
    }).collect();
    written.insert_str(flow_line, &format!("{}\n\n", nodes.join("\n")));
    Ok(written)
}

#[cfg(test)]
mod tests {
    /// What this host implements, the table's tools and its own additions,
    /// makes a valid service for every target of Story's chat, and no tool
    /// it implements is out of the chat's reach: Story's own services carry
    /// every one but the generation tools (FLOW's), so the agent-tools guard
    /// (`tests/agent_tools.rs`) reaches them all through Story's `app`
    /// service.
    #[test]
    fn the_hosts_tools_make_valid_services_for_every_target() {
        use makepad_studio_api::services::{risk, targets_of, Product, TARGETS};
        let mut registry: Registry<Handler> = Registry::new();
        super::register(&mut registry);
        let specs: Vec<ToolSpec> = registry.tools().into_iter().filter(|(_, available)| *available).map(|(spec, _)| spec.clone()).collect();
        assert!(specs.len() > 150, "{} tools", specs.len());
        for target in targets_of(&[Product::Story]) {
            let manifest = target.manifest(specs.iter()).unwrap_or_else(|error| panic!("{}: {error}", target.id));
            assert!(manifest.tools.len() > 4, "{}", target.id);
        }
        for spec in &specs {
            assert!(TARGETS.iter().any(|target| target.has(spec)), "{} is in no target", spec.name);
            assert!(spec.name.starts_with("gen_") || targets_of(&[Product::Story]).any(|target| target.has(spec)), "{} is in none of Story's services", spec.name);
            if spec.request {
                assert_eq!(risk(spec), makepad_ai_services::wire::Risk::Request);
            }
        }
        // The chat's own: the guide, the note to the video maker.
        for name in ["authoring_guide", "make_video_note"] {
            assert!(specs.iter().any(|spec| spec.name == name), "{name}");
        }
    }

    use super::*;

    #[test]
    fn g1_tools_are_registered_with_valid_schemas() {
        let mut registry: Registry<Handler> = Registry::new();
        register(&mut registry);
        for name in ["generate_add", "generate_run", "generate_pick", "generate_cancel", "generate_status", "new_video", "new_video_status", "new_video_redo", "new_video_cancel", "make_video"] {
            assert!(api::find(name).is_some(), "{name} is in the catalogue");
            assert!(registry.available(name), "{name}");
        }
        let error = registry.check("generate_run", Json::Obj(Vec::new())).err().unwrap();
        assert!(error.message.contains("`node` is missing"), "{}", error.message);
    }

    const DECK: &str = include_str!("../../libs/deck/tests/fixtures/custom_deck.splash");

    fn job(kind: DocKind, base: &str, recipe: Recipe) -> Result<(String, OpenDoc), String> {
        let steps = recipe.steps(base).map_err(|error| error.message)?;
        run_edit(&mut Patcher::new(), CutJob { ticket: 1, kind, base: base.to_string(), steps }).result
    }

    #[test]
    fn the_source_tools_are_registered_under_their_kind_free_names() {
        let mut registry: Registry<Handler> = Registry::new();
        register(&mut registry);
        for name in ["tree_list", "doc_read", "doc_write", "doc_patch", "doc_create", "doc_rename", "doc_delete", "doc_history", "doc_undo", "inspect", "open_deck", "new_deck", "add_slide", "move_slide", "delete_slide", "duplicate_slide", "set_slide", "set_demo", "add_app", "add_layer", "set_deck"] {
            assert!(registry.available(name), "{name}");
        }
        for old in ["read_edit", "write_edit", "patch_edit"] {
            assert!(api::find(old).is_none() && !registry.available(old), "{old}");
        }
    }

    #[test]
    fn write_and_patch_change_a_presentation_through_the_same_job_as_an_edit() {
        // A patch of the deck's source: loaded, one slide's notes changed.
        let syntax = makepad_script::syntax::parse(DECK).unwrap();
        let mut edits = Vec::new();
        syntax.walk(&mut |_, node| {
            if node.string() == Some("Thank you.") { edits.push(api::document::Edit::at(node.span.clone(), node.text(DECK), "\"Thanks.\"")); }
        });
        let (source, doc) = job(DocKind::Deck, DECK, Recipe::Edits(edits)).unwrap();
        let OpenDoc::Deck(doc) = doc else { panic!("a deck") };
        let closing = doc.slides.iter().find(|slide| slide.name == "closing").expect("the closing slide");
        assert_eq!(closing.notes, "Thanks.");
        assert_eq!(source, DECK.replace("Thank you.", "Thanks."));
        // A write is the whole source.
        let (_, doc) = job(DocKind::Deck, DECK, Recipe::Write(source.clone())).unwrap();
        assert!(matches!(doc, OpenDoc::Deck(_)));
        // A source that does not load is refused with its diagnostics and the same
        // for an exact passage that is missing or not unique.
        let error = job(DocKind::Deck, DECK, Recipe::Write("use mod.deck.*\nDeck{ slides: [] }".into())).err().unwrap();
        assert!(error.contains("at least one slide"), "{error}");
        let error = job(DocKind::Deck, DECK, Recipe::Edits(vec![api::document::Edit::at(1..2, "x", "y")])).err().unwrap();
        assert!(error.contains("not a parsed node span"), "{error}");
        let error = job(DocKind::Deck, DECK, Recipe::Edits(vec![api::document::Edit::at(syntax.root().span.clone(), "stale source", DECK)])).err().unwrap();
        assert!(error.contains("changed"), "{error}");
        // The same tools on an edit.
        let edit = super::super::template(1080, 1920, "Doc");
        let syntax = makepad_script::syntax::parse(&edit).unwrap();
        let mut edits = Vec::new();
        syntax.walk(&mut |_, node| {
            if let Some(field) = node.field("duration") { edits.push(api::document::Edit::at(field.value.span.clone(), field.value.text(&edit), "12.0")); }
        });
        let (patched, doc) = job(DocKind::Cut, &edit, Recipe::Edits(edits)).unwrap();
        let OpenDoc::Cut(doc) = doc else { panic!("an edit") };
        assert_eq!(doc.duration, 12.0);
        assert!(patched.contains("duration: 12.0"));
        let error = job(DocKind::Cut, &edit, Recipe::Write(DECK.into())).err().unwrap();
        assert!(!error.is_empty());
        // An edit's op is not a step of a deck.
        let steps = vec![Step::Op(CutOp::Label("x".into()))];
        let done = run_edit(&mut Patcher::new(), CutJob { ticket: 2, kind: DocKind::Deck, base: DECK.into(), steps });
        assert!(done.result.err().unwrap().contains("not of a deck"));
    }

    #[test]
    fn slide_ops_and_the_cut_tools_on_one_slide_run_as_one_job() {
        use makepad_story_deck::ops::{NewCut, DeckOp};
        use makepad_story_deck::SlideCut;
        let run = |steps: Vec<Step>| run_edit(&mut Patcher::new(), CutJob { ticket: 1, kind: DocKind::Deck, base: DECK.into(), steps }).result;
        // A deck op.
        let (source, doc) = run(vec![Step::Deck(DeckOp::AddSlide { after: Some(0), name: "second".into(), cut: NewCut::Blank })]).unwrap();
        let OpenDoc::Deck(doc) = doc else { panic!() };
        assert_eq!(doc.slides[1].name, "second");
        assert!(source.contains("second_main"));
        // The cut's tools on slide 1 (the blank one just added): a title.
        let (source, doc) = run(vec![
            Step::Deck(DeckOp::AddSlide { after: Some(0), name: "second".into(), cut: NewCut::Blank }),
            Step::Slide { slide: 1, steps: vec![Step::Op(CutOp::AddTitle { start: 0.0, text: "Hello".into() })] },
        ]).unwrap();
        let OpenDoc::Deck(doc) = doc else { panic!() };
        let SlideCut::Inline(cut) = &doc.slides[1].cut else { panic!() };
        assert!(cut.nodes.iter().any(|node| matches!(&node.kind, CutNodeKind::Title { text, .. } if text == "Hello")), "{source}");
        // Other slides are as they were.
        assert_eq!(doc.slides[0].name, "intro");
        // add_app: declared and placed in one job (on a blank slide: the tour's
        // hand-written stacks have no entry ids for the patcher to rewire).
        let steps = vec![
            Step::Deck(DeckOp::AddSlide { after: Some(0), name: "second".into(), cut: NewCut::Blank }),
            Step::Deck(DeckOp::AddApp { index: 1, name: "calc".into(), app: "makepad-example-calculator".into(), size: (400.0, 600.0), audio: false }),
            Step::Slide { slide: 1, steps: vec![Step::Op(CutOp::AppLayer { app: "calc".into() })] },
        ];
        let (source, doc) = run(steps).unwrap();
        let OpenDoc::Deck(doc) = doc else { panic!() };
        assert!(doc.slides[1].apps.iter().any(|app| app.name == "calc"));
        let SlideCut::Inline(cut) = &doc.slides[1].cut else { panic!() };
        assert!(cut.nodes.iter().any(|node| matches!(&node.kind, CutNodeKind::AppView { app, .. } if app == "calc")), "{source}");
        // A slide with a library cut has no cut source to change.
        let library = r#"use mod.cut.*
use mod.deck.*
Deck{ title: "L" slides: [Slide{ name: @lib cut: cut("ast_yqzlg4xa4mbgpzs6e2qsuqwhsu", "arev_5ab3f998fd4a7b6860a9337b9924be40ad341d12276e341079ce2093539c715f") }] }
"#.to_string();
        let done = run_edit(&mut Patcher::new(), CutJob { ticket: 2, kind: DocKind::Deck, base: library, steps: vec![Step::Slide { slide: 0, steps: vec![Step::Op(CutOp::AddTitle { start: 0.0, text: "x".into() })] }] });
        assert!(done.result.err().unwrap().contains("library cut"));
    }

    const RUN: &str = r#"run("ast_fklvc3bvjnuijdg33d2uuitkbi", "arev_e2e0b2ddc168547400e68147bf9e0161866f9cad405c9f94a2a47a5dcfe955ac", "ast_rbzhbuglyvqk6nprgjwvl2o33q", "arev_925b7b93f54e7cfa717f5f213568db1fdaea73ef483fc3ebbad2ea042a533bd1")"#;
    const MOTION: &str = r#"asset("ast_t5sq36kux6pbpvymcyrt4xwgiq", "arev_5e0c24eba6b66516a043337f665507f77ab7c7ad3f56725adb5bb847c2f5e78a")"#;

    #[test]
    fn overlays_land_as_bound_graphics_on_top_of_the_picture() {
        let patcher = Patcher::new().unwrap();
        // The page's template with one app-run clip on it.
        let (source, _) = ops::apply(&patcher, &super::super::template(1080, 1920, "Bound"), &CutOp::Add {
            media: Media { kind: MediaKind::AppRun, src: match makepad_story_cut::CutDoc::load(&format!("use mod.flow.*\nuse mod.cut.*\nlet demo = Clip{{src: {RUN} span: [0.0, 5.0]}}\nlet out = CutOutput{{frame: demo.out(@frame)}}\nFlow{{kind: @cut label: \"x\" output: {{width: 1080 height: 1920 fps: 30}} duration: 10.0 demo, out}}")).unwrap().node("demo").unwrap().kind.clone() {
                CutNodeKind::Clip { src, .. } => src,
                _ => unreachable!(),
            }, seconds: Some(5.0) },
            start: 0.0,
            track: None,
        }).unwrap();
        let doc = CutDoc::load(&source).unwrap();
        let clip = doc.nodes.iter().find(|node| matches!(node.kind, CutNodeKind::Clip { .. })).unwrap().id.clone();
        let params = vec![("target".to_string(), "\"main.increment\"".to_string())];
        let written = add_graphics(&patcher, &source, &doc, MOTION, &clip, &[(1.0, 1.5), (3.0, 1.0)], &params).unwrap();
        let (written, after) = ops::apply(&patcher, &source, &CutOp::Source { text: written }).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(after.graphics.len(), 2, "{written}");
        assert!(after.graphics.iter().all(|graphic| graphic.bind.is_some()), "{written}");
        let spans: Vec<(f64, f64)> = after.graphics.iter().map(|graphic| graphic.span.map(|span| (span.start, span.length)).unwrap()).collect();
        assert_eq!(spans, vec![(1.0, 1.5), (3.0, 1.0)]);
        // On top of the stack the output shows, after the clip.
        let CutNodeKind::Stack { layers } = &after.node("main").unwrap().kind else { panic!("{written}") };
        let order: Vec<&str> = layers.iter().map(|(wire, _)| wire.node.as_str()).collect();
        assert_eq!(order, vec![clip.as_str(), "overlay1", "overlay2"], "{written}");
        assert!(written.contains("conn(2, mod.flow.wire(@overlay1") && written.contains("conn(3, mod.flow.wire(@overlay2"), "{written}");
        assert!(after.graphics.iter().all(|graphic| graphic.track == 1), "{written}");
    }
}

/// Take graphics out: first every input that reads their frame (the
/// picture stack's entries), then the nodes.
fn remove_graphics(patcher: &Patcher, source: &str, ids: &[String]) -> Result<String, String> {
    use makepad_flow::NodeInputValue;
    let unwired = patcher.rewire(source, |graph| {
        let mut next = graph.clone();
        for consumer in &graph.nodes {
            for input in &consumer.inputs {
                match &input.value {
                    NodeInputValue::Edge(edge) if ids.contains(&edge.from_node) => {
                        next = makepad_flow_ui::graph_edit::disconnect(&next, &consumer.id, &input.port, None);
                    }
                    NodeInputValue::Edges(entries) => {
                        for entry in entries.iter().filter(|entry| ids.contains(&entry.from.from_node)) {
                            next = makepad_flow_ui::graph_edit::disconnect(&next, &consumer.id, &input.port, Some(entry.id));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(next)
    })?;
    patcher.remove_nodes(&unwired, ids)
}

// ------------------------------------------------------------ generative segments (G1)

// G1's tools: its own handler; `generate_add` and `generate_run` answer
// with a request whose result arrives on a later pump.

/// The open graphic's palette published as a library module (`name`),
/// for palette_apply on other scenes.
fn palette_save(ctx: &mut Ctx, args: &Json) -> Outcome {
    let name = match motion_name(args) {
        Ok(name) => name,
        Err(error) => return fail(error),
    };
    let title = text(args, "title").unwrap_or(&name).to_string();
    match super::motion_edit::variants::palette_module(&ctx.app.video_cut, &title) {
        Ok(source) => publish_motion_call(ctx, name, Some(title), source),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn variants_tool(ctx: &mut Ctx, name: &str, args: &Json) -> Outcome {
    use super::motion_edit::variants::{self, Answer};
    match variants::tool(&mut ctx.app.video_cut, ctx.cx, name, args) {
        Answer::Done(Ok(value)) => done(value),
        Answer::Done(Err(error)) => fail(ToolError::refused(error)),
        Answer::Later(pending) => Outcome::Wait(Wait::Variants(pending)),
    }
}

fn cut_panel_tool(ctx: &mut Ctx, name: &str, args: &Json) -> Outcome {
    match super::cut_panel::tool(&mut ctx.app.video_cut, ctx.cx, name, args) {
        Ok(value) => done(value),
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn storyboard_tool(ctx: &mut Ctx, name: &str, args: &Json) -> Outcome {
    match super::storyboard::tool(ctx.app, ctx.cx, name, args, ctx.origin) {
        super::storyboard::Answer::Done(result) => Outcome::Done(result),
        super::storyboard::Answer::Later(request) => Outcome::Wait(Wait::Storyboard { request }),
    }
}

fn generate_tool(ctx: &mut Ctx, name: &str, args: &Json) -> Outcome {
    let before = ctx.app.video_cut.session.as_ref().map(|session| (session.revision, session.source.clone()));
    if name != "generate_status" {
        note_author(&mut ctx.app.video_cut, ctx.origin, name);
    }
    match ctx.app.generate_tool_call(ctx.cx, name, args) {
        Ok(value) => match value.get("request").and_then(Json::as_u64) {
            Some(request) => Outcome::Wait(Wait::Generate { request, before, answer: None }),
            None => {
                // `generate_pick` changes the cut through the page's own op;
                // the change log records it once it lands.
                done(value)
            }
        },
        Err(error) => fail(ToolError::refused(error)),
    }
}

fn new_video_tool(ctx: &mut Ctx, name: &str, args: &Json) -> Outcome {
    match ctx.app.new_video_tool_call(name, args) {
        Ok(value) => done(value),
        Err(error) => fail(ToolError::refused(error)),
    }
}

#[cfg(test)]
mod sketch_tree_tests {
    use super::*;
    use makepad_asset_data::tree::{TreeDraft, TreeRole};
    use makepad_story_deck::ops::{DeckOp, NewCut, SlideSet};

    const DECK: &str = "use mod.flow.*\nuse mod.cut.*\nuse mod.deck.*\n\nDeck{\n    title: \"Sketches\"\n    look: \"shared/deck_look\"\n    slides: [\n        Slide{\n            name: @open\n            graphic: \"slides/open_slide\"\n        }\n    ]\n}\n";
    const PLAIN: &str = "use mod.motion.*\nMotion{size: vec2(1920, 1080) duration: 3.0 layers: [Rect{size: vec2(10, 10) fill: #ff0000}]}\n";

    fn tree() -> Arc<TreeDraft> {
        let mut tree = TreeDraft::new("sketches".into(), TreeRole::Deck, "deck.splash".into(), AssetKind::Deck, DECK.into()).unwrap();
        tree.write("slides/open_slide.splash", AssetKind::Motion, PLAIN.as_bytes().into()).unwrap();
        tree.write("shared/deck_look.splash", AssetKind::Motion, makepad_story_deck::sketch::NEUTRAL_LOOK.as_bytes().into()).unwrap();
        Arc::new(tree)
    }

    /// One job, as the page and the chat's tools run it: one change of the tree.
    fn job(tree: &Arc<TreeDraft>, steps: Vec<Step>) -> (Arc<TreeDraft>, Arc<makepad_story_deck::DeckDoc>) {
        let mut all = vec![Step::Tree { tree: tree.clone(), path: "deck.splash".into(), deck: None }];
        all.extend(steps);
        let done = run_edit(&mut Patcher::new(), CutJob { ticket: 1, kind: DocKind::Deck, base: String::new(), steps: all });
        match done.result.unwrap_or_else(|error| panic!("{error}")).1 {
            OpenDoc::Tree { doc, tree, .. } => (tree, doc.into_deck().unwrap()),
            _ => panic!("a deck tree"),
        }
    }

    fn changed(before: &TreeDraft, after: &TreeDraft) -> Vec<String> {
        after.files.iter().filter(|(path, file)| before.files.get(*path).is_none_or(|old| old.bytes != file.bytes)).map(|(path, _)| path.clone()).collect()
    }

    #[test]
    fn a_sketch_slide_is_added_typed_and_given_its_graphic_one_tree_change_each() {
        let start = tree();
        // "New slide": an empty sketch after slide 0.
        let (added, doc) = job(&start, vec![Step::Deck(DeckOp::AddSlide { after: Some(0), name: "slide1".into(), cut: NewCut::Sketch { text: String::new() } })]);
        assert_eq!(changed(&start, &added), ["deck.splash"]);
        assert!(doc.slides[1].sketch_only());
        // The words typed on it.
        let words = "Why sketch first\n- Type the words\n- Then fancy it up";
        let (typed, doc) = job(&added, vec![Step::Deck(DeckOp::SetSlide { index: 1, set: SlideSet { sketch: Some(words.into()), ..SlideSet::default() } })]);
        assert_eq!(changed(&added, &typed), ["deck.splash"]);
        assert_eq!(doc.slides[1].sketch.as_deref(), Some(words));
        // Fancy it up's result: the graphic file, then attached; the sketch stays.
        let (written, _) = job(&typed, vec![Step::Document(super::Change::Create { path: "slides/slide1_slide.splash".into(), from: None, text: Some(PLAIN.into()) })]);
        assert_eq!(changed(&typed, &written), ["slides/slide1_slide.splash"]);
        let (attached, doc) = job(&written, vec![Step::Deck(DeckOp::SetSlide { index: 1, set: SlideSet { graphic: Some(Some("slides/slide1_slide.splash".into())), ..SlideSet::default() } })]);
        assert_eq!(changed(&written, &attached), ["deck.splash"]);
        assert_eq!(doc.slides[1].graphic.as_deref(), Some("slides/slide1_slide.splash"));
        assert_eq!(doc.slides[1].sketch.as_deref(), Some(words));
        assert_eq!(doc.look.as_deref(), Some("shared/deck_look.splash"));
    }

    /// The code view's write of a slide file, as the page queues it: on the open deck.
    fn write_slide(tree: &Arc<TreeDraft>, deck: &Arc<makepad_story_deck::DeckDoc>, text: &str) -> Result<(String, OpenDoc), String> {
        let change = super::Change::Write { path: "slides/open_slide.splash".into(), text: text.into(), expected: Some(PLAIN.into()) };
        let steps = vec![Step::Tree { tree: tree.clone(), path: "deck.splash".into(), deck: Some(deck.clone()) }, Step::Document(change)];
        run_edit(&mut Patcher::new(), CutJob { ticket: 1, kind: DocKind::Deck, base: String::new(), steps }).result
    }

    #[test]
    fn a_code_view_edit_of_a_slide_file_reaches_the_slides_document() {
        let start = tree();
        let deck = super::super::super::document_tree::load_deck(&start).unwrap();
        let typed = PLAIN.replace("#ff0000", "#00ff00");
        let (tree, doc) = match write_slide(&start, &deck, &typed).unwrap_or_else(|error| panic!("{error}")).1 {
            OpenDoc::Tree { doc, tree, .. } => (tree, doc.into_deck().unwrap()),
            _ => panic!("a deck tree"),
        };
        assert_eq!(tree.text("slides/open_slide.splash").unwrap(), typed);
        // The slide's document is the typed text, resolved in the new tree:
        // what the player loads (its one motion document).
        let makepad_story_deck::SlideCut::Inline(cut) = &doc.slides[0].cut else { panic!("the slide is a cut") };
        assert_eq!(cut.graphics.len(), 1);
        let makepad_story_cut::GraphicSrc::Tree(context) = &cut.graphics[0].src else { panic!("a tree graphic") };
        assert_eq!(context.resolve(&context.document).unwrap().text().unwrap(), typed);
        assert!(!Arc::ptr_eq(&doc.graphics["slides/open_slide.splash"].1, &deck.graphics["slides/open_slide.splash"].1));
    }

    #[test]
    fn a_code_view_edit_that_does_not_parse_is_refused_and_named_on_its_line() {
        let start = tree();
        let deck = super::super::super::document_tree::load_deck(&start).unwrap();
        // Line 2's object is never closed.
        let broken = "use mod.motion.*\nMotion{size: vec2(1920, 1080) duration: 3.0 layers: [Rect{size: vec2(10, 10) fill: #ff0000}]\n";
        assert!(write_slide(&start, &deck, broken).is_err(), "a broken slide does not land");
        // The tree, and so the picture, stays the last that loaded.
        assert_eq!(start.text("slides/open_slide.splash").unwrap(), PLAIN);
        let problems = super::super::super::cut::motion_edit::checker::diagnose(&start, "slides/open_slide.splash", broken);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].line, 2, "{problems:?}");
        // One that parses but does not evaluate: on its own line too.
        let wrong = "use mod.motion.*\nMotion{size: vec2(1920, 1080) duration: 3.0\n    layers: [Text{text: \"hi\" align: @nowhere}]}\n";
        assert!(write_slide(&start, &deck, wrong).is_err());
        let problems = super::super::super::cut::motion_edit::checker::diagnose(&start, "slides/open_slide.splash", wrong);
        assert!(problems.iter().any(|problem| problem.line == 3), "{problems:?}");
    }
}
