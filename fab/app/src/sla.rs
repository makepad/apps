//! The resin slicer's side of the app: the plate's parts as the sections its layers are cut
//! from ([`fab_sla::section`]), and the Slice render mode's layers on a resin printer
//! ([`ResinView`]). A part that is one distance field is cut from its field; any other from its
//! mesh (the print-tolerance mesh when it has one).
//!
//! The Slice render mode shows the print where it is: the parts stand on their supports
//! ([`fab_sla::supports`]), lifted off the plate, and the supports are in the view's scene and in
//! the same layers. Other views keep the parts on the plate.
//!
//! Once the layers are cut, the print's rules ([`fab_sla::rules`]) are checked over them off the
//! UI thread, and Printability lists every break as an error on its layer.
//!
//! Print sends the stack to the linked resin printer ([`fab_sla::link::linked`]): the stack is
//! written for it off the UI thread, then goes over the link on a thread of its own
//! ([`fab_sla::link::printing`]), which follows the job; Monitor's job card says how far it is
//! and takes pause, resume and stop. Nothing connects to the printer before Start print. While
//! the job prints, Monitor's view shows the stack cut at the layer the printer cured last
//! ([`fab::print::resin::follow`]).
//!
//! The job is kept on disk from its start to its end ([`fab::print::resin::ResinRecord`]). As Fab
//! starts, a job its record says hasn't ended is found again: the link asks the printer how far
//! it is, and nothing else until the person presses Pause, Resume or Stop.

use fab::api::{AppState, ShellAction};
use fab::model::{ElementId, Scene};
use fab::print::resin::{self, Linked, ResinImage, ResinJob, ResinRecord, ResinShow, ResinStage, Stick, Sticks, TestFile};
use fab::print::{issue_markers, jobs, PrintState, PrinterControl};
use fab_model::eval::Evaluated;
use fab_printability::{Footprint, Issue, Report, Severity};
use fab_sla::grid::PixelGrid;
use fab_sla::image::LayerStack;
use fab_sla::load::Load;
use fab_sla::parts::LayerParts;
use fab_sla::points::PointSettings;
use fab_sla::estimate::{print_time, resin_ml, LayerTimes};
use fab_sla::export::{write, Written};
use fab_sla::link::printing::{Ask, End, Printing, Seen, Stage};
use fab_sla::link::State;
use fab_sla::{archive, calibration};
use fab_sla::printer::{Container, ResinProfile, Route};
use fab_sla::rules::{checked, Break, Level, Rule};
use fab_sla::section::{FieldSection, MeshSection, Section};
use fab_sla::supports::{supported, Dropped, SupportSettings, Supported, Supports};
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle};
use makepad_widgets::*;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Plate part `i` as a section.
pub fn section_of(ev: &Evaluated, i: usize) -> Result<Arc<dyn Section>, String> {
    let part = &ev.plate.parts[i];
    if let Some(field) = ev.fields.get(i).and_then(Option::as_ref) {
        let offset = ev.frames.get(i).map_or([0.0; 3], |f| f.offset);
        let b = field.bounds.ok_or_else(|| format!("{} has no solid", part.name))?;
        let bounds = (std::array::from_fn(|k| b.min[k] + offset[k]), std::array::from_fn(|k| b.max[k] + offset[k]));
        let (shape, _, to_printer, _) = field.distance();
        let section = FieldSection::new(shape, *to_printer, bounds).map_err(|e| format!("{}: {e}", part.name))?;
        return Ok(Arc::new(section));
    }
    let fine = match &part.print_mesh {
        Some(make) => Some((make.0)().map_err(|e| format!("{}: no print-tolerance mesh: {e}", part.name))?),
        None => None,
    };
    let mesh = fine.as_ref().unwrap_or(&part.mesh);
    if mesh.triangles.is_empty() {
        return Err(format!("{} has no solid geometry to slice", part.name));
    }
    Ok(Arc::new(MeshSection::new(mesh)))
}

/// Every printable part of the plate as a section, in plate order.
pub fn sections(ev: &Evaluated) -> Result<Vec<Arc<dyn Section>>, String> {
    (0..ev.plate.parts.len()).map(|i| section_of(ev, i)).collect()
}

/// The plate's layer height on its resin printer (mm): the project's setting, else its process
/// profile's.
fn layer_height(print: &PrintState, ev: &Evaluated) -> Result<f64, String> {
    let set = ev.plate.settings.iter().rev().find(|(k, _)| k == "layer_height").map(|(_, v)| v.as_str());
    let text = set.or_else(|| print.base_values.get("layer_height").map(String::as_str)).ok_or("The resin process sets no layer height")?;
    text.trim().parse::<f64>().ok().filter(|h| *h > 0.0).ok_or_else(|| format!("The layer height {text:?} is no height"))
}

/// How the plate's supports are built.
fn support_settings() -> SupportSettings {
    SupportSettings::default()
}

/// The resin profile's keys in `base` (the presets' values), the project's settings over them.
fn resin_config(base: &HashMap<String, String>, ev: &Evaluated) -> makepad_fab_settings::Config {
    let mut cfg = makepad_fab_settings::Config::new();
    for (k, v) in base.iter().chain(ev.plate.settings.iter().map(|(k, v)| (k, v))) {
        if k.starts_with("fab_sla_") {
            cfg.set_str(k, v.as_str());
        }
    }
    cfg
}

/// What the plate's supports are sized against: the resin profile's load keys, each one left out
/// at its default.
fn load_of(base: &HashMap<String, String>, ev: &Evaluated) -> Result<Load, String> {
    Load::from_config(&resin_config(base, ev))
}

/// The job on the plate's resin printer, layers `height` thick: its exposures and peels.
fn job_of(base: &HashMap<String, String>, ev: &Evaluated, height: f64) -> Result<ResinProfile, String> {
    let mut cfg = resin_config(base, ev);
    cfg.set_str("layer_height", height.to_string());
    ResinProfile::on(&ev.machine, &cfg)
}

/// A plate as its resin printer prints it: the layers (the parts lifted, then the supports), the
/// supports, and the view's scene with them in it; for the rules' check, the parts' cured pixels
/// in each layer and their names.
struct Print {
    stack: LayerStack,
    supports: Arc<Supports>,
    scene: Arc<Scene>,
    cured: Vec<LayerParts>,
    /// The parts' covered pixels over every layer ([`Supported::covered`]).
    covered: f64,
    names: Vec<String>,
}

/// The plate's parts on their supports, cut in layers of `height` for the screen of its resin
/// printer.
fn stand_up(ev: &Evaluated, height: f64, load: &Load) -> Result<Supported, String> {
    let screen = ev.machine.resin_screen().ok_or("not a resin printer")?;
    Ok(supported(sections(ev)?, PixelGrid::of(screen), height, &support_settings(), &PointSettings::default(), load))
}

