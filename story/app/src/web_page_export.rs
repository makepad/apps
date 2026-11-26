//! File > Export > Web page…: a Story document (a film's cut, a deck) as one web
//! page, by the wasm-export library (apps/apps/tools/wasm-export): `index.html`, a loader
//! and ONE wasm with everything embedded; the song is the only file beside them.
//!
//! This is the document driver of that export: makepad-story-doc-player in deterministic mode plays the
//! document at three window sizes (one with the script heap's stress mode) for the coverage
//! runs, and a held-out replay at a fourth size, in stress mode and with another seek order,
//! gates the result. The export runs on a worker thread whose traces are child processes; the
//! UI only drains the worker's events on its pump, and Cancel stops the runs.
//!
//! The dialog: the document, the output folder, Export / Cancel, the phase and its progress,
//! the size report by part when it is done, and Open folder. The result is never opened in a
//! browser from here.

use crate::cut::folder_docs::{self, Card};
use crate::StoryApp;
use makepad_wasm_export::report::Report;
use makepad_wasm_export::{Driver, Failure, Options, Phase, Progress};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

enum Event {
    Phase(Phase, f64, String),
    Note(String),
    Done(Result<Report, Failure>),
}

struct ChannelProgress {
    events: Sender<Event>,
    cancel: Arc<AtomicBool>,
}

impl Progress for ChannelProgress {
    fn phase(&mut self, phase: Phase, done: f64, note: &str) {
        let _ = self.events.send(Event::Phase(phase, done, note.to_string()));
        makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
    }

    fn note(&mut self, _phase: Phase, text: &str) {
        let _ = self.events.send(Event::Note(text.to_string()));
        makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

struct Running {
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct WebPageExport {
    /// The documents the dialog offers (cuts, then decks).
    cards: Vec<&'static Card>,
    chosen: usize,
    running: Option<Running>,
    /// Where the last export wrote (Open folder).
    written: Option<PathBuf>,
    notes: Vec<String>,
}

/// The repository Story's source is in (`apps/<repo>` of the Makepad checkout; the trace host
/// compiles makepad-story-doc-player from it).
fn story_repo_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    dir.canonicalize().unwrap_or(dir)
}

/// The Makepad checkout around it.
fn source_root() -> PathBuf {
    let dir = story_repo_dir().join("../..");
    dir.canonicalize().unwrap_or(dir)
}

fn default_out_dir(title: &str) -> String {
    let name: String = title.trim().chars().map(|c| if matches!(c, '/' | '\\' | ':' | '\0') { '-' } else { c }).collect();
    let name = if name.trim().is_empty() { "Story".to_string() } else { name.trim().to_string() };
    format!("~/Movies/{name} page")
}

fn expand(path: &str) -> PathBuf {
    match path.trim().strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(path.trim()),
    }
}

/// The song a card's folder plays: (its logical path, the file), when it has one on disk.
fn song_of(card: &Card) -> Option<(String, PathBuf)> {
    let logical = card.doc.song_path()?.to_string();
    let in_folder = card.doc.dir.join(&logical);
    if in_folder.is_file() {
        return Some((logical, in_folder));
    }
    let pin = card.doc.pin.as_ref().filter(|pin| pin.path == logical && pin.installed())?;
    Some((logical, makepad_model_install::weights_path(pin.model)))
}

/// The raster images in the document's own folder, as the URLs its runs fetch them by under
/// `prefix`: the exporting author's images, which the export may embed when the document
/// references them (JPEGs are never allowed).
fn document_images(dir: &Path, prefix: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), prefix.to_string())];
    while let Some((dir, url)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let (Some(name), Ok(kind)) = (entry.file_name().to_str().map(str::to_string), entry.file_type()) else { continue };
            let url = format!("{url}/{name}");
            if kind.is_dir() {
                pending.push((entry.path(), url));
            } else if matches!(name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref(), Some("png" | "gif" | "webp")) {
                urls.push(url);
            }
        }
    }
    urls.sort();
    urls
}

