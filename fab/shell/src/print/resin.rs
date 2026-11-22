//! A resin printer's slice, as the Slice render mode shows it: the plate's layers as images on
//! the screen's pixels. The app cuts them (the resin slicer); this is what the view, the layer
//! slider and the layer panel read: how many layers, and the one the slider is on as an image.
//!
//! The view draws the parts cut at the layer's top with its image on the cut, and the panel
//! shows the image flat, each pixel's covered share and the grey the screen lights it with.
//!
//! Print sends the stack to the linked resin printer ([`Linked`]) and Monitor follows the job
//! ([`ResinJob`]): the Printer panel's confirm card asks first ([`question`], [`blocker`]), and
//! the job card says how far it is and offers pause, resume and stop ([`job_view`]). A printer
//! that reads its prints from a USB stick ([`Route::Stick`]) gets the print saved to the stick
//! the person picks ([`Sticks`]); the person starts it on the printer, and nothing comes back.
//!
//! A job Fab started is kept on disk as it prints ([`ResinRecord`]), so Fab finds it again after
//! a restart and Monitor follows it from where it was. While the job prints on the plate's own
//! layers, Monitor's view shows the layer the printer cured last ([`follow`]); moving the layers
//! by hand stops that until Back to live.

use super::job_card::JobView;
use super::trays::DrawFabChip;
use super::{format_time, PrintState};
use crate::api::AppState;
use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
use makepad_widgets_core::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A layer's image flat: each texel the grey its pixel shows, the bed's +Y up.
    mod.draw.DrawFabResinImage = mod.std.set_type_default() do #(DrawFabResinImage::script_shader(vm)){
        ..mod.draw.DrawQuad
        image: texture_2d(float)
        u_lit: uniform(vec4(0.55, 0.85, 1.0, 1.0))
        u_screen: uniform(vec4(0.05, 0.07, 0.10, 1.0))
        pixel: fn() {
            let grey = self.image.sample_nearest(vec2(self.pos.x, 1.0 - self.pos.y)).x
            return vec4(self.u_screen.xyz.mix(self.u_lit.xyz, grey), 1.0)
        }
    }

    mod.widgets.FabResinPanelBase = #(FabResinPanel::register_widget(vm))
    mod.widgets.FabResinPanel = set_type_default() do mod.widgets.FabResinPanelBase{
        width: 320
        height: 240
        draw_bg: mod.draw.DrawFabChip{ radius: 4.0 }
        draw_image: mod.draw.DrawFabResinImage{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_active
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }
}

/// The plate's layers on a resin printer.
#[derive(Clone, Debug, Default)]
pub struct ResinShow {
    /// The layers in the stack.
    pub count: u32,
    /// Their height (mm): layer `k` cures from `k * layer_height` to `(k + 1) * layer_height`.
    pub layer_height: f64,
    /// The screen's pixel pitch along the bed's X and Y (mm).
    pub pitch: [f64; 2],
    /// The image of the layer the slider is on, once it is cut.
    pub image: Option<Arc<ResinImage>>,
    /// Its greys on the GPU, as the screen shows them: one texture the panel and the view both
    /// draw, its pixels written again in place for each layer.
    pub texture: Option<Texture>,
    /// Said in place of the image: the layers being cut, or why they can't be.
    pub note: String,
    /// The parts' supports: the points that stand on one, and those that don't, by why: their
    /// surface faces up, no head clears the part from them, no way down from their head.
    pub held: usize,
    pub unheld: [usize; 3],
    /// The job's time on the printer (s), when its profile sets the exposures and peels.
    pub print_seconds: Option<f64>,
    /// The parts' resin (ml), and the supports' own once the check has measured them.
    pub part_ml: f64,
    pub support_ml: Option<f64>,
    /// What Export did: writing the layers, where it wrote them, or why it could not.
    pub export: String,
    /// The stack is the plate's as it is now, and the print's rules are checked over it: Print
    /// can send it.
    pub ready: bool,
    /// Counts the stacks cut: what "Print anyway" was ticked for.
    pub stack: u64,
    /// The stack whose printability errors the person accepted ("Print anyway"); off on every
    /// opening of the confirm card.
    pub accepted: Option<u64>,
    /// The resin printer linked in Fab's printers folder.
    pub linked: Linked,
    /// How the print gets to the printer, and the printer's profile name.
    pub route: Route,
    pub printer: String,
    /// The USB sticks found, the one the person picked (none: the first found), and the file
    /// the print gets there.
    pub sticks: Sticks,
    pub stick: Option<PathBuf>,
    pub file: String,
    /// The orientation test's file, saved to the stick on its own press.
    pub test: TestFile,
}

/// How a print gets to the resin printer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Route {
    /// Over Fab's own link to it.
    #[default]
    Link,
    /// Saved to a USB stick, which the person puts in the printer and starts the print on its
    /// touchscreen: no status comes back.
    Stick,
}

/// The USB sticks (FAT volumes) found, as last looked for: on the view's start and on every
/// press of Print.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Sticks {
    #[default]
    Looking,
    Found(Vec<Stick>),
    /// The volumes can't be listed: why.
    Failed(String),
}

/// A USB stick: its name and its root folder, where the printer looks for prints.
#[derive(Clone, Debug, PartialEq)]
pub struct Stick {
    pub name: String,
    pub root: PathBuf,
}

/// The orientation test's file on its way to the USB stick, or how its save went. The test is
/// Fab's own small print that shows whether the printer's image is the right way round.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TestFile {
    #[default]
    Unsaved,
    /// Looking for the sticks again (no stick yet), then saving to the one picked.
    Saving { stick: Option<String> },
    /// On the stick and read back whole: its file, the stick, and its time on the printer (s).
    Saved { file: String, stick: String, seconds: f64 },
    Failed(String),
}

/// The resin printer linked in Fab's printers folder, as last looked for (on the view's start
/// and on every press of Print).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Linked {
    #[default]
    Looking,
    /// None is linked, or its file can't be read: why (empty when there is no file).
    Missing(String),
    /// Its file's name.
    Found(String),
}