/// The plate on its supports, cut for the screen of its resin printer.
fn print_of(ev: &Evaluated, height: f64, load: &Load, name: &str) -> Result<Print, String> {
    let started = std::time::Instant::now();
    let on = stand_up(ev, height, load)?;
    let supported_in = started.elapsed();
    let scene = crate::plate_scene::build_resin(ev, name, &on.supports.mesh());
    let s = &on.supports;
    log!(
        "fab resin: {} points, {} heads ({} tips sized up), {} pillars ({} sized up), {} bridges, {} pads, {} dropped in {:.0} ms, its scene in {:.0} ms",
        on.points,
        s.heads.len(),
        s.heads.iter().filter(|h| h.tip_radius > s.settings.tip_radius).count(),
        s.pillars.len(),
        s.pillars.iter().filter(|p| p.radius > s.settings.pillar_radius).count(),
        s.bridges.len(),
        s.pads.len(),
        s.dropped.len(),
        supported_in.as_secs_f64() * 1000.0,
        (started.elapsed() - supported_in).as_secs_f64() * 1000.0
    );
    let names = ev.plate.parts.iter().map(|p| p.name.clone()).collect();
    Ok(Print { stack: on.stack, supports: on.supports, scene, cured: on.cured, covered: on.covered, names })
}

/// A break of the print's rules as Printability lists it: on its layers, at its level, named for
/// the part it is in or holds (`names`: the parts', in plate order).
fn issue_of(b: &Break, names: &[String]) -> Issue {
    let (kind, hint) = match b.rule {
        Rule::Connected => ("unconnected_island", "Nothing holds it to the layer before it, so it cures onto the film and the print loses it. Add a support under it, or turn the part."),
        Rule::Clear => ("support_through_part", "A support would print through the part. Move or turn the part so its supports find a clear way to the plate."),
        Rule::Hung => ("loose_support", "It hangs from nothing and falls into the vat with what it holds. Move or turn the part so its supports are built again."),
        Rule::Strong => ("weak_support", "This support is too thin for the peel it carries from this layer on, and can tear off mid-print. Add supports under the part here, turn it so less area peels at once, or use a resin or film that peels lighter."),
        Rule::Cup => ("suction_cup", "A pocket here opens toward the film and seals against it, so every lift pulls on it like a suction cup and can tear the part off its supports; closed over, it keeps liquid resin inside. Turn the part so the pocket opens toward the plate, or add a drain hole at its first layer."),
        Rule::Lost => ("lost_detail", "This feature is thinner than about half a pixel, so the screen lights it too dimly to cure and it doesn't print. Make it thicker if it matters."),
        Rule::Peel => ("peel_area", "This layer peels a large share of the screen at once, which pulls hard on the supports and the plate. Turn the part so its layers are smaller."),
        Rule::Jump => ("peel_jump", "A flat face meets the film all at once here, so the peel jumps. Tilting the part spreads it over more layers."),
        Rule::Bond => ("plate_bond", "The first layer holds less to the plate than the largest layer pulls off the film, so the print can come off the plate. Turn the part so its largest layer is smaller."),
    };
    let severity = match b.level {
        Level::Error => Severity::Error,
        Level::Warning => Severity::Warning,
        Level::Info => Severity::Info,
    };
    let object = b.part.and_then(|i| names.get(i)).cloned();
    let footprint = Footprint { min: [b.at[0] - 0.5 * b.size[0], b.at[1] - 0.5 * b.size[1]], max: [b.at[0] + 0.5 * b.size[0], b.at[1] + 0.5 * b.size[1]] };
    Issue {
        kind,
        severity,
        object,
        at_mm: b.at,
        footprint,
        layer: Some(b.layer),
        layer_to: b.to,
        measure: b.size[0] * b.size[1],
        message: b.what.clone(),
        hint: hint.into(),
        fix: None,
        area_mm2: 0.0,
        lowest_z: b.at[2],
        highest_z: b.at[2],
        flags: 0,
    }
}

/// The breaks of the print's rules in `stack` (its first `names.len()` sections the parts, on
/// `supports`; `cured` their pixels) as Printability's issues, and the supports' own resin (ml);
/// none once `stop` says so.
fn breaks_of(stack: &LayerStack, supports: &Supports, cured: &[LayerParts], names: &[String], stop: &(dyn Fn() -> bool + Sync)) -> (Vec<Issue>, f64) {
    let started = std::time::Instant::now();
    let c = checked(stack, names.len(), supports, cured, stop);
    log!("fab resin: the rules checked over {} layers in {:.0} ms, {} breaks", stack.layers.count, started.elapsed().as_secs_f64() * 1000.0, c.breaks.len());
    (c.breaks.iter().map(|b| issue_of(b, names)).collect(), resin_ml(c.support_covered, &stack.grid, stack.layers.height))
}

/// Layer `k` of `stack` as the screen shows it, on a screen of `levels` greys, over the stack's
/// footprint (every layer the same size): the image and its greys.
fn image_of(stack: &LayerStack, k: u32, levels: u16, id: u64) -> (ResinImage, Vec<u8>) {
    let layer = stack.layer(k);
    let r = stack.footprint();
    let grey_of: [u8; 256] = std::array::from_fn(|s| fab_sla::spans::grey(s as u8, levels));
    // on 256 levels a pixel's grey is its share
    let shares = layer.spans.image(r, 256);
    let greys: Vec<u8> = shares.iter().map(|s| grey_of[*s as usize]).collect();
    let lit = greys.iter().filter(|g| **g > 0).count();
    (ResinImage { layer: k, rect: [r.x0, r.y0, r.x1, r.y1], shares, grey_of, lit, id }, greys)
}

/// A stack's plate revision and layer height (its bits).
type Key = (u64, u64);

/// How often the link asks the printer how far its job is.
const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// A job on the linked resin printer: its stack written for it, then on the link's thread. Or a
/// job Fab started before it restarted, found again: the link only follows it.
struct ResinPrint {
    /// The printer's file, by name.
    link: String,
    /// The job's time per layer: the time left at each layer.
    times: LayerTimes,
    /// The stack being written where it goes, and the printer's address looked up.
    writing: Option<(PathBuf, TaskHandle<Result<SocketAddr, String>>)>,
    printing: Option<Printing>,
    /// Stop was pressed before the link took the job.
    cancelled: bool,
    seen: Option<Seen>,
    /// A job found again: when its record saw it last (when it ended, if it ended unseen).
    found: Option<f64>,
    /// The job is kept on disk: it started printing, or it was found again.
    kept: bool,
}

/// The USB sticks in the computer: its FAT volumes.
fn sticks() -> Result<Vec<Stick>, String> {
    let volumes = makepad_widgets::makepad_platform::storage::mounted_volumes().map_err(|e| e.to_string())?;
    Ok(volumes.into_iter().filter(|v| v.is_fat()).map(|v| Stick { name: v.name, root: v.root }).collect())
}

/// What a restart found: the job's record and its printer's address.
type Found = Option<(ResinRecord, Result<SocketAddr, String>)>;

