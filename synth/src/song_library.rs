//! The Song library: the installed MIDI song library and chip collections
//! (`makepad_midi_library`, opt-in downloads from Resources) searched by
//! decade, genre, tag, artist or title, and a song opened as a score: Open
//! imports it at once (a loop per part, its form as sections, a mix and
//! master chain); Arrange has the chat's model rewrite that import as an
//! idiomatic score (minutes, on a worker; checked against the song). Game
//! music is found online and downloaded only on the person's own pick. And a
//! person's own music files: Open a music file… and a file dropped on the
//! window go through the same importers (MIDI, and the tracker and chip
//! formats: MOD, S3M, XM, IT, SID, NSF, SPC, VGM, GBS). The library's tools
//! for the chat are here too.

use crate::desk::run::Answer;
use crate::desk::Desk;
use crate::library::Document;
use crate::writer::AiWriter;
use makepad_midi_library::chip::zophar;
use makepad_midi_library::import::{self, About, Options};
use makepad_midi_library::{arranger, index, tools};
use makepad_music_iter::gen::TurnMonitor;
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::makepad_platform::file_dialogs::{FileDialog, FileDialogAction};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use makepad_score_live::PrepareControl;
use std::sync::mpsc::{channel, Receiver};

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let stage = mod.stage
    let Body = Label{width: Fill height: Fit text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 10}}}
    let Action = StageButton{width: Fit height: 22 padding: Inset{left: 8 right: 8}}
    // One song: its name, what it is, and its actions.
    let SongRow = View{visible: false width: Fill height: Fit flow: Right spacing: 8 padding: Inset{top: 4 bottom: 4} align: Align{y: 0.5}
        View{width: Fill height: Fit flow: Down spacing: 2
            name := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.text_bright text_style: theme.font_bold{font_size: 10}}}
            meta := Label{width: Fill height: Fit text: "" draw_text +: {color: stage.dim text_style: theme.font_regular{font_size: 9}}}
        }
        Tip{text: "Open it as a score now: a loop per part, its form as sections, a mix and master chain" open := Action{text: "Open"}}
        Tip{text: "Have the chat's model arrange it as a readable score (takes minutes; checked against the song)" arrange := Action{text: "Arrange"}}
    }
    mod.widgets.SynthSongLibraryPanel = Modal{
        content +: {width: 660 height: Fit flow: Down spacing: 10 padding: 18 show_bg: true
            draw_bg +: {
                color: stage.panel
                pixel: fn(){
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0., 0., self.rect_size.x, self.rect_size.y, 8.)
                    sdf.fill(vec4(self.color.xyz, 1.))
                    return sdf.result
                }
            }
            View{width: Fill height: Fit flow: Right spacing: 8
                Label{width: Fill text: "Song library" draw_text +: {color: stage.text_bright text_style: theme.font_bold{font_size: 14}}}
                songs_close := StageButton{width: 56 height: 24 text: "Close"}
            }
            View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                RoundedView{width: Fill height: 24 show_bg: true
                    draw_bg +: {color: stage.well border_color: stage.line border_size: 1.0 border_radius: 2.5}
                    songs_search := TextInput{width: Fill height: 24 margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} empty_text: "Search: 80s synthpop, c64 sid, new wave, a title…"
                        draw_bg +: {color: #0000 color_hover: #0000 color_focus: #0000 border_size: 0.0}
                        draw_text +: {text_style: theme.font_regular{font_size: 10}}}}
                Tip{text: "Find a game's music online (NES, SNES, Game Boy, Mega Drive): the game you pick is downloaded with its credits, a track per row" songs_online := Action{text: "Find game music"}}
            }
            View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                songs_status := Body{}
                songs_stop := Action{visible: false text: "Stop"}
            }
            ScrollYView{width: Fill height: 460 flow: Down spacing: 2
                song_0 := SongRow{} song_1 := SongRow{} song_2 := SongRow{} song_3 := SongRow{}
                song_4 := SongRow{} song_5 := SongRow{} song_6 := SongRow{} song_7 := SongRow{}
                song_8 := SongRow{} song_9 := SongRow{} song_10 := SongRow{} song_11 := SongRow{}
                song_12 := SongRow{} song_13 := SongRow{} song_14 := SongRow{} song_15 := SongRow{}
            }
            View{width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
                Body{width: Fill text: "The songs are their owners' works: study them, play them, make covers for yourself; each score keeps its credit."}
                songs_resources := StageButton{width: Fit height: 24 padding: Inset{left: 10 right: 10} text: "Resources…"}
            }
        }
    }
}