/// How far a job on the resin printer is.
#[derive(Clone, Debug, PartialEq)]
pub enum ResinStage {
    /// The stack is written for the printer.
    Writing,
    Connecting,
    Sending { sent: u32, of: u32 },
    Starting,
    /// A job found again after a restart: the printer is asked how far it is.
    Reconnecting,
    Printing,
    Paused,
    Done,
    Stopped,
    /// Stopped before it started: nothing printed.
    Cancelled,
    /// The job found again isn't on the printer any more: it ended while Fab was closed.
    EndedUnseen,
    Failed(String),
    /// The print is written to the stick `stick` and read back.
    Saving { stick: String },
    /// The print is on the stick as `file`, read back whole: the person starts it on the
    /// printer, and nothing comes back from it.
    Saved { file: String, stick: String },
}

/// A job on the linked resin printer, from the press of Start print to its end.
#[derive(Clone, Debug, PartialEq)]
pub struct ResinJob {
    /// The plate's.
    pub name: String,
    /// The printer's: its file's, then as it said hello.
    pub printer: String,
    pub stage: ResinStage,
    /// Layers cured, of `layers`.
    pub layer: u32,
    pub layers: u32,
    /// The job's time left (s) while it prints, from the printer's layer and the profile.
    pub left: Option<f64>,
    /// The job's time on the printer (s), as planned.
    pub planned: Option<f64>,
    /// When it started printing and when it ended (`Cx::time_now`).
    pub started: Option<f64>,
    pub ended: Option<f64>,
    /// Why the printer refused the last pause, resume or stop.
    pub note: String,
    /// The plate's stack it prints ([`ResinShow::stack`]), while this run of Fab has it: Monitor's
    /// view follows the printer's layer on it. Not kept on disk.
    pub stack: Option<u64>,
}

impl ResinJob {
    /// Over: it can't be asked anything any more.
    pub fn ended(&self) -> bool {
        matches!(self.stage, ResinStage::Done | ResinStage::Stopped | ResinStage::Cancelled | ResinStage::EndedUnseen | ResinStage::Failed(_) | ResinStage::Saved { .. })
    }
}

/// The record's file in the jobs folder ([`super::jobs::jobs_dir`]).
pub const RECORD: &str = "resin.job";
const RECORD_FORMAT: &str = "1";

/// The job Fab started on the resin printer, as it keeps it in the jobs folder from the job's
/// start to its end: a restart finds a job not ended and follows it again, with what Monitor
/// showed of it last. A print saved to a stick is never followed, so it is never kept.
#[derive(Clone, Debug, PartialEq)]
pub struct ResinRecord {
    /// The printer's file in Fab's printers folder, by name: where the job went.
    pub link: String,
    /// The job as the card showed it (its time left and refusals aren't kept).
    pub job: ResinJob,
    /// The job's time per layer, for its time left: its `bottom_layers` first layers take
    /// `bottom` seconds each, the others `layer`.
    pub bottom_layers: u32,
    pub bottom: f64,
    pub layer: f64,
    /// When the job was seen as it is.
    pub seen: f64,
}

impl ResinRecord {
    /// The record's text, its times on the wall clock (`now`: `Cx::time_now`, `wall`:
    /// [`super::jobs::wall_now`]). A job on its way to the printer is kept as starting, one being
    /// found again as printing.
    pub fn text(&self, now: f64, wall: f64) -> String {
        let there = |t: f64| wall - (now - t);
        let j = &self.job;
        let (stage, why) = match &j.stage {
            ResinStage::Writing | ResinStage::Connecting | ResinStage::Sending { .. } | ResinStage::Starting => ("starting", ""),
            ResinStage::Reconnecting | ResinStage::Printing => ("printing", ""),
            ResinStage::Paused => ("paused", ""),
            ResinStage::Done => ("done", ""),
            ResinStage::Stopped => ("stopped", ""),
            ResinStage::Cancelled => ("cancelled", ""),
            ResinStage::EndedUnseen => ("ended_unseen", ""),
            ResinStage::Failed(why) => ("failed", why.as_str()),
            // no record reads back as these: nothing follows them
            ResinStage::Saving { .. } | ResinStage::Saved { .. } => ("on_stick", ""),
        };
        let mut out = format!("format={RECORD_FORMAT}\nlink={}\nname={}\nprinter={}\nstage={stage}\n", one_line(&self.link), one_line(&j.name), one_line(&j.printer));
        if !why.is_empty() {
            out += &format!("why={}\n", one_line(why));
        }
        out += &format!("layer={}\nlayers={}\nbottom_layers={}\nbottom_seconds={}\nlayer_seconds={}\nseen={}\n", j.layer, j.layers, self.bottom_layers, self.bottom, self.layer, there(self.seen));
        for (key, t) in [("planned", j.planned), ("started", j.started.map(there)), ("ended", j.ended.map(there))] {
            if let Some(t) = t {
                out += &format!("{key}={t}\n");
            }
        }
        out
    }

    /// A record's `text`, its times on this run's clock; none when it isn't one (a newer format).
    pub fn parse(text: &str, now: f64, wall: f64) -> Option<ResinRecord> {
        let map: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        if map.get("format").copied() != Some(RECORD_FORMAT) {
            return None;
        }
        let here = |t: f64| now - (wall - t);
        let get = |key: &str| map.get(key).copied();
        let num = |key: &str| get(key).and_then(|v| v.parse::<f64>().ok());
        let stage = match get("stage")? {
            "starting" => ResinStage::Starting,
            "printing" => ResinStage::Printing,
            "paused" => ResinStage::Paused,
            "done" => ResinStage::Done,
            "stopped" => ResinStage::Stopped,
            "cancelled" => ResinStage::Cancelled,
            "ended_unseen" => ResinStage::EndedUnseen,
            "failed" => ResinStage::Failed(get("why").unwrap_or("").to_string()),
            _ => return None,
        };
        let job = ResinJob {
            name: get("name")?.to_string(),
            printer: get("printer")?.to_string(),
            stage,
            layer: get("layer")?.parse().ok()?,
            layers: get("layers")?.parse().ok()?,
            left: None,
            planned: num("planned"),
            started: num("started").map(here),
            ended: num("ended").map(here),
            note: String::new(),
            stack: None,
        };
        Some(ResinRecord { link: get("link")?.to_string(), job, bottom_layers: get("bottom_layers")?.parse().ok()?, bottom: num("bottom_seconds")?, layer: num("layer_seconds")?, seen: here(num("seen")?) })
    }
}