/// The export of `card` into `out_dir`: the document driver's runs.
pub(crate) fn options_for(card: &Card, out_dir: &Path) -> Result<Options, String> {
    let folder = card.doc.dir.file_name().and_then(|n| n.to_str()).ok_or("the document's folder has no name")?.to_string();
    let doc = format!("{folder}/{}", card.path);
    let mut serves = vec![(format!("/{folder}"), card.doc.dir.clone())];
    let fonts = makepad_doc_fonts::weights_root().join("fonts");
    if fonts.is_dir() {
        serves.push(("/fonts".into(), fonts));
    }
    // The song stays beside the page: the runs fetch it as `song/<file>`, the page as `<file>`.
    let mut traced_fill = String::new();
    let mut shipped_fill = String::new();
    if let Some((logical, file)) = song_of(card) {
        let name = file.file_name().and_then(|n| n.to_str()).ok_or("the song has no file name")?.to_string();
        let dir = file.parent().ok_or("the song has no folder")?.to_path_buf();
        serves.push(("/song".into(), dir));
        traced_fill = format!("&fill={logical}=song/{name}");
        shipped_fill = format!("&fill={logical}={name}");
    }
    let base = format!("?doc={doc}&deterministic&end=exit&shutter-max=4{traced_fill}");
    let until = Some("makepad-story-doc-player: exit".to_string());
    let run = |name: &str, window: &str, stress: bool, extra: &str| Driver {
        name: name.into(),
        window: window.into(),
        query: format!("{base}{extra}"),
        until: until.clone(),
        stress,
        ..Default::default()
    };
    Ok(Options {
        package: "makepad-story-doc-player".into(),
        package_dir: story_repo_dir(),
        source_root: source_root(),
        // built from the checkout and found as Makepad's launchers find their tools, the
        // intermediates in its target dir (makepad_wasm_export::tools)
        trace_host: None,
        out_dir: out_dir.to_path_buf(),
        build_dir: None,
        serves,
        external: vec!["song/".into()],
        allow_images: document_images(&card.doc.dir, &format!("/{folder}")),
        credits: None,
        drivers: vec![run("a1", "1280x720@1", false, ""), run("a2", "800x450@2", false, ""), run("s1", "1920x1080@1", true, "")],
        // another size, stress, and the timeline seeked out of order
        replays: vec![run("b1", "1600x900@1.5", true, "&seek=80%,20%,60%,5%,95%,40%")],
        query: format!("doc={doc}{shipped_fill}"),
        title: card.title.clone(),
        name: "player".into(),
        jobs: 2,
        ship_args: vec!["--no-splash-records".into()],
        js_stripped: makepad_web_pack::js::PLAYBACK_JS_STRIPPED.iter().map(|s| s.to_string()).collect(),
        allow_missing_node: false,
        block_shaking: false,
        environment_variations: true,
        seek_input_class_variations: true,
    })
}

impl WebPageExport {
    fn start(&mut self, cx: &mut Cx, options: Options) -> Result<(), String> {
        let (events, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut progress = ChannelProgress { events: events.clone(), cancel: cancel.clone() };
        let thread_options = ThreadOptions { name: Some(Arc::from("story-web-page")), stack_size: Some(8 * 1024 * 1024), priority: CxThreadPriority::Utility };
        let handle = cx
            .thread_spawner()
            .spawn_worker(thread_options, move || {
                let result = makepad_wasm_export::export(&options, &mut progress);
                let _ = events.send(Event::Done(result));
                makepad_widgets::makepad_platform::thread::SignalToUI::set_ui_signal();
            })
            .map_err(|error| format!("{error:?}"))?;
        handle.detach();
        self.running = Some(Running { events: rx, cancel });
        self.notes.clear();
        Ok(())
    }
}

fn report_text(report: &Report) -> String {
    let kb = |n: usize| format!("{:.0} KB", n as f64 / 1024.0);
    let mut out = format!("player.wasm {} ({} brotli 11)\n", kb(report.wasm_raw), kb(report.wasm_br));
    for part in &report.parts {
        if part.raw == 0 {
            continue;
        }
        let br = part.br.map(|b| format!(", {} br", kb(b))).unwrap_or_default();
        out.push_str(&format!("  {}: {}{br}\n", part.name, kb(part.raw)));
    }
    for (name, size) in &report.external {
        out.push_str(&format!("beside the page: {name} ({})\n", kb(*size as usize)));
    }
    out.push_str(&format!("gate: {}", report.gate));
    out
}

/// `makepad-story --export-web <folder>/<file> [--out <dir>]`: the dialog's export of one folder
/// document through package, without a window, into `--out` or `<target dir>/wasm-export/e2e-page`.
/// The CI runs it on a real talk on every pass (story/app/ci.toml `runs`). Exit status: 0 written,
/// 1 the export failed (the message names the phase), 2 usage.
pub(crate) fn export_web_main(args: &[String]) -> Option<std::process::ExitCode> {
    use std::process::ExitCode;

    if args.first().map(String::as_str) != Some("--export-web") {
        return None;
    }
    let (name, out) = match &args[1..] {
        [name] => (name, None),
        [name, flag, dir] if flag == "--out" => (name, Some(PathBuf::from(dir))),
        _ => {
            eprintln!("usage: makepad-story --export-web <folder>/<file> [--out <dir>]");
            return Some(ExitCode::from(2));
        }
    };
    let Some(card) = folder_docs::cut_cards().iter().chain(folder_docs::deck_cards())
        .find(|card| format!("{}/{}", card.doc.name, card.path) == *name) else {
        eprintln!("no folder document {name} in Story's content roots");
        return Some(ExitCode::FAILURE);
    };
    let out = out.unwrap_or_else(|| makepad_wasm_export::tools::target_dir(&source_root()).join("wasm-export").join("e2e-page"));
    let options = match options_for(card, &out) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("story: {error}");
            return Some(ExitCode::FAILURE);
        }
    };
    let report = match makepad_wasm_export::export_until(&options, makepad_wasm_export::Phase::Package, &mut makepad_wasm_export::StderrProgress) {
        Ok(Some(report)) => report,
        Ok(None) => {
            eprintln!("story: an export through package reported no page");
            return Some(ExitCode::FAILURE);
        }
        Err(failure) => {
            eprintln!("story: the web export failed: {failure}");
            return Some(ExitCode::FAILURE);
        }
    };
    for file in ["index.html", "player.js", "player.wasm"] {
        if !out.join(file).is_file() {
            eprintln!("story: no {file} in {}", out.display());
            return Some(ExitCode::FAILURE);
        }
    }
    match std::fs::read(out.join("player.wasm")) {
        Ok(bytes) if bytes.starts_with(b"\0asm") => {}
        Ok(_) => {
            eprintln!("story: player.wasm in {} has no wasm header", out.display());
            return Some(ExitCode::FAILURE);
        }
        Err(error) => {
            eprintln!("story: cannot read player.wasm in {}: {error}", out.display());
            return Some(ExitCode::FAILURE);
        }
    }
    println!("{}", report_text(&report));
    println!("story: web page written to {}", out.display());
    Some(ExitCode::SUCCESS)
}