const ROWS: [LiveId; 16] = [
    live_id!(song_0), live_id!(song_1), live_id!(song_2), live_id!(song_3), live_id!(song_4), live_id!(song_5), live_id!(song_6), live_id!(song_7),
    live_id!(song_8), live_id!(song_9), live_id!(song_10), live_id!(song_11), live_id!(song_12), live_id!(song_13), live_id!(song_14), live_id!(song_15),
];

/// The open-music-file dialog's id.
const DIALOG: LiveId = live_id!(synth_open_music_file);

/// Files the importers read: MIDI, and the tracker and chip formats.
pub fn is_music_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("mid" | "midi" | "smf" | "kar"))
        || makepad_midi_library::chip::import::is_supported(&path.to_string_lossy())
}

/// A person's music file as a score: (title, source). The one dispatch
/// point by format.
pub fn import_file(path: &Path) -> Result<(String, String), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let title = path.file_stem().map_or("Song".to_string(), |s| s.to_string_lossy().replace('_', " "));
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mid" | "midi" | "smf" | "kar") => {
            let about = About { title: title.clone(), path: path.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string()), ..About::default() };
            let imported = import::import(&bytes, &about, &Options::default())?;
            Ok((title, imported.source))
        }
        // Tracker and chip music: its samples are made playable here.
        _ if makepad_midi_library::chip::import::is_supported(&path.to_string_lossy()) => {
            use makepad_midi_library::chip::import as chip;
            let imported = chip::import(&bytes, &path.to_string_lossy(), &chip::About::default())?;
            chip::register(&imported);
            Ok((imported.arrangement.title.clone(), imported.source))
        }
        _ => Err(format!("{} is not a file Synth opens as a score (MIDI, MOD, S3M, XM, IT, SID, NSF, SPC, VGM, GBS)", path.display())),
    }
}

/// A library song imported (MIDI, or chip and tracker music with its
/// samples made playable): its arrangement and score.
fn open_entry(entry: &index::Entry) -> Result<(makepad_midi_library::arrangement::Arrangement, String), String> {
    tools::arrangement(entry)
}

type SongJob<T> = FrameJob<Result<T, String>>;

fn take_result<T>(job: &mut SongJob<T>) -> Option<Result<T, String>> {
    job.try_take().map(|done| done.map_err(|error| error.to_string()).and_then(|result| result))
}

enum OpenMusic {
    File(PathBuf),
    Library(index::Entry),
}

struct OpenedSong {
    title: String,
    source: String,
    file: Option<PathBuf>,
}

impl OpenMusic {
    fn decode(self) -> Result<OpenedSong, String> {
        match self {
            Self::File(path) => {
                let (title, source) = import_file(&path)?;
                Ok(OpenedSong { title, source, file: Some(path) })
            }
            Self::Library(entry) => {
                let title = entry.label();
                let (_, source) = open_entry(&entry).map_err(|error| format!("{title}: {error}"))?;
                Ok(OpenedSong { title, source, file: None })
            }
        }
    }
}

struct Arranging {
    title: String,
    job: SongJob<(String, String)>,
    progress: Receiver<String>,
    cancel: PrepareControl,
    monitor: TurnMonitor,
}

impl Drop for Arranging {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.job.cancel();
    }
}

/// A search of the online catalogue, or the picked game downloading.
enum Online {
    Searching(SongJob<Vec<zophar::Game>>),
    Downloading { name: String, job: SongJob<Vec<index::Entry>> },
}

#[derive(Default)]
pub struct SongLibrary {
    /// Games found online, shown in the rows instead of the library's songs.
    games: Option<Vec<zophar::Game>>,
    online: Option<Online>,
    entries: Option<Vec<index::Entry>>,
    query: String,
    /// The entries behind the visible rows. Opening one needs no catalogue scan.
    shown: Vec<index::Entry>,
    opening: Option<SongJob<OpenedSong>>,
    open_frame: NextFrame,
    status: String,
    arranging: Option<Arranging>,
    timer: Timer,
}

impl SongLibrary {
    fn load(&mut self) -> bool {
        if self.entries.is_none() {
            self.entries = index::load(&index::index_path()).ok();
        }
        self.entries.is_some()
    }

    /// The catalogue changed (Resources built or removed it): read again.
    pub fn reload(&mut self) {
        self.entries = None;
    }