impl PrintState {
    /// The layer Monitor's view shows of the resin job it could follow: the newest the printer
    /// cured (the first before any). None unless Monitor shows, holds no filament job, the job
    /// prints, and the plate's layers are the job's.
    pub fn resin_followed(&self) -> Option<u32> {
        let (job, show) = (self.resin_job.as_ref()?, self.resin.as_ref()?);
        let prints = matches!(job.stage, ResinStage::Reconnecting | ResinStage::Printing | ResinStage::Paused);
        let ours = job.stack == Some(show.stack) && show.count == job.layers && show.count > 0;
        (self.follow.monitor && !self.monitor_holds() && prints && ours).then(|| job.layer.saturating_sub(1).min(show.count - 1))
    }

    /// [`Self::resin_followed`] while Monitor follows the printer (the person's toggle).
    pub fn resin_follows(&self) -> Option<u32> {
        self.resin_followed().filter(|_| self.follow.on)
    }
}

/// Monitor's view on the layer the printer cured last while it follows the resin job: the
/// slice shows, cut there. Whether the view moved.
pub fn follow(p: &mut PrintState) -> bool {
    let Some(k) = p.resin_follows() else { return false };
    let play = &mut p.play;
    if play.active && !play.playing && play.layer_lo == 0 && play.layer_hi == k && p.focus_layer == Some(k) {
        return false;
    }
    play.active = true;
    play.playing = false;
    play.layer_lo = 0;
    play.layer_hi = k;
    p.focus_layer = Some(k);
    p.bump();
    true
}

/// A value on one line.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// Writes the record's `text` in the jobs folder `dir`, whole or not at all.
pub fn save_record(dir: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let writing = dir.join(format!("{RECORD}.new"));
    std::fs::write(&writing, text)?;
    std::fs::rename(writing, dir.join(RECORD))
}

/// The record in the jobs folder `dir`, its times on this run's clock: none when there is none
/// or it can't be read.
pub fn read_record(dir: &Path, now: f64, wall: f64) -> Option<ResinRecord> {
    ResinRecord::parse(&std::fs::read_to_string(dir.join(RECORD)).ok()?, now, wall)
}

/// The resin job in words, and the ways out it has now: Cancel until it prints, Pause or Resume
/// and Stop while it does, Close once it ended. A job being found again offers nothing until
/// the printer said how far it is.
pub fn job_view(job: &ResinJob, now: f64) -> JobView {
    let printer = &job.printer;
    let mut v = JobView { running: !job.ended(), ..JobView::default() };
    match &job.stage {
        ResinStage::Writing => {
            v.phase = format!("Writing the layers for {printer}");
            v.cancel = true;
        }
        ResinStage::Connecting => {
            v.phase = format!("Connecting to {printer}");
            v.cancel = true;
        }
        ResinStage::Sending { sent, of } => {
            v.phase = format!("Sending the layers to {printer}: {sent} of {of}");
            v.fraction = Some(*sent as f32 / (*of).max(1) as f32);
            v.cancel = true;
        }
        ResinStage::Starting => {
            v.phase = format!("Starting: waiting for {printer} to answer");
            v.stop = true;
        }
        ResinStage::Reconnecting => v.phase = format!("Reconnecting to {printer}…"),
        ResinStage::Printing => {
            v.phase = format!("Printing on {printer}");
            v.pause = Some(false);
            v.stop = true;
        }
        ResinStage::Paused => {
            v.phase = "Paused".into();
            v.pause = Some(true);
            v.stop = true;
        }
        ResinStage::Done => {
            v.phase = "Finished".into();
            v.close = true;
        }
        ResinStage::Stopped => {
            v.phase = "Stopped.".into();
            v.close = true;
        }
        ResinStage::Cancelled => {
            v.phase = "Cancelled: nothing printed.".into();
            v.close = true;
        }
        ResinStage::EndedUnseen => {
            v.phase = "It ended while Fab was closed: how isn't known.".into();
            v.close = true;
        }
        ResinStage::Failed(why) => {
            v.phase = format!("Failed: {why}.");
            v.attention = true;
            v.close = true;
        }
        ResinStage::Saving { stick } => v.phase = format!("Saving the print to the stick {stick} and reading it back"),
        ResinStage::Saved { .. } => {
            v.phase = "Printing on the printer: no live status over USB".into();
            v.close = true;
        }
    }
    if job.started.is_some() && job.layers > 0 {
        v.fraction = Some(job.layer as f32 / job.layers as f32);
    }
    let mut times = Vec::new();
    if let Some(t) = job.started {
        times.push(format!("Elapsed {}", format_time(job.ended.unwrap_or(now) - t)));
    }
    if let Some(left) = job.left.filter(|_| job.stage == ResinStage::Printing) {
        times.push(format!("Left {}", format_time(left)));
        times.push(format!("Finish about {}", super::job_stats::finish_at(left / 60.0)));
    }
    if let Some(t) = job.planned {
        times.push(format!("Planned {}", format_time(t)));
    }
    v.times = times.join("  ·  ");
    let mut detail = vec![job.name.clone()];
    if job.started.is_some() {
        detail.push(format!("Layer {} / {}", job.layer, job.layers));
        detail.push(format!("{:.0} %", 100.0 * job.layer as f64 / job.layers.max(1) as f64));
    } else {
        detail.push(format!("{} layers", job.layers));
    }
    v.detail = detail.join("  ·  ");
    v.note = match &job.stage {
        ResinStage::Saved { file, stick } => format!("{file} is on the stick {stick}, read back whole. Eject the stick, put it in {printer} and start the print on its touchscreen."),
        _ => job.note.clone(),
    };
    v
}

/// What the Printer panel's confirm card asks about the resin printer: the print's question
/// once the stack is ready, how far it is before that.
pub fn question(r: &ResinShow, empty_plate: bool) -> String {
    let printer = match &r.linked {
        _ if r.route == Route::Stick => r.printer.as_str(),
        Linked::Found(name) => name.as_str(),
        _ => "the resin printer",
    };
    if empty_plate {
        "The plate is empty: there is nothing to print yet.".into()
    } else if !r.note.is_empty() {
        r.note.clone()
    } else if !r.ready {
        format!("Preparing the print for {printer}: cutting the layers and checking the print's rules…")
    } else if r.route == Route::Stick {
        match r.picked_stick() {
            Some(stick) => format!("Save the {} layers to the stick {} as {}? Then eject the stick, put it in {printer} and start the print on its touchscreen.", r.count, stick.name, r.file),
            None => format!("Save the {} layers to a USB stick as {}? Then put the stick in {printer} and start the print on its touchscreen.", r.count, r.file),
        }
    } else {
        format!("Print the {} layers on {printer}? They go to the printer over the network, and it starts at once.", r.count)
    }
}