/// The job on the linked resin printer and its record on disk: they outlive the view (a print
/// goes on whatever the plate).
#[derive(Default)]
struct OnPrinter {
    job: Option<ResinPrint>,
    /// The record a restart left is read once, as Fab starts.
    looked: bool,
    finding: Option<TaskHandle<Found>>,
    /// The job as last kept (its time left and refusals aren't), the record's write on its way,
    /// and the record to write next: one write at a time, the newest next.
    kept: Option<ResinJob>,
    saving: Option<TaskHandle<()>>,
    unsaved: Option<String>,
    /// The print being saved to a stick and read back: its file and the stick's name.
    stick: Option<(String, String, TaskHandle<Result<Written, String>>)>,
}

/// The Slice render mode on a resin printer: the plate's layers cut off the UI thread, the
/// stack once per plate and the layer the slider is on as it moves (one cut at a time; the
/// newest layer asked for is cut next). While it shows, the parts stand lifted on their supports
/// and the view's scene is the one with the supports in it.
#[derive(Default)]
pub struct ResinView {
    /// What the stack is of: the plate (its revision) and the layer height.
    stack: Option<(Key, Arc<LayerStack>)>,
    /// The scene its supports show in.
    scene: Option<Arc<Scene>>,
    building: Option<(Key, TaskHandle<Result<Print, String>>)>,
    /// The rules' check of the stack, once it is cut (its stop: a newer stack is on its way), and
    /// its breaks.
    checking: Option<(Key, Arc<AtomicBool>, TaskHandle<(Vec<Issue>, f64)>)>,
    breaks: Option<(Key, Arc<Vec<Issue>>)>,
    /// The plate's report with the breaks in it, while it is Printability's.
    report: Option<Arc<Report>>,
    /// Printability took the breaks: the panels that list them draw again.
    pub issues_changed: bool,
    cutting: Option<((Key, u32), TaskHandle<(ResinImage, Vec<u8>)>)>,
    /// The stack and layer the shown image is of.
    shown: Option<(Key, u32)>,
    /// The stack whose scene was installed.
    installed: Option<Key>,
    /// The scene with the supports showed and the view went: the plate's comes back.
    pub restore_scene: bool,
    images: u64,
    /// The status line as last drawn, and whether it changed since.
    said: Option<String>,
    pub status_changed: bool,
    /// Export was pressed: the stack is written once it is cut.
    export_asked: bool,
    /// The stack being written, and where.
    exporting: Option<(std::path::PathBuf, TaskHandle<Result<Written, String>>)>,
    /// Counts the stacks cut.
    stacks: u64,
    /// The linked printer, as last read (none: none is linked), and the read on its way: once
    /// the view starts, and on every press of Print.
    linked: Option<Option<fab_sla::link::linked::Linked>>,
    looking: Option<TaskHandle<Result<Option<fab_sla::link::linked::Linked>, String>>>,
    look: bool,
    /// The USB sticks being listed, when the printer prints from one.
    listing: Option<TaskHandle<Result<Vec<Stick>, String>>>,
    /// The plate and layer height the printer's route was read for.
    route_of: Option<Key>,
    /// Start print was pressed: the stack goes now if nothing stops it.
    print_asked: bool,
    /// The orientation test's save was pressed: it goes once the sticks are listed again. Its
    /// file and stick while it is written and read back.
    test_asked: bool,
    testing: Option<(String, String, TaskHandle<Result<Written, String>>)>,
    /// The job on the linked printer and its record.
    on: OnPrinter,
    /// What the Printer panel and the job card show changed: they draw again.
    pub print_changed: bool,
}