    fn search(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        if !self.load() {
            self.shown.clear();
            self.status = "The song library is not installed: install it from Resources (it is an opt-in download, its notice and licences shown first).".into();
        } else if let Some(entries) = &self.entries {
            let hits = if self.query.trim().is_empty() { index::search(entries, "quality:90", ROWS.len()) } else { index::search(entries, &self.query, ROWS.len()) };
            self.games = None;
            if self.arranging.is_none() {
                self.status = if self.query.trim().is_empty() {
                    format!("{} songs. Some of the fullest arrangements:", entries.len())
                } else {
                    format!("{} matches for “{}” of {} songs", hits.len(), self.query.trim(), entries.len())
                };
            }
            self.shown = hits.into_iter().map(|(entry, _)| entry.clone()).collect();
            Self::fill(cx, ui, &self.shown);
        }
        if self.shown.is_empty() {
            for id in ROWS {
                ui.view(cx, &[id]).set_visible(cx, false);
            }
        }
        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        ui.redraw(cx);
    }

    /// Songs in the rows.
    fn fill(cx: &mut Cx, ui: &WidgetRef, rows: &[index::Entry]) {
        for (row, id) in ROWS.iter().copied().enumerate() {
            let Some(e) = rows.get(row) else {
                ui.view(cx, &[id]).set_visible(cx, false);
                continue;
            };
            let f = &e.features;
            let decade = e.decade().map_or(String::new(), |d| format!("{:02}s · ", d % 100));
            let genre = e.genres.first().map_or(String::new(), |g| format!("{} · ", g.trim_start_matches('~')));
            ui.view(cx, &[id]).set_visible(cx, true);
            ui.label(cx, &[id, live_id!(name)]).set_text(cx, &e.label());
            ui.label(cx, &[id, live_id!(meta)]).set_text(cx, &format!("{decade}{genre}{} bpm · {} · {}:{:02} · {} parts · quality {}", f.bpm, f.key, f.seconds as u64 / 60, f.seconds as u64 % 60, f.parts, f.quality));
        }
    }

    /// Games found online in the rows.
    fn show_games(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let games = self.games.clone().unwrap_or_default();
        for (row, id) in ROWS.iter().copied().enumerate() {
            let Some(g) = games.get(row) else {
                ui.view(cx, &[id]).set_visible(cx, false);
                continue;
            };
            ui.view(cx, &[id]).set_visible(cx, true);
            ui.label(cx, &[id, live_id!(name)]).set_text(cx, &g.name);
            ui.label(cx, &[id, live_id!(meta)]).set_text(cx, &format!("{} · {} · {} · Open downloads the game's music", g.console, g.year, g.developer));
        }
        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        ui.redraw(cx);
    }

    /// A downloaded game's tracks in the rows.
    fn show_tracks(&mut self, cx: &mut Cx, ui: &WidgetRef, tracks: &[index::Entry]) {
        self.entries = None;
        self.games = None;
        self.shown = tracks.iter().take(ROWS.len()).cloned().collect();
        Self::fill(cx, ui, &self.shown);
        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        ui.redraw(cx);
    }

    pub fn open(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.load();
        self.search(cx, ui);
        ui.modal(cx, ids!(song_library_panel)).open(cx);
    }

    fn start_open(&mut self, cx: &mut Cx, choice: OpenMusic) -> Result<(), String> {
        if let Some(old) = self.opening.take() {
            old.cancel();
        }
        let job = cx.task_pool().submit_frame_job(Class::Visible, move || choice.decode())
            .map_err(|error| error.to_string())?;
        self.opening = Some(job);
        self.open_frame = cx.new_next_frame();
        Ok(())
    }

    fn take_open(&mut self, cx: &mut Cx, event: &Event) -> Option<Result<OpenedSong, String>> {
        self.open_frame.is_event(event)?;
        let done = take_result(self.opening.as_mut()?);
        if done.is_some() {
            self.opening = None;
        } else {
            self.open_frame = cx.new_next_frame();
        }
        done
    }

    fn run(&mut self, cx: &mut Cx) {
        if self.timer.is_empty() {
            self.timer = cx.start_interval(0.5);
        }
    }