/// What stops Start print on the resin printer whatever the person ticks: a job still on it, no
/// printer linked; for a printer that prints from a stick, a print still being saved, no stick.
pub fn firm_blocker(p: &PrintState, r: &ResinShow) -> Option<String> {
    match p.resin_job.as_ref().map(|j| &j.stage) {
        Some(ResinStage::Saving { .. }) => return Some("The last print is still being saved to the stick.".into()),
        Some(_) if p.resin_job.as_ref().is_some_and(|j| !j.ended()) => return Some("A job is on the printer: it has to end first.".into()),
        _ => {}
    }
    if r.route == Route::Stick {
        return match &r.sticks {
            Sticks::Looking => Some("Looking for USB sticks…".into()),
            Sticks::Found(found) if found.is_empty() => Some("No USB stick found: put the printer's stick in this computer, then press Print again.".into()),
            Sticks::Found(_) => None,
            Sticks::Failed(why) => Some(format!("The USB sticks can't be listed: {why}.")),
        };
    }
    match &r.linked {
        Linked::Looking => Some("Looking for the resin printer…".into()),
        Linked::Missing(why) if why.is_empty() => Some(NO_RESIN_PRINTER.into()),
        Linked::Missing(why) => Some(format!("The resin printer can't be read: {why}.")),
        Linked::Found(_) => None,
    }
}

/// What stops Start print on the resin printer now: [`firm_blocker`], or printability errors the
/// person hasn't accepted ("Print anyway"). None once nothing stops it; while the stack isn't
/// ready the question says how far it is.
pub fn blocker(p: &PrintState, r: &ResinShow) -> Option<String> {
    firm_blocker(p, r).or_else(|| if r.ready && !errors_accepted(r) { p.errors_text() } else { None })
}

/// The person ticked "Print anyway" for the stack as it is.
pub fn errors_accepted(r: &ResinShow) -> bool {
    r.ready && r.accepted == Some(r.stack)
}

/// Start print sends the stack now: it is ready and nothing stops it.
pub fn can_print(p: &PrintState, r: &ResinShow, empty_plate: bool) -> bool {
    r.ready && !empty_plate && blocker(p, r).is_none()
}

/// No USB stick is in the computer: what to do.
pub const NO_STICK: &str = "Put the printer's USB stick (FAT32) in this computer, then press Print again. Export writes the same file to your documents.";

/// The orientation test: what it is and how to read it.
pub const TEST_TEXT: &str = "Print this once before the first real print from Fab: a 10 mm F on a thin pad, about 0.2 ml of resin, on the Normal process. Take it off the plate and lay it pad down. Seen from above, it reads as an F when Fab sends the layers the right way round. If it reads mirrored, the printer profile's mirror setting is wrong: don't print parts with it until that is fixed.";

/// No resin printer is linked: what to do.
pub const NO_RESIN_PRINTER: &str = "No resin printer is linked. Add a file named after it, ending in .resin, to ~/.makepad/fab/printers: a line host = its address and a line port = its port. Then press Print again. Export works without one.";

/// The orientation test card's line on its save: none before the first press.
pub fn test_status(r: &ResinShow) -> Option<String> {
    Some(match &r.test {
        TestFile::Unsaved => return None,
        TestFile::Saving { stick: None } => "Looking for the USB stick...".into(),
        TestFile::Saving { stick: Some(stick) } => format!("Saving the test to the stick {stick}..."),
        TestFile::Saved { file, stick, seconds } => format!("{file} is on the stick {stick}, read back whole. Eject the stick, put it in {} and start the print on its touchscreen: about {} min.", r.printer, (seconds / 60.0).ceil().max(1.0)),
        TestFile::Failed(why) => format!("The test wasn't saved: {}.", why.trim_end_matches('.')),
    })
}

impl ResinShow {
    /// The stick the print is saved to: the one the person picked while it is there, else the
    /// first found.
    pub fn picked_stick(&self) -> Option<&Stick> {
        let Sticks::Found(found) = &self.sticks else { return None };
        found.iter().find(|s| Some(&s.root) == self.stick.as_ref()).or(found.first())
    }

    /// The top of layer `k` (mm): where the view cuts the parts.
    pub fn top_of(&self, k: u32) -> f64 {
        (k + 1) as f64 * self.layer_height
    }

    /// Shows `image`, `greys` its pixels as the screen lights them (row by row over its rect).
    pub fn show(&mut self, cx: &mut Cx, image: ResinImage, greys: Vec<u8>) {
        let (w, h) = (image.width() as usize, image.height() as usize);
        match self.texture.as_ref().map(|t| t.get_format(cx)) {
            // the stack's layers are all its footprint's size: the pixels change, not the texture
            Some(TextureFormat::VecRu8 { width, height, data, updated, .. }) if (*width, *height) == (w, h) => {
                *data = Some(greys);
                *updated = updated.update(None);
            }
            _ => {
                let format = TextureFormat::VecRu8 { width: w, height: h, data: Some(greys), unpack_row_length: None, updated: TextureUpdated::Full };
                self.texture = Some(Texture::new_with_format(cx, format));
            }
        }
        self.image = Some(Arc::new(image));
    }