impl ResinView {
    /// Follows the plate and the layer slider. Whether what the view shows changed.
    pub fn handle_event(&mut self, cx: &mut Cx, state: &mut AppState, plate: Option<&Arc<Evaluated>>, revision: u64, name: &str) -> bool {
        self.print_changed |= self.find_job(cx, &mut state.print);
        self.print_changed |= self.follow_job(&mut state.print);
        self.print_changed |= self.follow_save(&mut state.print);
        self.keep_job(cx, &state.print);
        let Some((ev, screen)) = plate.and_then(|ev| Some((ev, *ev.machine.resin_screen()?))) else {
            // not a resin printer: nothing shows, nothing is cut (a job goes on)
            self.leave(state);
            self.stop_checking();
            *self = ResinView { images: self.images, restore_scene: self.restore_scene, stacks: self.stacks, on: std::mem::take(&mut self.on), print_changed: self.print_changed, ..ResinView::default() };
            return state.print.resin.take().is_some();
        };
        let mut changed = false;
        let print = &mut state.print;
        let height = layer_height(print, ev);
        let key = (revision, height.as_ref().map_or(0, |h| h.to_bits()));
        // the printer's profile says how its prints get there, once per plate and layer height
        let profile = (self.route_of != Some(key)).then(|| height.clone().and_then(|h| job_of(&print.base_values, ev, h)));
        let show = print.resin.get_or_insert_with(|| {
            changed = true;
            ResinShow { pitch: screen.pitch, note: "Cutting the layers...".into(), ..ResinShow::default() }
        });
        if changed {
            // the view starts: which printer is linked, or which sticks are in
            self.look = true;
        }
        if let Some(profile) = profile {
            self.route_of = Some(key);
            if let Ok(job) = profile {
                let route = match job.output.route {
                    Route::Link => resin::Route::Link,
                    Route::Drive => resin::Route::Stick,
                };
                self.look |= show.route != route;
                (show.route, show.printer, show.file) = (route, job.name.clone(), archive::file_name(name));
                self.print_changed = true;
            }
        }
        self.print_changed |= self.find_printer(cx, show);
        // the stack, once per plate and layer height
        if let Some((rev, task)) = &mut self.building {
            if let Some(done) = task.try_take() {
                let rev = *rev;
                self.building = None;
                match done.map_err(|e| format!("{e:?}")).and_then(|r| r) {
                    Ok(p) => {
                        show.count = p.stack.layers.count;
                        show.layer_height = p.stack.layers.height;
                        show.pitch = p.stack.grid.pitch;
                        show.held = p.supports.held();
                        // a twin's point is its twin's
                        show.unheld = [Dropped::FacesUp, Dropped::NoHead, Dropped::NoRoute].map(|why| p.supports.dropped.iter().filter(|d| d.1 == why).count());
                        show.part_ml = resin_ml(p.covered, &p.stack.grid, p.stack.layers.height);
                        show.support_ml = None;
                        show.print_seconds = job_of(&print.base_values, ev, p.stack.layers.height).ok().map(|job| print_time(&job, p.stack.layers.count));
                        show.note = String::new();
                        let stack = Arc::new(p.stack);
                        let (cut, on, cured, names) = (stack.clone(), p.supports.clone(), p.cured, p.names);
                        let stop = Arc::new(AtomicBool::new(false));
                        let stopped = stop.clone();
                        self.stop_checking();
                        // Printability, the markers and the supports' resin show what it finds
                        if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin rules", move || {
                            let breaks = breaks_of(&cut, &on, &cured, &names, &|| stopped.load(Ordering::Relaxed));
                            SignalToUI::set_ui_signal();
                            breaks
                        }) {
                            self.checking = Some((rev, stop, task));
                        }
                        self.stack = Some((rev, stack));
                        self.scene = Some(p.scene);
                        self.stacks += 1;
                        show.stack = self.stacks;
                    }
                    Err(e) => {
                        show.note = e;
                        show.image = None;
                    }
                }
                changed = true;
            }
        }
        if let Some((rev, _, task)) = &mut self.checking {
            if let Some(done) = task.try_take() {
                let rev = *rev;
                self.checking = None;
                if let Ok((issues, support_ml)) = done {
                    self.breaks = Some((rev, Arc::new(issues)));
                    show.support_ml = Some(support_ml);
                    changed = true;
                }
            }
        }
        changed |= self.report(print, ev, key);
        // Print can send the stack: the plate's as it is, its rules checked
        let ready = self.stack.as_ref().is_some_and(|s| s.0 == key) && self.checking.is_none() && self.breaks.as_ref().is_some_and(|b| b.0 == key);
        let show = print.resin.as_mut().unwrap();
        if show.ready != ready {
            show.ready = ready;
            self.print_changed = true;
        }
        if std::mem::take(&mut self.print_asked) {
            self.start_print(cx, print, ev, name);
            self.print_changed = true;
        }
        self.print_changed |= self.save_test(cx, print, ev);
        if self.stack.as_ref().is_none_or(|s| s.0 != key) && self.building.as_ref().is_none_or(|b| b.0 != key) {
            match height.and_then(|h| load_of(&print.base_values, ev).map(|l| (h, l))) {
                Ok((height, load)) => {
                    let (ev, name) = (ev.clone(), name.to_string());
                    if let Ok(task) = cx.task_pool().submit_named(Class::Interactive, "fab resin layers", move || {
                        let print = print_of(&ev, height, &load, &name);
                        SignalToUI::set_ui_signal();
                        print
                    }) {
                        self.building = Some((key, task));
                        self.stop_checking();
                    }
                }
                Err(e) => {
                    let show = print.resin.as_mut().unwrap();
                    changed |= show.note != e;
                    show.note = e;
                }
            }
        }
        // Export: the plate's stack once it is cut, as the printer shows it
        if self.export_asked && self.exporting.is_none() {
            if let Some((_, stack)) = self.stack.as_ref().filter(|s| s.0 == key) {
                self.export_asked = false;
                let show = print.resin.as_mut().unwrap();
                match job_of(&print.base_values, ev, stack.layers.height) {
                    Ok(job) => {
                        let file = job.output.container == Container::Archive;
                        let (stack, dir) = (stack.clone(), crate::print::export_path(name, if file { archive::EXTENSION } else { "layers" }));
                        let to = dir.clone();
                        if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin export", move || {
                            let started = std::time::Instant::now();
                            let written = if file { archive::write(&to, &stack, &job) } else { write(&to, &stack, &job) };
                            if let Ok(w) = &written {
                                log!("fab resin: wrote {} layers ({:.1} MB) in {:.0} ms", w.layers, w.bytes as f64 / 1e6, started.elapsed().as_secs_f64() * 1000.0);
                            }
                            written
                        }) {
                            show.export = "Writing the layers".into();
                            self.exporting = Some((dir, task));
                        }
                    }
                    Err(e) => show.export = format!("Export failed: {e}"),
                }
                changed = true;
            }
        }
        if let Some((dir, task)) = &mut self.exporting {
            if let Some(done) = task.try_take() {
                let show = print.resin.as_mut().unwrap();
                show.export = match done.map_err(|e| format!("{e:?}")).and_then(|w| w) {
                    Ok(w) => format!("Wrote {} layers to {}", w.layers, dir.display()),
                    Err(e) => format!("Export failed: {e}"),
                };
                self.exporting = None;
                changed = true;
            }
        }
        // the layer the slider is on, while the Slice render mode shows them
        if let Some((key, task)) = &mut self.cutting {
            if let Some(done) = task.try_take() {
                let key = *key;
                self.cutting = None;
                if let Ok((image, greys)) = done {
                    print.resin.as_mut().unwrap().show(cx, image, greys);
                    self.shown = Some(key);
                    changed = true;
                }
            }
        }
        // Monitor's view on the layer the printer cured last
        changed |= resin::follow(print);
        if print.resin_shown().is_some() {
            self.stand(cx, state, key, support_settings().lift());
        } else {
            self.leave(state);
        }
        let print = &mut state.print;
        // the status line shows in Printability (with no issues) and the Job panel: they draw
        // again when it changes
        if changed {
            let said = print.resin.as_ref().map(|r| r.status());
            if said != self.said {
                self.said = said;
                self.status_changed = true;
            }
        }
        let Some((rev, stack)) = self.stack.clone().filter(|_| print.resin_shown().is_some()) else { return changed };
        let k = print.play.layer_hi.min(stack.layers.count.saturating_sub(1));
        if self.cutting.is_none() && self.shown != Some((rev, k)) && stack.layers.count > 0 {
            self.images += 1;
            let (id, levels) = (self.images, screen.grey_levels);
            if let Ok(task) = cx.task_pool().submit_named(Class::Interactive, "fab resin layer", move || {
                let image = image_of(&stack, k, levels, id);
                SignalToUI::set_ui_signal();
                image
            }) {
                self.cutting = Some(((rev, k), task));
            }
        }
        if changed {
            print.bump();
        }
        changed
    }

    /// Export was pressed: the stack goes out once it is cut (one export at a time).
    pub fn ask_export(&mut self) {
        self.export_asked = true;
    }

    /// Print was pressed: which printer is linked is read again (a file added since counts).
    pub fn look_for_printer(&mut self) {
        self.look = true;
    }

    /// Start print was pressed: the stack goes to the linked printer if nothing stops it.
    pub fn ask_print(&mut self) {
        self.print_asked = true;
    }

    /// The orientation test's save was pressed: the sticks are listed again, then it is saved to
    /// the one picked.
    pub fn ask_test(&mut self) {
        (self.test_asked, self.look) = (true, true);
    }

    /// Pause, resume or stop the job; a stop before the link took it cancels it.
    pub fn control(&mut self, control: PrinterControl) {
        let Some(job) = &mut self.on.job else { return };
        let ask = match control {
            PrinterControl::Pause => Ask::Pause,
            PrinterControl::Resume => Ask::Resume,
            PrinterControl::Stop => Ask::Stop,
            PrinterControl::ClearError | PrinterControl::CheckStorage => return,
        };
        log!("fab resin: {ask:?} asked of the printer");
        match &job.printing {
            Some(p) => p.ask(ask),
            None if ask == Ask::Stop => job.cancelled = true,
            None => {}
        }
    }

    /// Close on an ended job: its card goes.
    pub fn close_job(&mut self, print: &mut PrintState) {
        if print.resin_job.as_ref().is_some_and(ResinJob::ended) {
            print.resin_job = None;
            self.on.job = None;
            self.print_changed = true;
        }
    }

    /// Reads which printer is linked when asked, or for a printer that prints from a stick
    /// which USB sticks are in, off the UI thread. Whether `show` changed.
    fn find_printer(&mut self, cx: &mut Cx, show: &mut ResinShow) -> bool {
        let mut changed = false;
        if show.route == resin::Route::Stick {
            if std::mem::take(&mut self.look) && self.listing.is_none() {
                if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin sticks", || {
                    let sticks = sticks();
                    SignalToUI::set_ui_signal();
                    sticks
                }) {
                    self.listing = Some(task);
                    changed = show.sticks != Sticks::Looking;
                    show.sticks = Sticks::Looking;
                }
            }
            if let Some(done) = self.listing.as_mut().and_then(|t| t.try_take()) {
                self.listing = None;
                show.sticks = match done.map_err(|e| format!("{e:?}")).and_then(|s| s) {
                    Ok(found) => Sticks::Found(found),
                    Err(why) => Sticks::Failed(why),
                };
                log!("fab resin: the USB sticks: {:?}", show.sticks);
                changed = true;
            }
            return changed;
        }
        if std::mem::take(&mut self.look) && self.looking.is_none() {
            if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin printer", || {
                let linked = fab_sla::link::linked::linked();
                SignalToUI::set_ui_signal();
                linked
            }) {
                self.looking = Some(task);
                changed = show.linked != Linked::Looking;
                show.linked = Linked::Looking;
            }
        }
        if let Some(done) = self.looking.as_mut().and_then(|t| t.try_take()) {
            self.looking = None;
            let found = done.map_err(|e| format!("{e:?}")).and_then(|l| l);
            show.linked = match &found {
                Ok(Some(l)) => Linked::Found(l.name.clone()),
                Ok(None) => Linked::Missing(String::new()),
                Err(why) => Linked::Missing(why.clone()),
            };
            log!("fab resin: the linked printer: {:?}", show.linked);
            self.linked = found.ok();
            changed = true;
        }
        changed
    }

    /// The stack goes to the linked printer, unless something stops it: it is written for the
    /// printer off the UI thread, then the link takes it ([`Self::follow_job`]).
    fn start_print(&mut self, cx: &mut Cx, print: &mut PrintState, ev: &Evaluated, name: &str) {
        if print.resin.as_ref().is_some_and(|r| r.route == resin::Route::Stick) {
            return self.save_to_stick(cx, print, ev, name);
        }
        let show = print.resin.as_ref().unwrap();
        let now = Cx::time_now();
        let mut job = ResinJob { name: name.to_string(), printer: String::new(), stage: ResinStage::Writing, layer: 0, layers: show.count, left: None, planned: show.print_seconds, started: None, ended: None, note: String::new(), stack: Some(show.stack) };
        let go = if resin::can_print(print, show, ev.plate.parts.is_empty()) { Ok(()) } else { Err(resin::blocker(print, show).unwrap_or_else(|| "the layers aren't ready yet".into())) };
        let stack = self.stack.as_ref().map(|s| s.1.clone());
        let started = go.and_then(|_| {
            let (Some(linked), Some(stack)) = (self.linked.clone().flatten(), stack) else { return Err("the layers or the printer aren't there".to_string()) };
            let profile = job_of(&print.base_values, ev, stack.layers.height)?;
            job.printer = linked.name.clone();
            let link = linked.name.clone();
            let dir = jobs::jobs_dir().join("resin.layers");
            let (to, written) = (dir.clone(), profile.clone());
            let task = cx
                .task_pool()
                .submit_named(Class::Visible, "fab resin print", move || {
                    let started = std::time::Instant::now();
                    let addr = write(&to, &stack, &written).and_then(|w| {
                        log!("fab resin: wrote {} layers ({:.1} MB) for the printer in {:.0} ms", w.layers, w.bytes as f64 / 1e6, started.elapsed().as_secs_f64() * 1000.0);
                        linked.addr()
                    });
                    SignalToUI::set_ui_signal();
                    addr
                })
                .map_err(|e| format!("{e:?}"))?;
            Ok(ResinPrint { link, times: LayerTimes::of(&profile), writing: Some((dir, task)), printing: None, cancelled: false, seen: None, found: None, kept: false })
        });
        match started {
            Ok(p) => {
                log!("fab resin: Start print pressed, {} layers for {}", job.layers, job.printer);
                self.on.job = Some(p);
                // Monitor's view follows the new job
                print.follow.on = true;
            }
            Err(why) => {
                log!("fab resin: Start print refused: {why}");
                job.stage = ResinStage::Failed(format!("not sent: {}", why.trim_end_matches('.')));
                job.ended = Some(now);
                self.on.job = None;
            }
        }
        print.resin_job = Some(job);
    }

    /// The stack is saved to the stick the person picked, unless something stops it: written
    /// there off the UI thread and read back through the independent reader
    /// ([`Self::follow_save`]). The person starts the print on the printer.
    fn save_to_stick(&mut self, cx: &mut Cx, print: &mut PrintState, ev: &Evaluated, name: &str) {
        let show = print.resin.as_ref().unwrap();
        let stick = show.picked_stick().cloned();
        let file = show.file.clone();
        let mut job = ResinJob { name: name.to_string(), printer: show.printer.clone(), stage: ResinStage::Saving { stick: stick.as_ref().map_or(String::new(), |s| s.name.clone()) }, layer: 0, layers: show.count, left: None, planned: show.print_seconds, started: None, ended: None, note: String::new(), stack: Some(show.stack) };
        let go = if resin::can_print(print, show, ev.plate.parts.is_empty()) { Ok(()) } else { Err(resin::blocker(print, show).unwrap_or_else(|| "the layers aren't ready yet".into())) };
        let started = go.and_then(|_| {
            let (Some(stick), Some(stack)) = (stick, self.stack.as_ref().map(|s| s.1.clone())) else { return Err("the layers or the stick aren't there".to_string()) };
            let profile = job_of(&print.base_values, ev, stack.layers.height)?;
            let path = stick.root.join(&file);
            let task = cx
                .task_pool()
                .submit_named(Class::Visible, "fab resin stick", move || {
                    let started = std::time::Instant::now();
                    let saved = archive::save(&path, &stack, &profile);
                    if let Ok(w) = &saved {
                        log!("fab resin: saved {} layers ({:.1} MB) to {} and read them back in {:.0} ms", w.layers, w.bytes as f64 / 1e6, path.display(), started.elapsed().as_secs_f64() * 1000.0);
                    }
                    SignalToUI::set_ui_signal();
                    saved
                })
                .map_err(|e| format!("{e:?}"))?;
            Ok((stick.name, task))
        });
        match started {
            Ok((stick, task)) => {
                log!("fab resin: Save to the stick pressed, {} layers to {stick}", job.layers);
                self.on.stick = Some((file, stick, task));
            }
            Err(why) => {
                log!("fab resin: Save to the stick refused: {why}");
                job.stage = ResinStage::Failed(format!("not saved: {}", why.trim_end_matches('.')));
                job.ended = Some(Cx::time_now());
            }
        }
        self.on.job = None;
        print.resin_job = Some(job);
    }

    /// The orientation test ([`calibration`]) once its save is asked and the sticks are listed
    /// again: built for the plate's printer and saved to the picked stick off the UI thread, then
    /// read back through the independent reader. Whether its card changed.
    fn save_test(&mut self, cx: &mut Cx, print: &mut PrintState, ev: &Evaluated) -> bool {
        let show = print.resin.as_mut().unwrap();
        if let Some(done) = self.testing.as_mut().and_then(|(_, _, t)| t.try_take()) {
            let (file, stick, _) = self.testing.take().unwrap();
            show.test = match done.map_err(|e| format!("{e:?}")).and_then(|w| w) {
                Ok(w) => TestFile::Saved { file, stick, seconds: w.print_seconds },
                Err(why) => TestFile::Failed(why),
            };
            log!("fab resin: the orientation test on the stick: {:?}", show.test);
            return true;
        }
        if !self.test_asked || self.listing.is_some() || self.testing.is_some() {
            return false;
        }
        self.test_asked = false;
        let stick = match (&show.sticks, show.picked_stick()) {
            (_, Some(stick)) => Ok(stick.clone()),
            (Sticks::Failed(why), _) => Err(why.clone()),
            _ => Err("no USB stick found. Put the printer's stick in this computer, then press again".to_string()),
        };
        let started = stick.and_then(|stick| {
            let job = calibration::job(&ev.machine, &resin_config(&print.base_values, ev))?;
            let file = archive::file_name(calibration::NAME);
            let path = stick.root.join(&file);
            let task = cx
                .task_pool()
                .submit_named(Class::Visible, "fab resin test", move || {
                    let saved = archive::save(&path, &calibration::plate(&job), &job);
                    if let Ok(w) = &saved {
                        log!("fab resin: saved the orientation test, {} layers ({:.1} MB), to {}", w.layers, w.bytes as f64 / 1e6, path.display());
                    }
                    SignalToUI::set_ui_signal();
                    saved
                })
                .map_err(|e| format!("{e:?}"))?;
            Ok((file, stick.name, task))
        });
        show.test = match started {
            Ok((file, stick, task)) => {
                let saving = TestFile::Saving { stick: Some(stick.clone()) };
                self.testing = Some((file, stick, task));
                saving
            }
            Err(why) => TestFile::Failed(why),
        };
        true
    }

    /// The print being saved to the stick: once it is there and reads back, the card says to
    /// start it on the printer. Whether the card changed.
    fn follow_save(&mut self, print: &mut PrintState) -> bool {
        let Some(done) = self.on.stick.as_mut().and_then(|(_, _, t)| t.try_take()) else { return false };
        let (file, stick, _) = self.on.stick.take().unwrap();
        let Some(shown) = print.resin_job.as_mut() else { return false };
        shown.stage = match done.map_err(|e| format!("{e:?}")).and_then(|w| w) {
            Ok(_) => ResinStage::Saved { file, stick },
            Err(why) => ResinStage::Failed(format!("not saved: {why}")),
        };
        shown.ended = Some(Cx::time_now());
        log!("fab resin: the print on the stick: {:?}", shown.stage);
        true
    }

    /// The job as its link sees it: the stack written, then sent, then printing. Whether its
    /// card changed.
    fn follow_job(&mut self, print: &mut PrintState) -> bool {
        let (Some(job), Some(shown)) = (self.on.job.as_mut(), print.resin_job.as_mut()) else { return false };
        let before = shown.clone();
        let now = Cx::time_now();
        if let Some((dir, done)) = job.writing.as_mut().and_then(|(dir, t)| Some((dir.clone(), t.try_take()?))) {
            job.writing = None;
            let link = done.map_err(|e| format!("{e:?}")).and_then(|r| r).and_then(|addr| match job.cancelled {
                true => Ok(None),
                false => Printing::start(addr, dir, FOLLOW_EVERY, Arc::new(SignalToUI::set_ui_signal)).map(Some),
            });
            match link {
                Ok(Some(p)) => {
                    job.printing = Some(p);
                    shown.stage = ResinStage::Connecting;
                }
                Ok(None) => shown.stage = ResinStage::Cancelled,
                Err(why) => shown.stage = ResinStage::Failed(why),
            }
        }
        if let Some(seen) = job.printing.as_ref().map(Printing::seen).filter(|s| job.seen.as_ref() != Some(s)) {
            if !seen.printer.is_empty() {
                shown.printer = seen.printer.clone();
            }
            shown.note = seen.refused.clone().unwrap_or_default();
            if let Some(st) = seen.status {
                shown.layer = st.layer;
                shown.layers = st.layers;
                shown.left = Some(job.times.until(st.layers) - job.times.until(st.layer));
            }
            shown.stage = match &seen.stage {
                Stage::Connecting if job.found.is_some() => ResinStage::Reconnecting,
                Stage::Connecting => ResinStage::Connecting,
                Stage::Sending { sent, of } => ResinStage::Sending { sent: *sent, of: *of },
                Stage::Starting => ResinStage::Starting,
                Stage::Following if seen.status.is_some_and(|s| s.state == State::Paused) => ResinStage::Paused,
                Stage::Following => ResinStage::Printing,
                Stage::Ended(End::Done) => ResinStage::Done,
                Stage::Ended(End::Stopped) => ResinStage::Stopped,
                Stage::Ended(End::Cancelled) => ResinStage::Cancelled,
                Stage::Ended(End::Unseen) => ResinStage::EndedUnseen,
                Stage::Ended(End::Failed(why)) => ResinStage::Failed(why.clone()),
            };
            if matches!(seen.stage, Stage::Following | Stage::Ended(End::Done | End::Stopped)) && shown.started.is_none() {
                shown.started = Some(now);
            }
            job.seen = Some(seen);
        }
        if shown.ended() && shown.ended.is_none() {
            // a job that ended unseen ended after it was seen last
            shown.ended = Some(job.found.filter(|_| shown.stage == ResinStage::EndedUnseen).unwrap_or(now));
        }
        if std::mem::discriminant(&shown.stage) != std::mem::discriminant(&before.stage) {
            log!("fab resin: the job on {}: {:?}, layer {} of {}", shown.printer, shown.stage, shown.layer, shown.layers);
        }
        *shown != before
    }

    /// As Fab starts, the job its record says hasn't ended is found again: the record, the
    /// printer's file and its address are read off the UI thread, then the link follows the job,
    /// asking only how far it is. Whether the job card changed.
    fn find_job(&mut self, cx: &mut Cx, print: &mut PrintState) -> bool {
        let on = &mut self.on;
        if !std::mem::replace(&mut on.looked, true) {
            let (now, wall) = (Cx::time_now(), jobs::wall_now());
            if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin record", move || {
                let found = resin::read_record(&jobs::jobs_dir(), now, wall).filter(|r| !r.job.ended()).map(|r| {
                    let addr = fab_sla::link::linked::named(&r.link).and_then(|l| l.addr());
                    (r, addr)
                });
                SignalToUI::set_ui_signal();
                found
            }) {
                on.finding = Some(task);
            }
        }
        let Some(done) = on.finding.as_mut().and_then(|t| t.try_take()) else { return false };
        on.finding = None;
        // a print started meanwhile: its record takes this one's place
        let Some((record, addr)) = done.ok().flatten().filter(|_| on.job.is_none() && print.resin_job.is_none()) else { return false };
        log!("fab resin: found the job on {} again: {:?}, layer {} of {}", record.job.printer, record.job.stage, record.job.layer, record.job.layers);
        let mut shown = ResinJob { stage: ResinStage::Reconnecting, ..record.job.clone() };
        let printing = addr.and_then(|addr| Printing::follow(addr, record.job.layers, FOLLOW_EVERY, Arc::new(SignalToUI::set_ui_signal)));
        let printing = match printing {
            Ok(p) => Some(p),
            Err(why) => {
                shown.stage = ResinStage::Failed(why);
                shown.ended = Some(Cx::time_now());
                None
            }
        };
        let times = LayerTimes { bottom_layers: record.bottom_layers, bottom: record.bottom, layer: record.layer };
        on.job = Some(ResinPrint { link: record.link, times, writing: None, printing, cancelled: false, seen: None, found: Some(record.seen), kept: true });
        on.kept = Some(record.job);
        print.resin_job = Some(shown);
        true
    }

    /// The job is kept on disk from when it starts printing: its record is written again
    /// whenever what its card shows changes, off the UI thread, one write at a time.
    fn keep_job(&mut self, cx: &mut Cx, print: &PrintState) {
        let on = &mut self.on;
        if let (Some(job), Some(shown)) = (on.job.as_mut(), print.resin_job.as_ref()) {
            job.kept |= matches!(shown.stage, ResinStage::Starting | ResinStage::Printing | ResinStage::Paused);
            let kept = ResinJob { left: None, note: String::new(), stack: None, ..shown.clone() };
            if job.kept && shown.stage != ResinStage::Reconnecting && on.kept.as_ref() != Some(&kept) {
                let now = Cx::time_now();
                let t = job.times;
                let record = ResinRecord { link: job.link.clone(), job: kept.clone(), bottom_layers: t.bottom_layers, bottom: t.bottom, layer: t.layer, seen: now };
                on.unsaved = Some(record.text(now, jobs::wall_now()));
                on.kept = Some(kept);
            }
        }
        if on.saving.as_mut().is_some_and(|t| t.try_take().is_some()) {
            on.saving = None;
        }
        if on.saving.is_some() {
            return;
        }
        let Some(text) = on.unsaved.clone() else { return };
        if let Ok(task) = cx.task_pool().submit_named(Class::Visible, "fab resin record", move || {
            if let Err(e) = resin::save_record(&jobs::jobs_dir(), &text) {
                log!("fab resin: the job's record could not be written: {e}");
            }
            SignalToUI::set_ui_signal();
        }) {
            on.saving = Some(task);
            on.unsaved = None;
        }
    }

    /// The rules' check, if one runs, stops: its stack is gone or another is on its way.
    fn stop_checking(&mut self) {
        if let Some((_, stop, task)) = self.checking.take() {
            stop.store(true, Ordering::Relaxed);
            task.detach();
        }
    }

    /// Printability lists the breaks of the stack of `key` with the plate's own issues, once
    /// they are found. Whether it changed.
    fn report(&mut self, print: &mut PrintState, ev: &Evaluated, key: Key) -> bool {
        let Some((_, issues)) = self.breaks.as_ref().filter(|b| b.0 == key && !b.1.is_empty()) else { return false };
        if self.report.as_ref().is_some_and(|r| Arc::ptr_eq(r, &print.printability)) || !Arc::ptr_eq(&print.printability, &ev.printability) {
            return false;
        }
        let mut report = (*ev.printability).clone();
        report.issues.extend(issues.iter().cloned());
        let report = Arc::new(report);
        print.model_markers = Some(issue_markers(&report));
        print.printability = report.clone();
        self.report = Some(report);
        self.issues_changed = true;
        true
    }

    /// The slice shows: the printed parts stand `lift` up (mm) on their supports, and once the plate's
    /// supports are built the view's scene has them.
    fn stand(&mut self, cx: &mut Cx, state: &mut AppState, key: Key, lift: f64) {
        let parts: HashSet<ElementId> = state.plate.parts.iter().filter(|p| p.printed).filter_map(|p| p.element).collect();
        let up = (lift * 0.001) as f32;
        if state.scene_state.lift.as_ref().is_none_or(|(on, at)| **on != parts || *at != up) {
            state.scene_state.set_lift(Some((Arc::new(parts), up)));
        }
        if let Some(scene) = self.scene.as_ref().filter(|_| self.stack.as_ref().is_some_and(|s| s.0 == key) && self.installed != Some(key)) {
            cx.action(ShellAction::SceneUpdated { scene: scene.clone(), frame: false });
            self.installed = Some(key);
        }
    }

    /// The slice went: the parts back on the plate, and the plate's scene back if ours shows.
    fn leave(&mut self, state: &mut AppState) {
        state.scene_state.set_lift(None);
        if self.installed.take().is_some() && self.scene.as_ref().is_some_and(|s| Arc::ptr_eq(s, &state.scene)) {
            self.restore_scene = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_model::{Change, FabModel, PlaceArg};

    /// The area of `points`' convex hull grown by `grow` (mm²).
    fn hull_area(points: &[[f64; 2]], grow: f64) -> f64 {
        let mut p = points.to_vec();
        p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        p.dedup();
        let turn = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
        let mut h: Vec<[f64; 2]> = Vec::new();
        for pass in 0..2 {
            let start = h.len();
            let order: Vec<[f64; 2]> = if pass == 0 { p.clone() } else { p.iter().rev().copied().collect() };
            for q in order {
                while h.len() >= start + 2 && turn(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                    h.pop();
                }
                h.push(q);
            }
            h.pop();
        }
        let n = h.len();
        let area = 0.5 * (0..n).map(|i| h[i][0] * h[(i + 1) % n][1] - h[(i + 1) % n][0] * h[i][1]).sum::<f64>();
        let rim = (0..n).map(|i| (h[(i + 1) % n][0] - h[i][0]).hypot(h[(i + 1) % n][1] - h[i][1])).sum::<f64>();
        area.max(0.0) + rim * grow + std::f64::consts::PI * grow * grow
    }

    /// The resin census's printer: the simulated printer's 120 x 80 mm bed at 0.1 mm pixels and
    /// 0.1 mm layers, a quarter of its pixels and half its layers. Its least tip, five of its
    /// pixels, is 0.5 mm: as wide as any tip starts on the simulated printer (twice the tip
    /// radius).
    fn census_printer() -> ResinProfile {
        let mut cfg = fab_sla::printer::simulated_config();
        cfg.set_str("fab_sla_resolution", "1200x800");
        cfg.set_str("fab_sla_pixel_pitch", "0.1x0.1");
        cfg.set_str("layer_height", "0.1");
        ResinProfile::from_config("Census printer", &cfg).expect("the census printer reads")
    }

    /// The demos the resin census prints on its printer, each with what it brings. They are
    /// chosen so that each of the structure's rules, broken in the supports (islands given no
    /// points, heads that skip their clearance, a part's last base given no pad, tips sized for
    /// half their load), fails at least one of them; no demo left out caught a break these
    /// miss. A demo too big for the bed is the demo test's ([`crate::demo_part`]).
    const CENSUS: [(&str, &str); 3] = [
        ("chainmail", "rings hung through rings: heads whose first way down meets the ring below"),
        ("ball_cage", "a ball loose in its cage: islands held from below, bases on their pads, and tips at their least size that carry nearly all they hold"),
        ("marble_block", "a mesh part, where the others are fields"),
    ];

    /// The placements of the plate `ev` (its changes `c`): an assembly's parts are one.
    fn placements_of(c: &fab_model::program::Compiled, ev: &Evaluated) -> Vec<(String, String)> {
        let mut placements: Vec<(String, String)> = Vec::new();
        for p in &ev.plate.parts {
            if let Some(Change::Place { instance, object, .. }) = Change::place(c, &p.object, PlaceArg::At(None)) {
                if !placements.contains(&(instance.clone(), object.clone())) {
                    placements.push((instance, object));
                }
            }
        }
        placements
    }

    /// The placement `alone` of the plate `m` (evaluated as `ev`, its placements `placements`) by
    /// itself on the resin printer `profile`'s bed, where that bed arranges it, on its supports:
    /// they hold it and the structure's rules are kept; how it peels (its cups, lost detail and
    /// peel) is printed. Gives its issues and the top layer it cures.
    fn stood_alone(what: &str, m: &mut FabModel, ev: Evaluated, e: &mut fab_model::eval::Evaluator, placements: &[(String, String)], alone: &(String, String), profile: &ResinProfile) -> (Vec<Issue>, Option<u32>) {
        // the plate's places are for a filament printer's bed: the resin printer's arranges it
        let free: Vec<Change> = placements.iter().map(|p| Change::Place { instance: p.0.clone(), object: p.1.clone(), arg: if p == alone { PlaceArg::At(None) } else { PlaceArg::Show(false) } }).collect();
        let head = m.head();
        let (_, built) = m.apply(e, "alone", head, &free, None).unwrap_or_else(|f| panic!("{what}: {f:?}"));
        let ev = built.map_or(ev, |b| b.1);
        let on = stand_up(&ev, profile.layer_height, &Load::default()).unwrap_or_else(|f| panic!("{what}: {f}"));
        // the first layer against the hull pad it replaced (the feet's hull grown by a base and
        // the brim), and what the load sized up
        let (sup, s) = (&on.supports, on.supports.settings);
        let feet: Vec<[f64; 2]> = sup.pillars.iter().map(|p| [p.top[0], p.top[1]]).collect();
        let first = on.stack.layer(0).area(&on.stack.grid);
        let tips = sup.heads.iter().filter(|h| h.tip_radius > s.tip_radius).count();
        let wide = sup.pillars.iter().filter(|p| p.radius > s.pillar_radius).count();
        println!("{what}: first layer {first:.0} mm² against the hull's {:.0} mm²; {} pads; {tips} tips and {wide} of {} pillars sized up", hull_area(&feet, s.base_radius + s.brim), sup.pads.len(), sup.pillars.len());
        let names: Vec<String> = ev.plate.parts.iter().map(|p| p.name.clone()).collect();
        let (issues, _) = breaks_of(&on.stack, &on.supports, &on.cured, &names, &|| false);
        let (broken, peel): (Vec<&Issue>, Vec<&Issue>) = issues.iter().partition(|i| ["unconnected_island", "support_through_part", "loose_support", "weak_support"].contains(&i.kind));
        for i in &peel {
            println!("{what}: {}", shown(i));
        }
        let found: Vec<String> = broken.iter().map(|i| shown(i)).collect();
        assert!(found.is_empty(), "{what}: {found:#?}");
        assert!(on.supports.held() > 0, "{what}: no supports");
        let rim = on.cured.iter().rposition(|l| l.pixels() > 0).map(|k| k as u32);
        (issues, rim)
    }

    /// An issue as the census tells it.
    fn shown(i: &Issue) -> String {
        format!("{} {} {:?} on layers {:?} to {:?} at {:?}: {}", i.severity.name(), i.kind, i.object, i.layer, i.layer_to, i.at_mm, i.message)
    }

    /// Every placement of the demo `key`, each alone on the resin printer `profile`'s bed where
    /// the plate arranges it, stands on supports that hold it, keeps the structure's rules and
    /// has no error. Each placement starts from its plate as loaded and evaluated.
    fn prints_alone(key: &str, profile: &ResinProfile, e: &mut fab_model::eval::Evaluator) {
        let d = crate::demo_part::demo(key).unwrap_or_else(|| panic!("{key}: not a demo"));
        let model = || d.model().unwrap_or_else(|f| panic!("{key}: {f}"));
        let m = model();
        let (c, ev) = m.evaluate(e, None).unwrap_or_else(|f| panic!("{key}: {f}"));
        let placements = placements_of(&c, &ev);
        assert!(!placements.is_empty(), "{key}: nothing placed");
        let mut loaded = Some((m, ev));
        for alone in &placements {
            let (mut m, ev) = loaded.take().unwrap_or_else(|| {
                let m = model();
                let (_, ev) = m.evaluate(e, None).unwrap_or_else(|f| panic!("{key}: {f}"));
                (m, ev)
            });
            let what = format!("{key} {}/{}", alone.0, alone.1);
            let (issues, _) = stood_alone(&what, &mut m, ev, e, &placements, alone, profile);
            let errors: Vec<String> = issues.iter().filter(|i| i.severity == Severity::Error).map(shown).collect();
            assert!(errors.is_empty(), "{what}: {errors:#?}");
        }
    }

    /// The resin census: each demo of [`CENSUS`] prints alone on the census printer
    /// ([`prints_alone`]). Then the bud vase on the simulated printer itself prints on filament
    /// only: a vase holds water, so a drain hole is no fix, and stood on the resin printer its
    /// cup seals against the film from its floor up to its rim: its only errors are its cup's.
    #[test]
    fn the_resin_census_keeps_the_rules() {
        let profile = census_printer();
        let mut e = fab_model::eval::Evaluator::new(profile.machine());
        for (key, _) in CENSUS {
            prints_alone(key, &profile, &mut e);
        }
        // the bud vase, on the printer whose layers its cup is read in
        let profile = fab_sla::printer::simulated();
        let mut e = fab_model::eval::Evaluator::new(profile.machine());
        let mut m = crate::demo_part::model();
        let (c, ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("demo: {f}"));
        let placements = placements_of(&c, &ev);
        let vase = placements.iter().find(|p| p.0 == "vase").expect("the demo's vase").clone();
        let (issues, rim) = stood_alone("the bud vase", &mut m, ev, &mut e, &placements, &vase, &profile);
        let errors: Vec<String> = issues.iter().filter(|i| i.severity == Severity::Error).map(shown).collect();
        let cups: Vec<&Issue> = issues.iter().filter(|i| i.severity == Severity::Error && i.kind == "suction_cup").collect();
        assert!(!cups.is_empty() && cups.len() == errors.len(), "the bud vase prints on filament only for its cup, and has no other error: {errors:#?}");
        assert!(cups[0].layer_to == rim, "the bud vase's cup: {} to {rim:?}", shown(cups[0]));
    }
}