    /// The panel's and the file opener's actions; `resources` is asked to
    /// open when the person wants the downloads.
    pub fn handle_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk, actions: &Actions) -> bool {
        let mut resources = false;
        if ui.button(cx, ids!(score_songs)).clicked(actions) {
            self.open(cx, ui);
        }
        if ui.button(cx, ids!(score_open_music)).clicked(actions) {
            // MIDI, and the tracker and chip formats the same importer reads.
            let mut extensions: Vec<String> = ["mid", "midi"].iter().map(|e| e.to_string()).collect();
            extensions.extend(makepad_midi_library::chip::import::EXTENSIONS.iter().map(|e| e.to_string()));
            cx.open_select_file_dialog(FileDialog::new().set_id(DIALOG).set_title("Open a music file as a score".to_string())
                .add_filter("Music files (MIDI, MOD, S3M, XM, IT, SID, NSF, SPC, VGM, GBS)".to_string(), extensions));
        }
        for action in actions {
            if let Some(FileDialogAction::FileSelected { id, paths }) = action.downcast_ref::<FileDialogAction>() {
                if *id == DIALOG {
                    if let Some(path) = paths.first() {
                        self.open_music(cx, ui, desk, OpenMusic::File(path.clone()));
                    }
                }
            }
        }
        if ui.button(cx, ids!(songs_close)).clicked(actions) {
            ui.modal(cx, ids!(song_library_panel)).close(cx);
        }
        if ui.button(cx, ids!(songs_stop)).clicked(actions) && self.arranging.is_some() {
            self.arranging = None;
            self.status = "Arranging stopped.".into();
            ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
            ui.button(cx, ids!(songs_stop)).set_visible(cx, false);
        }
        if ui.button(cx, ids!(songs_resources)).clicked(actions) {
            ui.modal(cx, ids!(song_library_panel)).close(cx);
            resources = true;
        }
        if let Some(text) = ui.text_input(cx, ids!(songs_search)).changed(actions) {
            self.query = text;
            self.search(cx, ui);
        }
        // Game music, found and downloaded only on the person's own clicks.
        if ui.button(cx, ids!(songs_online)).clicked(actions) && !matches!(self.online, Some(Online::Downloading { .. })) {
            let words = self.query.trim().to_string();
            if words.len() < 2 {
                self.status = "Type a game's name to find its music (Chrono Trigger, Mega Man 2…).".into();
            } else {
                if let Some(Online::Searching(old)) = self.online.take() {
                    old.cancel();
                }
                match cx.task_pool().submit_frame_job(Class::Background, move || zophar::search(&words)) {
                    Ok(job) => {
                        self.online = Some(Online::Searching(job));
                        self.status = format!("Searching for the music of “{}”…", self.query.trim());
                        self.run(cx);
                    }
                    Err(error) => self.status = format!("The search did not start: {error}"),
                }
            }
            ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        }
        for (row, id) in ROWS.iter().copied().enumerate() {
            let open = ui.button(cx, &[id, live_id!(open)]).clicked(actions);
            let arrange = ui.button(cx, &[id, live_id!(arrange)]).clicked(actions);
            if let (Some(games), true) = (&self.games, open || arrange) {
                if self.online.is_some() {
                    continue;
                }
                let Some(game) = games.get(row).cloned() else { continue };
                let name = game.name.clone();
                match cx.task_pool().submit_frame_job(Class::Background, move || zophar::download(&game)) {
                    Ok(job) => {
                        self.status = format!("Downloading {name}'s music… {}", zophar::NOTICE);
                        self.online = Some(Online::Downloading { name, job });
                        self.run(cx);
                    }
                    Err(error) => self.status = format!("{name} was not downloaded: {error}"),
                }
                ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
                continue;
            }
            if open {
                let Some(entry) = self.shown.get(row).cloned() else { continue };
                self.open_music(cx, ui, desk, OpenMusic::Library(entry));
            }
            if arrange {
                let Some(entry) = self.shown.get(row).cloned() else { continue };
                self.start_arranging(cx, ui, entry);
            }
        }
        resources
    }

    pub fn handle_event(&mut self, cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk, event: &Event) {
        // A music file dragged onto the window opens as a score.
        match event {
            Event::Drag(drag) => {
                if drag.items.iter().any(|item| matches!(item, DragItem::FilePath { path, .. } if is_music_file(Path::new(path)))) {
                    if let Ok(mut response) = drag.response.lock() {
                        *response = DragResponse::Copy;
                    }
                }
            }
            Event::Drop(dropped) => {
                let file = dropped.items.iter().find_map(|item| match item {
                    DragItem::FilePath { path, .. } if is_music_file(Path::new(path)) => Some(PathBuf::from(path)),
                    _ => None,
                });
                if let Some(path) = file {
                    self.open_music(cx, ui, desk, OpenMusic::File(path));
                }
            }
            _ => {}
        }
        if let Some(result) = self.take_open(cx, event) {
            match result {
                Ok(song) => {
                    if song.file.is_none() {
                        ui.modal(cx, ids!(song_library_panel)).close(cx);
                    }
                    open_imported(cx, ui, desk, &song.title, song.source);
                }
                Err(error) => self.open_failed(cx, ui, desk, error),
            }
        }
        if self.timer.is_event(event).is_some() {
            self.pump_online(cx, ui);
            self.pump_arranging(cx, ui, desk);
        }
    }

    fn open_music(&mut self, cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk, choice: OpenMusic) {
        if let Err(error) = self.start_open(cx, choice) {
            self.open_failed(cx, ui, desk, error);
        }
    }

    fn open_failed(&mut self, cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk, error: String) {
        self.status = format!("Not opened: {error}");
        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        desk.music_open_failed(error);
    }

    fn start_arranging(&mut self, cx: &mut Cx, ui: &WidgetRef, entry: index::Entry) {
        self.arranging = None;
        let (say, progress) = channel();
        let title = entry.label();
        let writer = AiWriter::default();
        let monitor = writer.monitor.clone();
        let cancel = PrepareControl::default();
        let flag = cancel.clone();
        let job = match cx.task_pool().submit_frame_job(Class::Background, move || {
            let (arrangement, source) = open_entry(&entry)?;
            let arranged = arranger::arrange(&writer, &arrangement, &source, &flag, &|p| {
                let _ = say.send(p.to_string());
            })?;
            Ok((arranged.similarity.line(), arranged.source))
        }) {
            Ok(job) => job,
            Err(error) => {
                self.status = format!("{title} was not arranged: {error}");
                ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
                ui.button(cx, ids!(songs_stop)).set_visible(cx, false);
                return;
            }
        };
        self.status = format!("Arranging {title}…");
        self.arranging = Some(Arranging { title, job, progress, cancel, monitor });
        self.run(cx);
        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
        ui.button(cx, ids!(songs_stop)).set_visible(cx, true);
    }

    /// The game search's results, or the picked game's tracks.
    fn pump_online(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        match self.online.take() {
            Some(Online::Searching(mut job)) => match take_result(&mut job) {
                None => self.online = Some(Online::Searching(job)),
                Some(result) => {
                    match result {
                        Ok(games) => {
                            self.status = format!("{} games with music Synth plays. {}", games.len(), zophar::NOTICE);
                            self.games = Some(games.into_iter().take(ROWS.len()).collect());
                        }
                        Err(error) => {
                            self.status = format!("The search did not answer: {error}");
                            self.games = Some(Vec::new());
                        }
                    }
                    self.show_games(cx, ui);
                }
            },
            Some(Online::Downloading { name, mut job }) => match take_result(&mut job) {
                None => self.online = Some(Online::Downloading { name, job }),
                Some(result) => match result {
                    Ok(tracks) => {
                        self.status = format!("{name}: {} tracks in the library. {}", tracks.len(), tracks.first().map_or("", |e| e.tid.as_str()));
                        self.show_tracks(cx, ui, &tracks);
                    }
                    Err(error) => {
                        self.status = format!("{name} was not downloaded: {error}");
                        ui.label(cx, ids!(songs_status)).set_text(cx, &self.status);
                    }
                },
            },
            None => {}
        }
    }

    fn pump_arranging(&mut self, cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk) {
        let Some(arranging) = &mut self.arranging else {
            if self.online.is_none() {
                cx.stop_timer(self.timer);
                self.timer = Timer::empty();
            }
            return;
        };
        let title = arranging.title.clone();
        while let Ok(line) = arranging.progress.try_recv() {
            self.status = format!("Arranging {title}: {line}");
        }
        let quiet = arranging.monitor.not_responding();
        let done = take_result(&mut arranging.job);
        if let Some(result) = done {
            self.arranging = None;
            match result {
                Ok((similarity, source)) => {
                    self.status = format!("Arranged {title} ({similarity}): open in the desk");
                    open_imported(cx, ui, desk, &title, source);
                }
                Err(error) => self.status = format!("{title} was not arranged: {error}"),
            }
        }
        let running = self.arranging.is_some();
        ui.label(cx, ids!(songs_status)).set_text(cx, if running && quiet { "not responding — Stop?" } else { &self.status });
        ui.button(cx, ids!(songs_stop)).set_visible(cx, running);
        ui.redraw(cx);
    }
}