    /// The slice's status line: the stack, or what stands in its place.
    pub fn status(&self) -> String {
        if !self.note.is_empty() {
            return self.note.clone();
        }
        // a pixel's sides in µm, one when it is square: "on 16.8 × 24.8 µm pixels"
        let um = |mm: f64| format!("{:.1}", mm * 1000.0).trim_end_matches(".0").to_string();
        let pixel = if um(self.pitch[0]) == um(self.pitch[1]) { um(self.pitch[0]) } else { format!("{} × {}", um(self.pitch[0]), um(self.pitch[1])) };
        let layers = format!("{} layers of {:.3} mm on {pixel} µm pixels", self.count, self.layer_height);
        let unheld: usize = self.unheld.iter().sum();
        let stack = if unheld == 0 {
            format!("{layers}; {} supports.", self.held)
        } else {
            let why: Vec<String> = ["facing up", "with no clear head", "with no way down"].iter().zip(self.unheld).filter(|(_, n)| *n > 0).map(|(w, n)| format!("{n} {w}")).collect();
            let points = if unheld == 1 { "1 point".to_string() } else { format!("{unheld} points") };
            format!("{layers}; {} supports, {points} without one: {}.", self.held, why.join(", "))
        };
        let export = if self.export.is_empty() { String::new() } else { format!(" {}.", self.export) };
        format!("{stack} {}{export}", self.estimate())
    }

    /// The job's time and resin: "About 27:58 to print, 0.50 ml of resin in the parts and 0.31
    /// ml in the supports."
    pub fn estimate(&self) -> String {
        let ml = |v: f64| if v < 10.0 { format!("{v:.2} ml") } else { format!("{v:.1} ml") };
        let time = self.print_seconds.map_or(String::new(), |t| format!("About {} to print, ", super::format_time(t)));
        let resin = format!("{} of resin in the parts", ml(self.part_ml));
        match self.support_ml {
            Some(s) => format!("{time}{resin} and {} in the supports.", ml(s)),
            None => format!("{time}{resin}."),
        }
    }
}

/// One layer as the screen shows it.
#[derive(Clone, Debug)]
pub struct ResinImage {
    pub layer: u32,
    /// The pixels it spans, the stack's footprint (every layer's the same): columns
    /// `rect[0]..rect[2]`, rows `rect[1]..rect[3]`, pixel `(i, j)` on the bed from
    /// `(i, j) * pitch` to `(i + 1, j + 1) * pitch` (row 0 at the bed's Y = 0).
    pub rect: [u32; 4],
    /// Each pixel's covered share (255: wholly), row by row over `rect`.
    pub shares: Vec<u8>,
    /// The grey the screen lights a pixel of each share with.
    pub grey_of: [u8; 256],
    /// The pixels it lights at all.
    pub lit: usize,
    /// Equal only for the same image (the view's texture follows it).
    pub id: u64,
}

impl ResinImage {
    pub fn width(&self) -> u32 {
        self.rect[2] - self.rect[0]
    }