impl StoryApp {
    /// File > Export > Web page…: the dialog, opened with the documents.
    pub(crate) fn open_web_page_export(&mut self, cx: &mut Cx) {
        let export = &mut self.web_page;
        if export.cards.is_empty() {
            export.cards = folder_docs::cut_cards().iter().chain(folder_docs::deck_cards()).collect();
        }
        let labels: Vec<String> = if export.cards.is_empty() {
            vec!["(no documents)".into()]
        } else {
            export.cards.iter().map(|c| if c.is_deck() { format!("{} (deck)", c.title) } else { c.title.clone() }).collect()
        };
        let chosen = export.chosen.min(export.cards.len().saturating_sub(1));
        self.ui.drop_down(cx, ids!(wp_doc)).set_labels(cx, labels);
        self.ui.drop_down(cx, ids!(wp_doc)).set_selected_item(cx, chosen);
        if let Some(card) = export.cards.get(chosen) {
            if self.ui.text_input(cx, ids!(wp_out)).text().trim().is_empty() {
                self.ui.text_input(cx, ids!(wp_out)).set_text(cx, &default_out_dir(&card.title));
            }
        }
        self.sync_web_page_export(cx);
        self.ui.modal(cx, ids!(web_page_dialog)).open(cx);
    }

    pub(crate) fn handle_web_page_export(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(index) = self.ui.drop_down(cx, ids!(wp_doc)).selected(actions) {
            self.web_page.chosen = index;
            if let Some(card) = self.web_page.cards.get(index) {
                self.ui.text_input(cx, ids!(wp_out)).set_text(cx, &default_out_dir(&card.title));
            }
        }
        if self.ui.button(cx, ids!(wp_start)).clicked(actions) && self.web_page.running.is_none() {
            let out = expand(&self.ui.text_input(cx, ids!(wp_out)).text());
            let started = match self.web_page.cards.get(self.web_page.chosen) {
                None => Err("no document to export".to_string()),
                Some(_) if out.as_os_str().is_empty() => Err("choose an output folder".to_string()),
                Some(card) => options_for(card, &out).and_then(|options| self.web_page.start(cx, options)),
            };
            match started {
                Ok(()) => {
                    self.web_page.written = None;
                    self.ui.label(cx, ids!(wp_report)).set_text(cx, "");
                    self.ui.label(cx, ids!(wp_status)).set_text(cx, "Starting…");
                }
                Err(error) => self.ui.label(cx, ids!(wp_status)).set_text(cx, &error),
            }
            self.sync_web_page_export(cx);
        }
        if self.ui.button(cx, ids!(wp_close)).clicked(actions) {
            match &self.web_page.running {
                // Cancel: the worker stops the runs and reports `cancelled`.
                Some(running) => {
                    running.cancel.store(true, Ordering::Release);
                    self.ui.label(cx, ids!(wp_status)).set_text(cx, "Stopping…");
                }
                None => self.ui.modal(cx, ids!(web_page_dialog)).close(cx),
            }
        }
        if self.ui.button(cx, ids!(wp_open)).clicked(actions) {
            if let Some(dir) = &self.web_page.written {
                cx.open_url(&dir.to_string_lossy(), OpenUrlInPlace::No);
            }
        }
    }