/// A song imported (the Song library, a dropped or opened file, the
/// arranger) as the open score, a new document titled for it.
fn open_imported(cx: &mut Cx, ui: &WidgetRef, desk: &mut Desk, title: &str, source: String) {
    desk.open_document(cx, ui, Document::new(title), source);
}

/// The library's tools for the chat: `midi_lookup`, `midi_transcribe`,
/// `score_text`, `song_find_online`, `song_download` (the library's own), and
/// `midi_open` (a song as the open score).
pub fn tool(cx: &mut Cx, _ui: &WidgetRef, desk: &mut Desk, name: &str, args: &Json) -> Answer {
    if matches!(name, "midi_transcribe" | "score_text") {
        return desk.midi_tool(cx, name, args);
    }
    if name != "midi_open" {
        return Answer::Done(tools::call_library(name, args));
    }
    let args = args.clone();
    desk.frame = cx.new_next_frame();
    let control = makepad_score_live::PrepareControl::default();
    desk.jobs.submit_tool(cx, "edit", Some(desk.revision), Some(control.clone()), move || {
        let entries = index::load(&index::index_path()).map_err(|e| format!("the song library is not installed on this machine ({e}): install it from Resources"))?;
        let (entry, bytes) = tools::song(&entries, args.get("song").and_then(Json::as_str).unwrap_or(""))?;
        let presets = args.get("presets").and_then(Json::as_bool).unwrap_or(false);
        let imported = import::import(&bytes, &tools::about(&entry), &Options { presets, ..Options::default() })?;
        let problems = crate::desk::run::check(&imported.source, &control)?;
        if !problems.is_empty() {
            return Err(format!("the imported score does not load:\n{}", problems.join("\n")));
        }
        let parts = imported.parts.iter().map(|p| json::obj(vec![("name", json::s(p.name.clone())), ("role", json::s(p.role.name())), ("voice", json::s(p.voice.about())), ("notes", Json::Int(p.notes as i64))])).collect();
        let sections = imported.sections.iter().map(|s| json::obj(vec![("name", json::s(s.name.clone())), ("bars", Json::Int((s.to - s.from) as i64))])).collect();
        Ok(crate::desk::run::PreparedTool::Edit { source: imported.source, title: Some(entry.label()),
            fields: vec![("song".into(), json::s(entry.label())), ("parts".into(), Json::Arr(parts)), ("sections".into(), Json::Arr(sections))] })
    })
}