    pub fn height(&self) -> u32 {
        self.rect[3] - self.rect[1]
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Pixel `(i, j)`'s covered share (0 outside the image).
    pub fn share_at(&self, i: u32, j: u32) -> u8 {
        if i < self.rect[0] || i >= self.rect[2] || j < self.rect[1] || j >= self.rect[3] {
            return 0;
        }
        self.shares[(j - self.rect[1]) as usize * self.width() as usize + (i - self.rect[0]) as usize]
    }
}

/// What the layer panel says about pixel `(i, j)` of `image`.
pub fn pixel_readout(image: &ResinImage, i: u32, j: u32) -> String {
    let share = image.share_at(i, j);
    let grey = image.grey_of[share as usize];
    format!("Pixel {i}, {j}  ·  {:.0} % covered  ·  grey {grey}", share as f64 * 100.0 / 255.0)
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabResinImage {
    #[deref]
    draw_super: DrawQuad,
}

/// The shown layer flat, beside the view: the image fitted to the panel (the bed's +Y up), its
/// layer, height and lit pixels above it; hovering a pixel tips its covered share and grey.
#[derive(Script, ScriptHook, Widget)]
pub struct FabResinPanel {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawFabChip,
    #[live]
    draw_image: DrawFabResinImage,
    #[live]
    draw_text: DrawText,
    /// Where the image was drawn in the panel, and which image.
    #[rust]
    drawn: Option<(Rect, Arc<ResinImage>)>,
    #[rust]
    hover: Option<(u32, u32)>,
    /// The line above the image: the layer, its height and lit pixels.
    #[rust]
    head: String,
}

impl FabResinPanel {
    /// The image's pixel under `abs`, and its rect on screen.
    fn pixel_at(&self, cx: &Cx, abs: DVec2) -> Option<(u32, u32, Rect)> {
        let (inner, im) = self.drawn.as_ref()?;
        // where the panel ended up: its row is laid out at the bottom after it draws
        let panel = self.draw_bg.area().rect(cx);
        let r = &Rect { pos: panel.pos + inner.pos, size: inner.size };
        let (fx, fy) = ((abs.x - r.pos.x) / r.size.x, (abs.y - r.pos.y) / r.size.y);
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            return None;
        }
        let (w, h) = (im.width() as f64, im.height() as f64);
        let (a, b) = ((fx * w).floor(), ((1.0 - fy) * h).floor().min(h - 1.0));
        let (pw, ph) = (r.size.x / w, r.size.y / h);
        let at = Rect { pos: dvec2(r.pos.x + a * pw, r.pos.y + (h - 1.0 - b) * ph), size: dvec2(pw, ph) };
        Some((im.rect[0] + a as u32, im.rect[1] + b as u32, at))
    }
}

impl Widget for FabResinPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Image;
        makepad_platform::a11y::set_string(&mut node.name, &self.head);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                let now = self.pixel_at(cx, fh.abs);
                if now.map(|p| (p.0, p.1)) != self.hover {
                    self.hover = now.map(|p| (p.0, p.1));
                    match (now, &self.drawn) {
                        (Some((i, j, at)), Some((_, im))) => {
                            let mut request = TipRequest::plain(pixel_readout(im, i, j), at);
                            request.place = TipPlace::Right;
                            cx.widget_action(self.uid, TipAction::HoverInWith(request));
                        }
                        _ => cx.widget_action(self.uid, TipAction::HoverOut),
                    }
                }
                cx.set_cursor(if now.is_some() { MouseCursor::Crosshair } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                self.hover = None;
                cx.widget_action(self.uid, TipAction::HoverOut);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(show) = scope.data.get::<AppState>().and_then(|s| s.print.resin_shown()) else { return DrawStep::done() };
        let rect = cx.walk_turtle(walk);
        self.draw_bg.fill = vec4(0.0, 0.0, 0.0, 0.62);
        self.draw_bg.ring = vec4(1.0, 1.0, 1.0, 0.08);
        self.draw_bg.spool = 0.0;
        self.draw_bg.draw_abs(cx, rect);
        let image = show.image.clone().filter(|im| !im.is_empty());
        self.head = match &show.image {
            Some(im) => format!("Layer {} of {}  ·  {:.2} mm  ·  {} px lit", im.layer + 1, show.count, show.top_of(im.layer), im.lit),
            None => show.note.clone(),
        };
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + 10.0, rect.pos.y + 8.0), &self.head);
        self.drawn = None;
        let (Some(im), Some(texture)) = (image, &show.texture) else { return DrawStep::done() };
        // the image's own shape (mm), as large as the panel under its line takes
        let area = Rect { pos: dvec2(rect.pos.x + 10.0, rect.pos.y + 28.0), size: dvec2(rect.size.x - 20.0, rect.size.y - 38.0) };
        let (mw, mh) = (im.width() as f64 * show.pitch[0], im.height() as f64 * show.pitch[1]);
        let k = (area.size.x / mw).min(area.size.y / mh);
        let size = dvec2(mw * k, mh * k);
        let at = Rect { pos: area.pos + (area.size - size) * 0.5, size };
        self.draw_image.draw_super.draw_vars.set_texture(0, texture);
        self.draw_image.draw_abs(cx, at);
        self.drawn = Some((Rect { pos: at.pos - rect.pos, size: at.size }, im));
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status line gives a pixel's sides in µm, the side once when the pixel is square.
    #[test]
    fn the_status_line_gives_a_pixels_sides() {
        let line = |pitch| ResinShow { count: 3, layer_height: 0.05, pitch, ..ResinShow::default() }.status();
        assert!(line([0.0168, 0.0248]).starts_with("3 layers of 0.050 mm on 16.8 × 24.8 µm pixels; "), "{}", line([0.0168, 0.0248]));
        assert!(line([0.05, 0.05]).starts_with("3 layers of 0.050 mm on 50 µm pixels; "), "{}", line([0.05, 0.05]));
    }

    fn job(stage: ResinStage) -> ResinJob {
        ResinJob { name: "block".into(), printer: "Simulated resin".into(), stage, layer: 0, layers: 220, left: None, planned: Some(1678.0), started: None, ended: None, note: String::new(), stack: None }
    }

    /// Every stage of a job on the resin printer says what is going on, and offers its way out:
    /// Cancel until it prints, Pause or Resume and Stop while it prints, Close once it ended.
    /// While it prints the card says its layer, how far, the time it took and the time left.
    #[test]
    fn every_stage_of_a_resin_job_says_what_is_going_on_and_has_its_way_out() {
        let v = job_view(&job(ResinStage::Writing), 0.0);
        assert_eq!((v.phase.as_str(), v.cancel, v.stop, v.pause, v.close, v.running), ("Writing the layers for Simulated resin", true, false, None, false, true));
        assert_eq!((v.times.as_str(), v.detail.as_str()), ("Planned 27:58", "block  ·  220 layers"));
        let v = job_view(&job(ResinStage::Sending { sent: 55, of: 220 }), 0.0);
        assert_eq!((v.phase.as_str(), v.fraction, v.cancel), ("Sending the layers to Simulated resin: 55 of 220", Some(0.25), true));
        let v = job_view(&job(ResinStage::Starting), 0.0);
        assert!(v.phase.starts_with("Starting") && v.stop && !v.cancel, "{v:?}");

        let printing = ResinJob { layer: 110, started: Some(10.0), left: Some(839.0), ..job(ResinStage::Printing) };
        let v = job_view(&printing, 70.0);
        assert_eq!((v.phase.as_str(), v.pause, v.stop, v.cancel, v.fraction), ("Printing on Simulated resin", Some(false), true, false, Some(0.5)));
        assert!(v.times.starts_with("Elapsed 1:00  ·  Left 13:59  ·  Finish about ") && v.times.ends_with("  ·  Planned 27:58"), "{}", v.times);
        assert_eq!(v.detail, "block  ·  Layer 110 / 220  ·  50 %");
        let v = job_view(&ResinJob { stage: ResinStage::Paused, note: "the printer refused PAUSE: the printer is paused".into(), ..printing.clone() }, 70.0);
        assert_eq!((v.phase.as_str(), v.pause, v.stop, v.times.as_str()), ("Paused", Some(true), true, "Elapsed 1:00  ·  Planned 27:58"));
        assert_eq!(v.note, "the printer refused PAUSE: the printer is paused", "a refused ask is said");
        // found again after a restart: what it was, and nothing to press until the printer answers
        let v = job_view(&ResinJob { stage: ResinStage::Reconnecting, left: None, ..printing.clone() }, 70.0);
        assert_eq!((v.phase.as_str(), v.running, v.cancel, v.stop, v.pause, v.close), ("Reconnecting to Simulated resin…", true, false, false, None, false));
        assert_eq!(v.detail, "block  ·  Layer 110 / 220  ·  50 %");

        let done = ResinJob { layer: 220, ended: Some(100.0), ..printing.clone() };
        for (stage, phase) in [
            (ResinStage::Done, "Finished"),
            (ResinStage::Stopped, "Stopped."),
            (ResinStage::Cancelled, "Cancelled: nothing printed."),
            (ResinStage::EndedUnseen, "It ended while Fab was closed: how isn't known."),
            (ResinStage::Failed("the printer refused PRINT: no manifest came".into()), "Failed: the printer refused PRINT: no manifest came."),
        ] {
            let failed = matches!(stage, ResinStage::Failed(_));
            let v = job_view(&ResinJob { stage, ..done.clone() }, 500.0);
            assert_eq!((v.phase.as_str(), v.close, v.running, v.stop, v.pause, v.attention), (phase, true, false, false, None, failed));
            assert_eq!(v.times, "Elapsed 1:30  ·  Planned 27:58", "the time it took, not the clock");
        }
    }

    /// A print saved to a stick: while it is written and read back nothing can be pressed; once
    /// it is there the card says the printer prints it with no live status, the planned time,
    /// and what to do, and it can be closed. Nothing on the card pretends to follow it.
    #[test]
    fn a_print_saved_to_a_stick_says_to_start_it_on_the_printer() {
        let v = job_view(&job(ResinStage::Saving { stick: "PRINTS".into() }), 0.0);
        assert_eq!((v.phase.as_str(), v.running, v.cancel, v.stop, v.pause, v.close), ("Saving the print to the stick PRINTS and reading it back", true, false, false, None, false));
        let saved = ResinJob { ended: Some(30.0), ..job(ResinStage::Saved { file: "block.file".into(), stick: "PRINTS".into() }) };
        assert!(saved.ended(), "nothing can be asked of it");
        let v = job_view(&saved, 500.0);
        assert_eq!((v.phase.as_str(), v.running, v.close, v.stop, v.pause, v.fraction), ("Printing on the printer: no live status over USB", false, true, false, None, None));
        assert_eq!((v.times.as_str(), v.detail.as_str()), ("Planned 27:58", "block  ·  220 layers"));
        assert_eq!(v.note, "block.file is on the stick PRINTS, read back whole. Eject the stick, put it in Simulated resin and start the print on its touchscreen.");
        let record = ResinRecord { link: String::new(), job: saved, bottom_layers: 4, bottom: 33.5, layer: 6.65, seen: 30.0 };
        assert_eq!(ResinRecord::parse(&record.text(0.0, 0.0), 0.0, 0.0), None, "a print on a stick is never found again");
    }

    /// The orientation test's card says nothing before its press, then which stick it goes to,
    /// then where it is and how long it prints, or why it wasn't saved.
    #[test]
    fn the_orientation_test_says_where_it_went() {
        let mut r = ResinShow { route: Route::Stick, printer: "Vat printer".into(), ..Default::default() };
        assert_eq!(test_status(&r), None);
        r.test = TestFile::Saving { stick: Some("STICK".into()) };
        assert_eq!(test_status(&r).as_deref(), Some("Saving the test to the stick STICK..."));
        r.test = TestFile::Saved { file: "test.file".into(), stick: "STICK".into(), seconds: 610.0 };
        let said = test_status(&r).unwrap();
        assert!(said.starts_with("test.file is on the stick STICK, read back whole.") && said.contains("put it in Vat printer") && said.ends_with("about 11 min."), "{said}");
        r.test = TestFile::Failed("no USB stick found.".into());
        assert_eq!(test_status(&r).as_deref(), Some("The test wasn't saved: no USB stick found."));
    }

    /// A printer that prints from a stick asks to save the print to the stick picked (the first
    /// found until one is picked), and before that says it is looking, that none is in, or why
    /// they can't be listed; a print still being saved stops the next. Fab's link plays no part.
    #[test]
    fn the_stick_confirm_card_asks_to_save_to_the_stick_picked() {
        let mut p = PrintState::default();
        let mut r = ResinShow { count: 220, stack: 3, route: Route::Stick, printer: "Vat printer".into(), file: "block.file".into(), ..Default::default() };
        assert!(question(&r, false).starts_with("Preparing the print for Vat printer: cutting the layers"));
        assert_eq!(blocker(&p, &r).as_deref(), Some("Looking for USB sticks…"));
        r.sticks = Sticks::Found(Vec::new());
        assert_eq!(blocker(&p, &r).as_deref(), Some("No USB stick found: put the printer's stick in this computer, then press Print again."));
        r.ready = true;
        assert_eq!(question(&r, false), "Save the 220 layers to a USB stick as block.file? Then put the stick in Vat printer and start the print on its touchscreen.");
        r.ready = false;
        r.sticks = Sticks::Failed("no mount table".into());
        assert_eq!(blocker(&p, &r).as_deref(), Some("The USB sticks can't be listed: no mount table."));
        let stick = |name: &str| Stick { name: name.into(), root: PathBuf::from("/sticks").join(name) };
        r.sticks = Sticks::Found(vec![stick("A"), stick("B")]);
        r.ready = true;
        assert_eq!((blocker(&p, &r), can_print(&p, &r, false)), (None, true), "no printer link needed");
        assert_eq!(question(&r, false), "Save the 220 layers to the stick A as block.file? Then eject the stick, put it in Vat printer and start the print on its touchscreen.");
        r.stick = Some(PathBuf::from("/sticks/B"));
        assert_eq!(r.picked_stick().map(|s| s.name.as_str()), Some("B"));
        r.stick = Some(PathBuf::from("/sticks/gone"));
        assert_eq!(r.picked_stick().map(|s| s.name.as_str()), Some("A"), "a stick taken out: the first again");
        p.resin_job = Some(job(ResinStage::Saving { stick: "A".into() }));
        assert_eq!(firm_blocker(&p, &r).as_deref(), Some("The last print is still being saved to the stick."));
        p.resin_job = Some(job(ResinStage::Saved { file: "block.file".into(), stick: "A".into() }));
        assert!(can_print(&p, &r, false), "a saved print is no job in the way");
    }

    /// The job is kept as its card showed it, its times on the wall clock: read in a later run,
    /// they are on that run's clock. A job on its way is kept as starting. A record of a newer
    /// format, or none, is no job.
    #[test]
    fn a_resin_job_is_kept_on_disk_as_its_card_showed_it() {
        let paused = ResinJob { layer: 110, started: Some(10.0), left: Some(839.0), note: "the printer refused PAUSE".into(), stack: Some(3), ..job(ResinStage::Paused) };
        let record = ResinRecord { link: "vat".into(), job: paused, bottom_layers: 4, bottom: 33.5, layer: 6.65, seen: 70.0 };
        // written at 100 s on its run's clock, 1000 s on the wall; read at 20 s, 5000 s on the wall
        let read = ResinRecord::parse(&record.text(100.0, 1000.0), 20.0, 5000.0).expect("the record");
        let shifted = ResinJob { started: Some(10.0 - 4080.0), left: None, note: String::new(), stack: None, ..record.job.clone() };
        assert_eq!(read, ResinRecord { job: shifted, seen: 70.0 - 4080.0, ..record.clone() });
        for (stage, kept) in [
            (ResinStage::Writing, ResinStage::Starting),
            (ResinStage::Starting, ResinStage::Starting),
            (ResinStage::Reconnecting, ResinStage::Printing),
            (ResinStage::Printing, ResinStage::Printing),
            (ResinStage::Done, ResinStage::Done),
            (ResinStage::Stopped, ResinStage::Stopped),
            (ResinStage::EndedUnseen, ResinStage::EndedUnseen),
            (ResinStage::Failed("the printer refused\nSTOP".into()), ResinStage::Failed("the printer refused STOP".into())),
        ] {
            let r = ResinRecord { job: ResinJob { ended: Some(90.0), ..job(stage.clone()) }, ..record.clone() };
            let read = ResinRecord::parse(&r.text(0.0, 0.0), 0.0, 0.0).expect("the record");
            assert_eq!((read.job.stage, read.job.ended), (kept, Some(90.0)), "{stage:?}");
        }

        let dir = std::env::temp_dir().join(format!("fab-resin-record-{}", std::process::id()));
        assert_eq!(read_record(&dir, 0.0, 0.0), None, "no folder, no job");
        save_record(&dir, &record.text(0.0, 0.0)).unwrap();
        assert_eq!(read_record(&dir, 0.0, 0.0).map(|r| r.job.layer), Some(110));
        assert!(!dir.join(format!("{RECORD}.new")).exists(), "written whole");
        save_record(&dir, &record.text(0.0, 0.0).replace("format=1", "format=2")).unwrap();
        assert_eq!(read_record(&dir, 0.0, 0.0), None, "a newer record isn't read");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// In Monitor, the view follows the printer: the slice shows, cut at the layer the printer
    /// cured last, as long as the job prints on the plate's own layers. Moving the layers by hand
    /// stops it until Back to live; another look, another stack or an ended job doesn't follow.
    #[test]
    fn monitor_follows_the_resin_printers_layer() {
        let mut p = PrintState::default();
        p.resin = Some(ResinShow { count: 220, stack: 3, ..Default::default() });
        p.resin_job = Some(ResinJob { layer: 110, stack: Some(3), ..job(ResinStage::Printing) });
        assert_eq!(p.resin_follows(), None, "only in Monitor");
        p.follow.monitor = true;
        assert_eq!(p.resin_follows(), Some(109), "the layer cured last");
        assert!(follow(&mut p) && p.play.active && (p.play.layer_lo, p.play.layer_hi, p.focus_layer) == (0, 109, Some(109)));
        assert!(!follow(&mut p), "it is there");
        p.resin_job.as_mut().unwrap().layer = 0;
        assert!(follow(&mut p) && p.play.layer_hi == 0, "the first layer before any cured");
        // the layers moved by hand: it stays until Back to live
        p.resin_job.as_mut().unwrap().layer = 111;
        super::super::apply(&mut p, &super::super::PrintAction::SetTopLayer(12));
        assert_eq!((p.resin_follows(), p.resin_followed(), p.play.layer_hi), (None, Some(110), 12));
        super::super::apply(&mut p, &super::super::PrintAction::FollowPrinter(true));
        assert_eq!((p.resin_follows(), p.play.layer_hi), (Some(110), 110));
        for (stack, stage) in [(Some(4), ResinStage::Printing), (None, ResinStage::Printing), (Some(3), ResinStage::Done), (Some(3), ResinStage::Sending { sent: 1, of: 220 })] {
            p.resin_job = Some(ResinJob { layer: 111, stack, ..job(stage.clone()) });
            assert_eq!(p.resin_followed(), None, "{stack:?} {stage:?}");
        }
    }

    /// The confirm card asks once the stack is ready and a printer is linked. Before, it says
    /// how far the stack is or what to do; a job still on the printer stops it; printability
    /// errors stop it until "Print anyway" is ticked for this stack.
    #[test]
    fn the_resin_confirm_card_asks_once_the_stack_is_ready_and_says_what_stops_it() {
        let mut p = PrintState::default();
        let mut r = ResinShow { count: 220, stack: 3, ..Default::default() };
        assert!(question(&r, false).starts_with("Preparing the print for the resin printer: cutting the layers"));
        assert_eq!(blocker(&p, &r).as_deref(), Some("Looking for the resin printer…"));
        r.linked = Linked::Missing(String::new());
        assert_eq!(blocker(&p, &r).as_deref(), Some(NO_RESIN_PRINTER));
        r.linked = Linked::Missing("vat.resin: no port, or not one".into());
        assert_eq!(blocker(&p, &r).as_deref(), Some("The resin printer can't be read: vat.resin: no port, or not one."));
        r.linked = Linked::Found("vat".into());
        assert_eq!((blocker(&p, &r), can_print(&p, &r, false)), (None, false), "not ready: the question says so");
        r.ready = true;
        assert_eq!(question(&r, false), "Print the 220 layers on vat? They go to the printer over the network, and it starts at once.");
        assert!(can_print(&p, &r, false));
        assert!(!can_print(&p, &r, true) && question(&r, true).starts_with("The plate is empty"));
        // a printability error stops it until accepted for this very stack
        let footprint = fab_printability::Footprint::square([30.0, 30.0, 11.0], 2.0);
        let issue = fab_printability::Issue { kind: "unconnected_island", severity: fab_printability::Severity::Error, object: None, at_mm: [30.0, 30.0, 11.0], footprint, layer: Some(220), layer_to: None, measure: 0.0, message: "a 1.00 mm² island cures onto the film".into(), hint: "Add a support.".into(), fix: None, area_mm2: 0.0, lowest_z: 11.0, highest_z: 11.0, flags: 0 };
        p.printability = Arc::new(fab_printability::Report { issues: vec![issue], ..Default::default() });
        assert_eq!(blocker(&p, &r).as_deref(), Some("Not printable: a 1.00 mm² island cures onto the film on layer 221. Add a support."));
        assert_eq!(firm_blocker(&p, &r), None, "the person may print anyway");
        r.accepted = Some(3);
        assert!(errors_accepted(&r) && can_print(&p, &r, false));
        r.stack = 4;
        assert!(!errors_accepted(&r) && !can_print(&p, &r, false), "a new stack is asked again");
        r.accepted = Some(4);
        // a job on the printer stops it until it ends
        p.resin_job = Some(job(ResinStage::Printing));
        assert_eq!(firm_blocker(&p, &r).as_deref(), Some("A job is on the printer: it has to end first."));
        p.resin_job = Some(job(ResinStage::Done));
        assert!(can_print(&p, &r, false), "an ended job is no job");
    }
}