    /// Drains the worker's events (the pump).
    pub(crate) fn pump_web_page_export(&mut self, cx: &mut Cx) {
        let Some(running) = &self.web_page.running else { return };
        let mut status = None;
        let mut done = None;
        while let Ok(event) = running.events.try_recv() {
            match event {
                Event::Phase(phase, fraction, note) => {
                    let note: String = note.chars().take(140).collect();
                    status = Some(format!("{} — {:.0}%\n{note}", phase.title(), fraction * 100.0));
                }
                Event::Note(text) => self.web_page.notes.push(text),
                Event::Done(result) => done = Some(result),
            }
        }
        if let Some(status) = status {
            self.ui.label(cx, ids!(wp_status)).set_text(cx, &status);
        }
        if let Some(result) = done {
            self.web_page.running = None;
            match result {
                Ok(report) => {
                    self.web_page.written = Some(PathBuf::from(&report.out_dir));
                    self.ui.label(cx, ids!(wp_status)).set_text(cx, &format!("Written to {}", report.out_dir));
                    let mut text = report_text(&report);
                    for note in &self.web_page.notes {
                        text.push_str(&format!("\nnote: {note}"));
                    }
                    self.ui.label(cx, ids!(wp_report)).set_text(cx, &text);
                }
                Err(failure) => {
                    self.ui.label(cx, ids!(wp_status)).set_text(cx, &format!("Export failed ({}): {}", failure.phase.name(), failure.message));
                    let details = failure.details.map(|d| format!("details: {}", d.display())).unwrap_or_default();
                    self.ui.label(cx, ids!(wp_report)).set_text(cx, &details);
                }
            }
            self.sync_web_page_export(cx);
        }
    }

    pub(crate) fn web_page_export_running(&self) -> bool {
        self.web_page.running.is_some()
    }

    fn sync_web_page_export(&mut self, cx: &mut Cx) {
        let running = self.web_page.running.is_some();
        self.ui.button(cx, ids!(wp_start)).set_enabled(cx, !running && !self.web_page.cards.is_empty());
        self.ui.button(cx, ids!(wp_close)).set_text(cx, if running { "Cancel" } else { "Close" });
        self.ui.button(cx, ids!(wp_open)).set_visible(cx, self.web_page.written.is_some());
        self.ui.redraw(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_dirs_are_safe_names() {
        assert_eq!(default_out_dir("Harbour (night): the film"), "~/Movies/Harbour (night)- the film page");
        assert_eq!(default_out_dir("  "), "~/Movies/Story page");
    }

    /// The dialog's export options for a folder document (`options_for`), on a small deck made
    /// for it (`tests/export-fixture`): the runs fetch the document from its folder, served under
    /// the folder's name; the folder's own raster images may be embedded; training has a stress
    /// run (the held-out replays are stress runs, and the gate needs the stress code trained);
    /// each held-out replay plays at a size no training run has and seeks out of order; the
    /// shipped page's query names the document without the runs' flags. The export itself, a
    /// real talk through package, is the CI run of `makepad-story --export-web` ([`export_web_main`], named in story/app/ci.toml `runs`).
    #[test]
    fn options_serve_the_folder_and_hold_out_a_stress_replay() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/export-fixture");
        let doc: &'static folder_docs::FolderDoc = Box::leak(Box::new(folder_docs::FolderDoc::at(&dir, false).unwrap().expect("the fixture holds a deck")));
        let card = Card { doc, path: "deck.splash".into(), title: "Web export fixture".into() };
        let options = options_for(&card, Path::new("page")).unwrap();
        assert_eq!(options.serves[0], ("/export-fixture".to_string(), doc.dir.clone()), "the folder is served under its name");
        assert_eq!(options.allow_images, ["/export-fixture/media/story_icon.png"], "the folder's own images");
        assert_eq!(options.query, "doc=export-fixture/deck.splash", "the shipped page's query");
        for run in options.drivers.iter().chain(&options.replays) {
            assert!(run.query.starts_with("?doc=export-fixture/deck.splash&deterministic&end=exit"), "{}: {}", run.name, run.query);
            assert_eq!(run.until.as_deref(), Some("makepad-story-doc-player: exit"), "{} ends at the player's exit", run.name);
        }
        assert!(options.drivers.iter().any(|driver| driver.stress), "training has a stress run");
        assert!(!options.replays.is_empty(), "the gate has a held-out replay");
        for replay in &options.replays {
            assert!(replay.stress, "{} is a stress run", replay.name);
            assert!(options.drivers.iter().all(|driver| driver.window != replay.window), "{} plays at a held-out size", replay.name);
            assert!(replay.query.contains("&seek="), "{} seeks out of order", replay.name);
        }
    }
}