/// The library's tools as the chat's service lists them.
pub fn tool_defs() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str, &'static str)> = tools::TOOLS.iter().map(|t| (t.name, t.description, t.input)).collect();
    out.push(("midi_open", "Import a song of the song library (`song`: an id from midi_lookup, or \"Artist - Title\") as a score and open it (the previous score keeps its draft): a loop per part on its GM program and the drums on a GM kit for the genre (`presets: true` puts the synth roles on the engine's production presets instead), the form found from repetition as sections, the file's tempo map, a mix and master chain. Returns the parts and sections. The song stays its owners' work: a cover carries its credit, and publishing one needs a licence for the song.",
        r#"{"type": "object", "properties": {"song": {"type": "string", "minLength": 1, "description": "an id from midi_lookup, or Artist - Title"}, "presets": {"type": "boolean", "description": "synth roles on the production presets"}}, "required": ["song"]}"#));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small MIDI file (one track: a C major arpeggio, twice) opens as a
    /// score that loads, and anything else is refused by name.
    #[test]
    fn a_music_file_imports_as_a_score() {
        let mut track = Vec::new();
        for _ in 0..2 {
            for note in [60u8, 64, 67, 72] {
                track.extend_from_slice(&[0x00, 0x90, note, 0x64, 0x60, 0x80, note, 0x40]);
            }
        }
        track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let mut smf = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x00\x60MTrk".to_vec();
        smf.extend_from_slice(&(track.len() as u32).to_be_bytes());
        smf.extend_from_slice(&track);
        let dir = std::env::temp_dir().join(format!("synth-test-{}-midi", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("little_arp.mid");
        std::fs::write(&path, &smf).unwrap();
        assert!(is_music_file(&path));
        let (title, source) = import_file(&path).unwrap();
        assert_eq!(title, "little arp");
        assert!(crate::desk::run::check(&source, &PrepareControl::default()).unwrap().is_empty(), "{:?}", crate::desk::run::check(&source, &PrepareControl::default()).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(import_file(Path::new("/nowhere/x.txt")).is_err());
        assert!(!is_music_file(Path::new("notes.txt")));
    }
}
