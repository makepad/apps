//! The G-code writer (R1 §8–9); the printer's own commands come from its [`Firmware`].
//!
//! Lines are built in memory and fed through the interpreter as they are
//! made, so the move records, the time line, the M73 progress lines and the
//! header totals all come from one pass; the text is assembled once at the
//! end.

use crate::cal_bands;
use crate::firmware::{self, Firmware};
use crate::fmt;
use crate::process::{self, Processor, Timeline};
use crate::sequential;
use std::sync::{Condvar, Mutex};
use crate::tower::{self as tw, Block, Section, Tower, TowerLayer};
use crate::vars::{self, PlateFacts};
use makepad_fab_settings::change::ChangeModel;
use makepad_fab_settings::machine::Job;
use makepad_fab_settings::family;
use makepad_fab_settings::template::{self, Dialect, Env, TVar, Template, TemplateError};
use makepad_fab_slicer::geom::*;
use makepad_fab_slicer::grid::SegGrid;
use makepad_fab_toolpath::{flow, ExtrusionPath, Layer, NpBlock, Role, Toolpaths};

/// Templates Fab ships for printers without an installed profile: a plain
/// home / heat / prime sequence in the template language, written for Fab.
pub mod fallback {
    pub const START: &str = "; Fab start sequence (no printer profile installed)\nM140 S[bed_temperature_initial_layer_single]\nM104 S{first_layer_temperature[initial_extruder]}\nG28\nM190 S[bed_temperature_initial_layer_single]\nM109 S{first_layer_temperature[initial_extruder]}\nG90\nM83\nG1 Z5 F1200\n";
    pub const END: &str = "; Fab end sequence\nM104 S0\nM140 S0\nM106 S0\nG91\nG1 Z10 F600\nG90\nM84\n";
    pub const LAYER: &str = "";
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub total_time: f64,
    pub prepare_time: f64,
    pub model_time: f64,
    pub layers: usize,
    pub max_z: f64,
    /// Per filament slot: mm, mm³, g.
    pub filament_mm: Vec<f64>,
    pub filament_mm3: Vec<f64>,
    pub filament_g: Vec<f64>,
    pub used: Vec<usize>,
    pub label_ids: Vec<u32>,
    pub first_layer_time: f64,
}

/// The one warning of a [`write`] that was told to stop.
pub const STOPPED: &str = "stopped";

const UNKNOWN_Z_LIFT: f64 = 1.0;
/// How far over everything printed the head parks while the print waits for items to be placed
/// (mm): clear of the print, within the print safety check's travel lift (3 mm). The park at
/// the far corner is what gives the person room.
const PAUSE_LIFT: f64 = 2.0;

pub struct GcodeOutput {
    pub text: String,
    pub timeline: Timeline,
    pub stats: Stats,
    pub warnings: Vec<String>,
}

struct Wipe {
    pts: Vec<Pt>,
    speed: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lift { Auto, Normal, Slope, Spiral }

struct LiftArea {
    grid: makepad_fab_slicer::grid::SegGrid<usize>,
    bbox: BBox,
}

struct Avoid {
    rings: Vec<Vec<Pt>>,
    edges: SegGrid<(u32, u32)>,
    inside: SegGrid<u32>,
    snap: f64,
}

/// Proper crossing: travel fraction and whether it leaves the edge's left side.
fn travel_crossing(a: Pt, b: Pt, c: Pt, d: Pt) -> Option<(f64, bool)> {
    let (ab, cd, ac) = (b.sub(a), d.sub(c), c.sub(a));
    let den = cross(ab, cd);
    if den == 0 { return None; }
    let (t, u) = (cross(ac, cd), cross(ac, ab));
    let inside = |v: i128| if den > 0 { v > 0 && v < den } else { v < 0 && v > den };
    (inside(t) && inside(u)).then(|| (t as f64 / den as f64, den > 0))
}

fn travel_crosses<T: Copy>(grid: &SegGrid<T>, a: Pt, b: Pt) -> bool {
    let mut hit = false;
    grid.visit_box(BBox::of(&[a, b]), |c, d, _| {
        if !hit && travel_crossing(a, b, c, d).is_some() { hit = true; }
    });
    hit
}

fn travel_length(a: (f64, f64), via: &[(f64, f64)], b: (f64, f64)) -> f64 {
    let (mut prev, mut length) = (a, 0.0);
    for p in via.iter().copied().chain(std::iter::once(b)) {
        length += (p.0 - prev.0).hypot(p.1 - prev.1);
        prev = p;
    }
    length
}

impl Avoid {
    fn new(layer: &Layer, widths: &[(f64, f64)], support: bool) -> Option<Self> {
        use makepad_fab_slicer::{clip::union_ex, offset::{offset_ex, Join}};
        let objects = || layer.objects.iter().filter(move |o| support || !o.support_only);
        let mut parts = union_ex(&objects().flat_map(|o| o.slices.iter().cloned()).collect::<Vec<_>>());
        parts.retain(|p| {
            let b = p.bbox();
            unscaled(b.max.x - b.min.x) * unscaled(b.max.y - b.min.y) >= 0.1
        });
        // One boundary serves every nozzle on this layer.
        let (width, outer) = objects().fold((0.0f64, 0.0f64), |(inner, outer), o| {
            (inner.max(widths[o.filament].0), outer.max(widths[o.filament].1))
        });
        let d = 1.5 * flow::spacing(width, layer.height);
        if parts.is_empty() || d <= 0.0 { return None; }
        let mut boundary = Vec::new();
        for mut p in parts {
            p.holes.retain(|h| {
                let b = h.bbox();
                (b.max.x - b.min.x).min(b.max.y - b.min.y) >= scaled(2.0)
            });
            let join = Join::Round(0.01 * SCALE);
            let mut inset = offset_ex(std::slice::from_ref(&p), -d * SCALE, join);
            if inset.is_empty() { inset = offset_ex(std::slice::from_ref(&p), -d * 0.5 * SCALE, join); }
            boundary.extend(inset);
        }
        if boundary.is_empty() { return None; }
        let rings: Vec<Vec<Pt>> = boundary.iter().flat_map(|p| p.rings().map(|r| r.pts.clone())).collect();
        let edges = SegGrid::new(rings.iter().enumerate().flat_map(|(r, pts)| {
            (0..pts.len()).map(move |e| (pts[e], pts[(e + 1) % pts.len()], (r as u32, e as u32)))
        }).collect());
        Some(Self { rings, edges, inside: SegGrid::of_regions(&boundary, |i| i as u32), snap: (d + outer) * SCALE })
    }

    fn snap(&self, p: Pt) -> Pt {
        if self.inside.contains_any(p) { return p; }
        let Some((dist, (r, e))) = self.edges.nearest(p, |key| key) else { return p };
        if dist > self.snap { return p; }
        let ring = &self.rings[r as usize];
        let (a, b) = (ring[e as usize], ring[(e as usize + 1) % ring.len()]);
        let ab = b.sub(a);
        let l2 = dot(ab, ab);
        if l2 == 0 { return a; }
        lerp(a, b, (dot(p.sub(a), ab) as f64 / l2 as f64).clamp(0.0, 1.0))
    }

    fn visible(&self, a: Pt, b: Pt) -> bool {
        !travel_crosses(&self.edges, a, b) && self.inside.contains_any(lerp(a, b, 0.5))
    }
}

/// Fx-style mixing for the memo's fixed integer keys; no string bytes are hashed.
#[derive(Default)]
struct MemoHasher(u64);

impl MemoHasher {
    fn add(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for MemoHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.add(byte as u64);
        }
    }
    fn write_u8(&mut self, value: u8) {
        self.add(value as u64);
    }
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
}

type Memo = std::collections::HashMap<(u8, usize, usize, usize), f64, std::hash::BuildHasherDefault<MemoHasher>>;

pub struct Writer<'a> {
    family: &'static family::Family,
    job: &'a Job,
    /// How the job's printer writes what standard G-code leaves out.
    firmware: Firmware,
    /// Lines recorded so far, including the pending chunk.
    lines: usize,
    /// Non-finite values formatted on the writer thread (outside move records).
    nonfinite: usize,
    /// The first line that is [`END_MARK`].
    end_line: Option<usize>,
    /// The line ranges of the printer's template blocks (no insertions inside them).
    blocks: Vec<(usize, usize)>,
    scratch: String,
    /// The template dialect of the job's family, read once.
    template_dialect: Dialect,
    /// Cache failures as well as parsed sources.
    templates: std::collections::HashMap<String, Result<Template, TemplateError>>,
    /// Built once from the immutable plate facts, before the first template render.
    start_vars: Option<std::collections::HashMap<String, TVar>>,
    /// Settings read per move, keyed by (kind, index, static key pointer, length).
    memo: std::cell::RefCell<Memo>,
    /// Each extruder's reach ([`Job::reach`]), read once: the spiral lift asks on every travel.
    reach: Vec<Option<((f64, f64), (f64, f64))>>,
    /// Where the lines go: the text and the time model beside it, or the time model alone.
    sink: Sink<'a>,
    chunk: Chunk,
    x: f64,
    y: f64,
    z: f64,
    /// Highest Z reached by an extrusion so far.
    printed_top: f64,
    xy_known: bool,
    /// The head height is known (false after the start sequence).
    z_known: bool,
    /// The next travel leads into a 3D path: a descent waits for the XY move.
    descend_after_xy: bool,
    /// The last path printed was a 3D ramp and the head is still at its end
    /// (no retraction or other move since): a next ramp may carry straight on.
    ramp_open: bool,
    retracted: f64,
    restart_extra: f64,
    feed: f64,
    accel: f64,
    role: Role,
    /// The experimental mode last tagged (`; FAB_MODE:`).
    mode: u8,
    /// A closed-in fill tagged (`; FAB_CORE: 1`) since the last `; FEATURE:`, which ends it.
    core: bool,
    width: f64,
    height: f64,
    /// The part fan as last written (`M106 S` value, 0..255, fractional).
    fan: f64,
    fan_aux: i32,
    /// The overhang/bridge fan of this layer (`M106 S` value), when it is on
    /// and above the layer's part fan.
    overhang_fan: Option<f64>,
    /// The overhang fan is on for the path being printed.
    overhang_fan_on: bool,
    /// Plate layer for geometry and progress.
    layer_index: usize,
    /// Layer within the object, or the plate in layer mode.
    object_layer: usize,
    object_layer_start: usize,
    by_object: bool,
    /// Highest print Z reached.
    sequential_z: f64,
    /// Estimated seconds on this layer, for cooling.
    layer_time: f64,
    /// The layer's part outlines: travels from fill to fill that stay
    /// inside need no retraction.
    inner: Option<makepad_fab_slicer::grid::SegGrid<u32>>,
    /// Overhangs and supports, indexed by merged layer.
    lift_areas: Vec<(f64, Option<LiftArea>)>,
    avoid_on: bool,
    avoid: Vec<Option<Avoid>>,
    /// Skip one travel after a moving template.
    avoid_skip: bool,
    filament: usize,
    /// Parsed once; omitted fields carry the previous calibration value.
    cal_bands: Vec<cal_bands::Band>,
    cal_active: Option<usize>,
    cal_flow: f64,
    cal_temp: Option<f64>,
    cal_pa: Option<f64>,
    wipe: Option<Wipe>,
    last_role: Role,
    errors: Vec<String>,
    label_ids: Vec<u32>,
    object_names: Vec<String>,
    open_label: Option<usize>,
    tower: Option<Tower>,
    skirt: Option<Skirt>,
    /// This layer's step up from the last one (the tower's bead height).
    tower_height: f64,
    /// Per object: its own accelerations, when it sets any.
    object_accels: Vec<Option<ObjectAccels>>,
    /// The object being printed.
    accel_object: usize,
    tl: Option<TowerLayer>,
    /// Printing a tower layer that has no change: the grid at the wall's
    /// speed, moves inside the tower without retracting.
    tower_empty: bool,
    /// This layer's changes (from, to) and how many are done.
    layer_changes: Vec<(usize, usize)>,
    change_idx: usize,
    /// The filament sitting in each physical extruder.
    loaded: Vec<Option<usize>>,
    /// The filament each extruder has drawn back since it last printed (mm).
    drawn_back: Vec<f64>,
    /// Each parked head's push-back, including its restart extra; zero before its first park.
    parked_prime: Vec<f64>,
    /// Without a tower, restore the speed factor after the incoming head's push-back.
    restore_change_speed: bool,
    change_speed_guarded: bool,
    toolchanges: usize,
    changes_left: usize,
    layer_fans: (f64, i32),
    retract_override: Option<f64>,
    wipe_speed_override: Option<f64>,
    next_filament_hint: usize,
    /// What each filament change purges.
    changes: makepad_fab_settings::change::ChangeModel,
    /// Per layer, in change order: what of each change's purge prints into what doesn't show
    /// (the filament changed to, mm³), by the flush options. The printer purges the rest.
    flush_into: Vec<Vec<(usize, f64)>>,
    /// Arc fits made ahead, in parallel, per planned path (address, reversed, seat):
    /// the same `arcfit::fit_arcs` the writer would make in line.
    fits: std::collections::HashMap<(usize, bool, Option<u32>), Vec<crate::arcfit::Seg>>,
    /// The planned path the next `extrude` prints, for its fit.
    fit_key: Option<(usize, bool, Option<u32>)>,
}

/// A closed non-planar ring started at the point nearest `from` among its low points (within
/// 0.05 mm of its lowest), so the hop from the previous ring stays short and low.
fn seam_near(path: &ExtrusionPath, from: (f64, f64)) -> ExtrusionPath {
    let n = path.pts.len();
    let closed = n > 2 && path.pts[0] == path.pts[n - 1];
    if !closed { return path.clone(); }
    let zmin = path.z[..n - 1].iter().fold(f64::MAX, |a, z| a.min(*z));
    let k = (0..n - 1).filter(|&i| path.z[i] <= zmin + 0.05).min_by(|&a, &b| {
        let d = |i: usize| (path.pts[i].x_mm() - from.0).hypot(path.pts[i].y_mm() - from.1);
        d(a).total_cmp(&d(b))
    }).unwrap_or(0);
    let mut out = path.clone();
    let mut pts: Vec<Pt> = path.pts[..n - 1].to_vec();
    let mut z: Vec<f64> = path.z[..n - 1].to_vec();
    pts.rotate_left(k);
    z.rotate_left(k);
    pts.push(pts[0]);
    z.push(z[0]);
    if !path.flow.is_empty() {
        let mut f = path.flow.clone();
        let r = k.min(f.len());
        f.rotate_left(r);
        out.flow = f;
    }
    out.pts = pts;
    out.z = z;
    out
}

/// The bead width of segment `i` of a variable-width path (its flow scales
/// the cross-section of a bead `h` tall); None for a path of one width and
/// for a scarf or a vase, whose flow also fills a wedge under the bead.
fn segment_width(path: &ExtrusionPath, i: usize, h: f64) -> Option<f64> {
    use makepad_fab_toolpath::exp::mode;
    if path.flow.is_empty() || path.mode == mode::SCARF || path.mode == mode::VASE || h <= 0.0 {
        return None;
    }
    let f = path.flow.get(i).or(path.flow.last()).copied()?.max(0.0);
    Some(flow::width_for_spacing(f * path.area / h, h))
}

/// Where the writer's lines go.
enum Sink<'a> {
    /// The G-code text, read by the time model beside the writer ([`write`]).
    Text(&'a Pipe),
    /// The time model alone, in line: moves go to it as numbers, never formatted or read
    /// back; other lines as their text ([`estimate`]).
    Model(Box<Processor>),
}

impl<'a> Writer<'a> {
    fn new(job: &'a Job, family: &'static family::Family, sink: Sink<'a>) -> Self {
        let mut errors = Vec::new();
        let cal_bands = match cal_bands::parse(job.s("fab_calibration_bands")) {
            Ok(bands) => bands,
            Err(e) => { errors.push(e); Vec::new() }
        };
        Self {
            job,
            family,
            firmware: Firmware::of(family),
            lines: 0,
            nonfinite: 0,
            end_line: None,
            blocks: Vec::new(),
            scratch: String::with_capacity(64),
            template_dialect: family.templates,
            templates: std::collections::HashMap::new(),
            start_vars: None,
            memo: std::cell::RefCell::new(Memo::default()),
            reach: Vec::new(),
            sink,
            chunk: Chunk::default(),
            x: 0.0,
            y: 0.0,
            z: 0.0,
            printed_top: 0.0,
            xy_known: false,
            z_known: false,
            descend_after_xy: false,
            ramp_open: false,
            retracted: 0.0,
            restart_extra: 0.0,
            feed: 0.0,
            accel: 0.0,
            role: Role::None,
            mode: 0,
            core: false,
            width: 0.0,
            height: 0.0,
            fan: -1.0,
            fan_aux: -1,
            overhang_fan: None,
            overhang_fan_on: false,
            layer_index: 0,
            object_layer: 0,
            object_layer_start: 0,
            by_object: false,
            sequential_z: 0.0,
            layer_time: 0.0,
            inner: None,
            lift_areas: Vec::new(),
            avoid_on: job.b("reduce_crossing_wall"),
            avoid: Vec::new(),
            avoid_skip: false,
            filament: 0,
            cal_bands,
            cal_active: None,
            cal_flow: 1.0,
            cal_temp: None,
            cal_pa: None,
            wipe: None,
            last_role: Role::None,
            errors,
            label_ids: Vec::new(),
            object_names: Vec::new(),
            open_label: None,
            tower: None,
            skirt: None,
            tower_height: 0.0,
            object_accels: Vec::new(),
            accel_object: 0,
            tl: None,
            tower_empty: false,
            layer_changes: Vec::new(),
            change_idx: 0,
            loaded: Vec::new(),
            drawn_back: Vec::new(),
            parked_prime: Vec::new(),
            restore_change_speed: false,
            change_speed_guarded: false,
            toolchanges: 0,
            changes_left: 0,
            layer_fans: (0.0, 0),
            retract_override: None,
            wipe_speed_override: None,
            next_filament_hint: 0,
            changes: makepad_fab_settings::change::ChangeModel::new(job),
            flush_into: Vec::new(),
            fits: std::collections::HashMap::new(),
            fit_key: None,
        }
    }

    /// Hand a text line to the time model (in chunks of whole lines).
    fn feed(&mut self, l: &str) {
        let start = self.chunk.text.len();
        self.chunk.text.push_str(l);
        self.chunk.lines.push(Line::Text(start..self.chunk.text.len()));
        self.recorded_line();
    }

    fn recorded_line(&mut self) {
        self.lines += 1;
        if self.chunk.lines.len() >= CHUNK_LINES {
            if let Sink::Text(pipe) = &self.sink {
                pipe.send(std::mem::take(&mut self.chunk), false);
            }
        }
    }

    /// A setting through the memo (the job never changes while writing).
    fn memo(&self, kind: u8, key: &'static str, i: usize, read: impl FnOnce() -> f64) -> f64 {
        // Equal text at different static addresses may cache twice; its read is identical.
        let key = (kind, i, key.as_ptr() as usize, key.len());
        if let Some(&value) = self.memo.borrow().get(&key) {
            return value;
        }
        let v = read();
        self.memo.borrow_mut().insert(key, v);
        v
    }
    fn eff_m(&self, key: &'static str, f: usize) -> f64 {
        self.memo(0, key, f, || self.job.eff(key, f))
    }
    fn ext_m(&self, key: &'static str, e: usize) -> f64 {
        self.memo(1, key, e, || self.job.ext(key, e))
    }
    fn fil_m(&self, key: &'static str, f: usize) -> f64 {
        self.memo(2, key, f, || self.job.fil(key, f))
    }
    fn f_m(&self, key: &'static str) -> f64 {
        self.memo(3, key, 0, || self.job.f(key))
    }
    fn b_m(&self, key: &'static str) -> bool {
        self.memo(4, key, 0, || if self.job.b(key) { 1.0 } else { 0.0 }) != 0.0
    }
    fn lift_type(&self, f: usize) -> Lift {
        match self.memo(9, "z_hop_types", f, || match self.job.eff_str("z_hop_types", f).as_str() {
            "Normal Lift" => 1.0, "Slope Lift" => 2.0, "Spiral Lift" => 3.0, _ => 0.0,
        }) as u8 {
            1 => Lift::Normal, 2 => Lift::Slope, 3 => Lift::Spiral, _ => Lift::Auto,
        }
    }

    /// One command line `head W<v> W<v> ...`, each value with its decimals
    /// trimmed as [`fmt::trim`] does, built without temporary strings.
    fn push_words(&mut self, head: &'static str, words: &[(&'static str, f64, usize)]) {
        if words.iter().any(|&(k, v, _)| k == "E" && v > 0.0) {
            let top = words.iter().filter(|w| w.0 == "Z").fold(self.z, |t, w| t.max(w.1));
            self.printed_top = self.printed_top.max(top);
        }
        if let Sink::Model(model) = &mut self.sink {
            let motion = match head {
                "G0" | "G1" => Some((false, false)),
                "G2" => Some((true, true)),
                "G3" => Some((true, false)),
                _ => None,
            };
            if let Some((arc, cw)) = motion {
                model.op(&process::Op::Move { arc, cw, w: process::Words::of_values(words) }, "");
                return;
            }
        }
        if matches!(self.sink, Sink::Text(_)) {
            let start = self.chunk.words.len();
            self.chunk.words.extend_from_slice(words);
            self.chunk.lines.push(Line::Words { head, words: start..self.chunk.words.len() });
            self.recorded_line();
            return;
        }
        let mut l = std::mem::take(&mut self.scratch);
        l.clear();
        l.push_str(head);
        for &(k, v, d) in words {
            l.push(' ');
            l.push_str(k);
            fmt::trim_into(&mut l, v, d);
        }
        if let Sink::Model(model) = &mut self.sink {
            model.line(&l);
        }
        self.scratch = l;
    }

    /// A `; NAME: value` tag line, its value as [`fmt::tag_into`] writes it.
    fn push_tag(&mut self, head: &str, v: f64) {
        let mut l = std::mem::take(&mut self.scratch);
        l.clear();
        l.push_str(head);
        fmt::tag_into(&mut l, v);
        self.push(&l);
        self.scratch = l;
    }

    /// A `; FEATURE:` line: the role the moves after it print, and the end of a closed-in fill's
    /// tag (`; FAB_CORE:`), as the time model reads it.
    fn feature(&mut self, role: Role) {
        self.push(&format!("; FEATURE: {}", role.name()));
        self.role = role;
        self.core = false;
    }

    fn push(&mut self, s: &str) {
        for l in s.split('\n') {
            if let Sink::Model(model) = &mut self.sink {
                model.line(l);
                continue;
            }
            if self.end_line.is_none() && l == END_MARK {
                self.end_line = Some(self.lines);
            }
            self.feed(l);
        }
    }

    /// An extruding arc from the head's written position to `end` around `center` (I J from the
    /// position the printer has, as the fit checked them).
    fn push_arc(&mut self, end: [f64; 2], center: [f64; 2], ccw: bool, e: f64) {
        let start = [crate::arcfit::quantize(self.x), crate::arcfit::quantize(self.y)];
        let (_, ij) = crate::arcfit::arc_words(start, end, center);
        self.push_words(if ccw { "G3" } else { "G2" }, &[("X", end[0], 3), ("Y", end[1], 3), ("I", ij[0], 3), ("J", ij[1], 3), ("E", e, 5)]);
    }

    /// Whether a printer sequence may leave the head where the writer does not know it.
    fn sequence_moves(&self, src: &str) -> bool {
        template_moves(src) || self.firmware.0 == family::Flavor::Named && src.lines().any(named_line_moves)
    }

    /// Append template output (which ends with its own newline). In the second family the
    /// writer's own modes follow a sequence that left others.
    fn push_block(&mut self, s: &str) {
        self.push_end_block(s);
        self.restore_modes(s);
    }

    /// Append the last printer sequence: nothing of the writer's may follow it.
    fn push_end_block(&mut self, s: &str) {
        let s = s.strip_suffix('\n').unwrap_or(s);
        if s.is_empty() {
            return;
        }
        if self.avoid_on && self.sequence_moves(s) { self.avoid_skip = true; }
        let first = self.lines;
        self.push(s);
        self.blocks.push((first, self.lines));
    }

    /// The second family: the writer's own moves are absolute, with relative extrusion. A
    /// sequence that leaves relative moves or absolute extrusion is followed by the writer's
    /// modes again, so its next move means what it says.
    fn restore_modes(&mut self, s: &str) {
        if self.firmware.0 != family::Flavor::Named { return; }
        let (mut relative, mut absolute_e) = (false, false);
        for line in s.lines() {
            match line.split(';').next().unwrap_or("").split_whitespace().next().map(str::to_ascii_uppercase).as_deref() {
                Some("G90") => relative = false,
                Some("G91") => relative = true,
                Some("M82") => absolute_e = true,
                Some("M83") => absolute_e = false,
                _ => {}
            }
        }
        if relative { self.push("G90"); }
        if absolute_e { self.push("M83"); }
    }

    /// Seed `position` as a writable global for templates. Its changes are not read back:
    /// after a change sequence or a moving template, `xy_known`/`z_known` make the next
    /// move use an explicit position.
    fn render(&mut self, name: &str, src: &str, setup: impl FnOnce(&mut Env)) -> String {
        let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeTemplates);
        let cfg = &self.job.cfg;
        let mut env = Env::new(cfg, self.template_dialect);
        env.base = Some(self.start_vars.as_ref().expect("plate start variables must precede template rendering"));
        env.current_extruder_id = self.filament;
        env.globals.insert("position".into(), TVar::f64s(&[self.x, self.y, self.z]));
        setup(&mut env);
        if !self.templates.contains_key(src) {
            self.templates.insert(src.to_owned(), template::parse(src, self.template_dialect));
        }
        let rendered = match &self.templates[src] {
            Ok(t) => env.render_parsed(t).map_err(|mut e| {
                // Env::render locates evaluation errors in the original source too.
                e.line = src[..e.pos.min(src.len())].matches('\n').count() + 1;
                e
            }),
            Err(e) => Err(e.clone()),
        };
        match rendered {
            Ok(s) => s,
            Err(e) => {
                self.errors.push(format!("{name}: {e}"));
                String::new()
            }
        }
    }

    /// Set the active filament's temperature using its firmware tool number.
    fn nozzle_temperature(&mut self, temp: f64) {
        let tool = self.firmware.tool(self.job, self.filament);
        let line = self.firmware.nozzle_temperature(tool, temp);
        self.push(&line);
    }

    /// Restore calibration PA, or the active filament's PA when enabled.
    fn pressure_advance(&mut self) {
        let pa = self.cal_pa.or_else(|| {
            self.job.cfg.bool_at("enable_pressure_advance", self.filament)
                .then(|| self.job.fil("pressure_advance", self.filament))
        });
        if let Some(pa) = pa {
            let line = self.firmware.pressure_advance(pa);
            self.push(&line);
        }
    }

    /// Apply newly crossed bands at print Z, carrying fields even when a
    /// layer steps over more than one band. Travel lifts never select bands.
    fn calibration_layer(&mut self, print_z: f64) {
        let active = self.cal_bands.iter().rposition(|b| b.z_from <= print_z + 1e-6);
        if active == self.cal_active {
            return;
        }
        let Some(active) = active else { return };
        let mut temp = None;
        let mut pa = None;
        for band in self.cal_bands.iter().take(active + 1).skip(self.cal_active.map_or(0, |i| i + 1)) {
            if let Some(flow) = band.flow {
                self.cal_flow = flow;
            }
            temp = band.temp.or(temp);
            pa = band.pa.or(pa);
        }
        self.cal_active = Some(active);
        if let Some(temp) = temp {
            self.cal_temp = Some(temp);
            self.nozzle_temperature(temp);
        }
        if let Some(pa) = pa {
            self.cal_pa = Some(pa);
            let line = self.firmware.pressure_advance(pa);
            self.push(&line);
        }
    }

    fn set_accel(&mut self, a: f64) {
        if a <= 0.0 || (a - self.accel).abs() <= 1e-9 {
            return;
        }
        let factor = self.b_m("accel_to_decel_enable").then(|| self.f_m("accel_to_decel_factor"));
        let line_of = |a: f64| self.firmware.acceleration(a, factor.map(|f| a * f / 100.0));
        let line = line_of(a);
        // A value that writes the line already in force changes nothing.
        if self.accel > 0.0 && line == line_of(self.accel) {
            return;
        }
        self.push(&line);
        self.accel = a;
    }

    fn set_feed_line(&mut self, f: f64) {
        if (f - self.feed).abs() > 1e-6 {
            self.push_words("G1", &[("F", f, 3)]);
            self.feed = f;
        }
    }

    fn retract_len(&self) -> f64 {
        self.retract_override.unwrap_or_else(|| self.eff_m("retraction_length", self.filament))
    }

    fn retract_speed(&self) -> f64 {
        self.eff_m("retraction_speed", self.filament)
    }

    fn deretract_speed(&self) -> f64 {
        let v = self.eff_m("deretraction_speed", self.filament);
        if v > 0.0 {
            v
        } else {
            self.retract_speed()
        }
    }

    /// Retract, wiping along the last path first when wiping is on.
    fn retract(&mut self, first_layer: bool) {
        self.ramp_open = false;
        let len = self.retract_len();
        if self.retracted > 0.0 || len <= 0.0 {
            return;
        }
        let mut done = 0.0;
        let f = self.filament;
        if self.memo(7, "wipe", f, || if self.job.eff_bool("wipe", f) { 1.0 } else { 0.0 }) != 0.0 {
            if let Some(w) = self.wipe.take() {
                let dist = self.eff_m("wipe_distance", self.filament) * SCALE;
                let e = self.job.ext_of(self.filament);
                let before = self.memo(8, "retract_before_wipe", e, || self.job.cfg.f64_at("retract_before_wipe", e)) / 100.0;
                if self.b_m("fab_wipe_retract_at_retraction_speed") {
                    self.wipe_at_retraction_speed(&w, len, before, dist);
                    done = len;
                } else {
                // The wipe carries most of the retraction; a small rest is
                // done in place after it.
                let wipe_e = len * (1.0 - before) * 0.95;
                if before > 0.0 {
                    let e0 = len * before;
                    let rs = self.retract_speed() * 60.0;
                    self.push_words("G1", &[("E-", e0.abs(), 5), ("F", rs, 3)]);
                    done += e0;
                }
                // Walk the wipe path for `dist`.
                let mut segs = Vec::new();
                let mut left = dist;
                let mut cur = Pt::from_mm(self.x, self.y);
                for &p in &w.pts {
                    if left <= 0.0 {
                        break;
                    }
                    let d = cur.dist(p);
                    if d <= 0.0 {
                        continue;
                    }
                    if d >= left {
                        let q = lerp(cur, p, left / d);
                        segs.push((q, left));
                        break;
                    }
                    segs.push((p, d));
                    left -= d;
                    cur = p;
                }
                let walked: f64 = segs.iter().map(|s| s.1).sum();
                if walked > 0.0 {
                    self.push("; WIPE_START");
                    let speed = self.wipe_speed_override.unwrap_or_else(|| {
                        if self.b_m("role_base_wipe_speed") && w.speed > 0.0 { w.speed } else { self.travel_speed() / 60.0 * self.f_m("wipe_speed") / 100.0 }
                    });
                    self.set_feed_line(speed * 60.0);
                    let acc = if first_layer { self.ext_m("initial_layer_acceleration", self.job.ext_of(self.filament)) } else { self.ext_m("default_acceleration", self.job.ext_of(self.filament)) };
                    self.set_accel(acc);
                    let we = wipe_e * (walked / dist).min(1.0);
                    for (p, d) in &segs {
                        let e = we * d / walked;
                        self.push_words("G1", &[("X", p.x_mm(), 3), ("Y", p.y_mm(), 3), ("E-", e, 5)]);
                        self.x = p.x_mm();
                        self.y = p.y_mm();
                    }
                    self.push("; WIPE_END");
                    done += we;
                }
                }
            }
        }
        let rest = len - done;
        if rest > 1e-6 {
            let rs = self.retract_speed() * 60.0;
            self.push_words("G1", &[("E-", rest, 5), ("F", rs, 3)]);
            self.feed = self.retract_speed() * 60.0;
        }
        self.retracted = len;
        self.restart_extra = self.eff_m("retract_restart_extra", f);
        self.wipe = None;
    }

    /// The wipe of a printer whose presets come from the other slicer: it draws back only what
    /// the extruder can at its retraction speed while the wipe moves, and at most what
    /// `retract_before_wipe` leaves it. The rest of `len` is retracted in place before the wipe,
    /// which runs at the acceleration in force (a travel sets its own before it retracts).
    fn wipe_at_retraction_speed(&mut self, w: &Wipe, len: f64, before: f64, dist: f64) {
        let mut segs = Vec::new();
        let mut left = dist;
        let mut cur = Pt::from_mm(self.x, self.y);
        for &p in &w.pts {
            if left <= 0.0 {
                break;
            }
            let d = cur.dist(p);
            if d <= 0.0 {
                continue;
            }
            if d >= left {
                segs.push((lerp(cur, p, left / d), left));
                break;
            }
            segs.push((p, d));
            left -= d;
            cur = p;
        }
        let walked: f64 = segs.iter().map(|s| s.1).sum();
        let speed = self.wipe_speed_override.unwrap_or_else(|| {
            if self.b_m("role_base_wipe_speed") && w.speed > 0.0 {
                w.speed
            } else {
                let travel = self.travel_speed() / 60.0;
                let e = self.job.ext_of(self.filament);
                self.job.cfg.abs_or_pct("wipe_speed", e, travel)
            }
        }).max(10.0);
        let rs = self.retract_speed();
        let can = if walked > 0.0 { rs * (walked / SCALE) / speed } else { 0.0 };
        let during = if can > 1e-9 { can.min(len * (1.0 - before)) } else { 0.0 };
        let in_place = len - during;
        if in_place > 1e-6 {
            self.push_words("G1", &[("E-", in_place, 5), ("F", rs * 60.0, 3)]);
            self.feed = rs * 60.0;
        }
        if walked > 0.0 {
            self.push("; WIPE_START");
            self.set_feed_line(speed * 60.0);
            for (p, d) in &segs {
                if during > 0.0 {
                    self.push_words("G1", &[("X", p.x_mm(), 3), ("Y", p.y_mm(), 3), ("E-", during * d / walked, 5)]);
                } else {
                    self.push_words("G1", &[("X", p.x_mm(), 3), ("Y", p.y_mm(), 3)]);
                }
                self.x = p.x_mm();
                self.y = p.y_mm();
            }
            self.push("; WIPE_END");
        }
    }

    fn unretract(&mut self) {
        if self.retracted > 0.0 {
            let f = self.deretract_speed() * 60.0;
            let r = self.retracted + self.restart_extra;
            if r > 0.0 {
                self.push_words("G1", &[("E", r, 5), ("F", f, 3)]);
                self.feed = f;
            }
        }
        self.retracted = 0.0;
        self.restart_extra = 0.0;
        if std::mem::take(&mut self.restore_change_speed) {
            self.end_change_speed_guard();
        }
    }

    fn travel_accel(&self, first_layer: bool) -> f64 {
        let e = self.job.ext_of(self.filament);
        if first_layer {
            self.ext_m("initial_layer_travel_acceleration", e)
        } else {
            self.ext_m("travel_acceleration", e)
        }
    }

    fn travel_speed(&self) -> f64 {
        self.ext_m("travel_speed", self.job.ext_of(self.filament)) * 60.0
    }

    /// The print waits for items to be placed into their parts before the layer `layer` (0-based,
    /// at `lz`) that covers them: `names`, `near` their centre on the bed. The nozzle draws back,
    /// rises [`PAUSE_LIFT`] over everything printed (within the printer's height) and parks at the
    /// corner of its reach farthest from them; `pause`, the printer's own, waits for the person.
    /// On resume the bed and the nozzle come back to `bed` and `temp` before anything moves, and
    /// the head's place is unknown (the printer's pause may have moved it): the next travel goes
    /// over the print to the layer, and its unretract primes the nozzle.
    fn pause_for(&mut self, names: &[&str], near: (f64, f64), layer: usize, lz: f64, pause: &str, temp: f64, bed: f64) {
        self.retract(false);
        self.ramp_open = false;
        self.wipe = None;
        let ((x0, y0), (x1, y1)) = self.job.reach(self.job.ext_of(self.filament));
        let far = |p: &(f64, f64)| (p.0 - near.0).hypot(p.1 - near.1);
        let park = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].into_iter().filter(|p| p.0.is_finite() && p.1.is_finite()).max_by(|a, b| far(a).total_cmp(&far(b)));
        let height = self.job.cfg.f64("printable_height");
        let over = self.printed_top.max(if self.z_known { self.z } else { 0.0 });
        let z = if height > over { (over + PAUSE_LIFT).min(height) } else { over + PAUSE_LIFT };
        let feed = self.lift_feed();
        self.push(&format!("G1 Z{} F{}", fmt::xyz(z), fmt::f(feed)));
        self.z = z;
        self.z_known = true;
        self.feed = feed;
        if let Some((px, py)) = park {
            let tf = self.travel_speed();
            self.push(&format!("G1 X{} Y{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::f(tf)));
            (self.x, self.y, self.feed) = (px, py, tf);
        }
        let what = names.join(", ");
        self.push(&format!("; FAB_PAUSE: layer {} z {}: place {what}", layer + 1, (lz * 1000.0).round() / 1000.0));
        self.push_block(pause);
        if bed > 0.0 {
            self.push(&format!("M190 S{}", bed.round()));
        }
        let line = self.firmware.wait_nozzle_temperature(self.firmware.tool(self.job, self.filament), temp);
        self.push(&line);
        self.xy_known = false;
        self.z_known = false;
    }

    /// After a printer sequence the head's height is unknown: before any XY move, lift to an absolute height
    /// above everything printed and the layer about to print, by at least UNKNOWN_Z_LIFT (the H2D start sequence
    /// extrudes its purge line 0.8 mm off the plate and lifts 0.4 mm above it).
    fn lift_if_z_unknown(&mut self, lz: f64) {
        if self.z_known { return; }
        let z = self.printed_top.max(lz) + self.eff_m("z_hop", self.filament).max(UNKNOWN_Z_LIFT);
        self.push(&format!("G1 Z{}", fmt::xyz(z)));
        self.z = z;
        self.z_known = true;
    }

    fn path_speed(&self, path: &ExtrusionPath, f: usize, first: bool) -> f64 {
        if !self.by_object || !first { return path.speed; }
        let key = if matches!(path.role, Role::BottomSurface | Role::InternalSolidInfill | Role::SparseInfill) { "initial_layer_infill_speed" } else { "initial_layer_speed" };
        let speed = self.ext_m(key, self.job.ext_of(f));
        if speed > 0.0 { path.speed.min(speed) } else { path.speed }
    }

    fn lift_feed(&self) -> f64 {
        let v = self.ext_m("travel_speed_z", self.job.ext_of(self.filament));
        if v > 0.0 { v * 60.0 } else { self.travel_speed() }
    }

    fn lift_allowed(&self, z: f64) -> bool {
        let below = self.eff_m("retract_lift_below", self.filament);
        self.retracted > 0.0 && z >= self.eff_m("retract_lift_above", self.filament) && (below <= 0.0 || z <= below)
    }

    /// Cross to an object above all printed paths.
    fn object_travel(&mut self, p: Pt, first: bool) {
        let hop = self.eff_m("z_hop", self.filament);
        let safe = self.sequential_z + if hop > 0.0 { hop } else { 0.4 };
        self.set_accel(self.travel_accel(first));
        if !self.z_known || self.z < safe {
            let feed = self.lift_feed();
            self.push(&format!("G1 Z{} F{}", fmt::xyz(safe), fmt::f(feed)));
            self.z = safe;
            self.z_known = true;
            self.feed = feed;
        }
        let feed = self.travel_speed();
        self.push(&format!("G1 X{} Y{} F{}", fmt::xyz(p.x_mm()), fmt::xyz(p.y_mm()), fmt::f(feed)));
        self.x = p.x_mm();
        self.y = p.y_mm();
        self.xy_known = true;
        self.feed = feed;
        self.ramp_open = false;
    }

    /// Rise by `hop` on a circle of the radius `arc` asks for, trying centres in order, falling
    /// back to a vertical lift.
    /// Never from an unknown position: after a printer sequence the next travel lifts to a safe height first.
    fn lift_arc(&mut self, hop: f64, arc: f64, feed: f64, centres: &[(f64, f64)]) {
        if !self.z_known || !self.xy_known {
            return;
        }
        let r = arc / (std::f64::consts::TAU * 3.0f64.to_radians().atan());
        let (job, e) = (self.job, self.job.ext_of(self.filament));
        if self.reach.len() <= e {
            self.reach.resize(e + 1, None);
        }
        let ((x0, y0), (x1, y1)) = *self.reach[e].get_or_insert_with(|| job.reach(e));
        let centre = centres.iter().map(|&(i, j)| (i * r, j * r)).find(|&(i, j)| {
            let (cx, cy) = (self.x + i, self.y + j);
            cx - r >= x0 && cx + r <= x1 && cy - r >= y0 && cy + r <= y1
        });
        let z = self.z + hop;
        if let Some(lines) = centre.and_then(|(i, j)| self.firmware.spiral_lift(z, i, j, feed)) {
            for line in lines {
                self.push(&line);
            }
        } else {
            self.push(&format!("G1 Z{} F{}", fmt::xyz(z), fmt::f(feed)));
        }
        self.feed = feed;
        self.z = z;
    }

    /// Lift beside the travel, trying its left side first.
    /// The spiral's radius follows `z_hop` alone, whatever the rise.
    fn spiral_lift(&mut self, to: (f64, f64), hop: f64) {
        let (dx, dy) = (to.0 - self.x, to.1 - self.y);
        let l = dx.hypot(dy);
        let (ux, uy) = if l > 1e-9 { (dx / l, dy / l) } else { (1.0, 0.0) };
        let arc = self.eff_m("z_hop", self.filament);
        self.lift_arc(hop, arc, self.lift_feed(), &[(-uy, ux), (uy, -ux)]);
    }

    /// Lift before a move whose direction is not known yet.
    fn lift_eager(&mut self, hop: f64, feed: f64, normal: bool) {
        self.lift_arc(hop, hop, feed, if normal { &[] } else { &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] });
    }

    /// Move to `p` on layer height `lz`.
    /// A non-planar block (NONPLANAR-DESIGN §3): its rings in order, each from a low point
    /// next to the nozzle. A hop of up to 1 mm lifts z_hop over both ends (a 20 deg surface
    /// rises less than that over 1 mm); a longer one lifts z_hop over everything the block
    /// has printed. The feed of a ring keeps the machine's Z jerk at its sharpest change of
    /// pitch (the writer's 3D extrusion already keeps the Z speed).
    fn emit_np_block(&mut self, b: &NpBlock, first_layer: bool) {
        let id = ((b.object.min(255) as u16) << 8) | ((b.region + 1).min(255) as u16);
        self.push(&format!("; BLOCK: {id}"));
        let hop = self.eff_m("z_hop", self.filament).max(0.2);
        let jerk = { let j = self.f_m("machine_max_jerk_z"); if j > 0.0 { j } else { 3.0 } };
        let tf = self.travel_speed();
        let mut top = self.z;
        for path in &b.paths {
            if path.pts.len() < 2 || path.z.len() != path.pts.len() { continue; }
            let ring = seam_near(path, (self.x, self.y));
            let start = ring.pts[0];
            let (sx, sy, sz) = (start.x_mm(), start.y_mm(), b.z + ring.z[0]);
            self.lift_if_z_unknown(sz);
            let dist = (sx - self.x).hypot(sy - self.y);
            if self.retracted <= 0.0 && dist > 0.05 {
                if self.b_m("fab_wipe_retract_at_retraction_speed") {
                    let a = self.travel_accel(first_layer);
                    self.set_accel(a);
                }
                self.retract(first_layer);
            }
            let safe = if dist <= 1.0 { self.z.max(sz) + hop } else { top.max(self.z).max(sz) + hop };
            let a = self.travel_accel(first_layer);
            self.set_accel(a);
            if self.z < safe - 1e-9 {
                self.push(&format!("G1 Z{} F{}", fmt::xyz(safe), fmt::f(tf)));
                self.z = safe;
            }
            self.push(&format!("G1 X{} Y{} F{}", fmt::xyz(sx), fmt::xyz(sy), fmt::f(tf)));
            self.push_words("G1", &[("Z", sz, 3)]);
            self.feed = tf;
            self.x = sx;
            self.y = sy;
            self.z = sz;
            self.xy_known = true;
            self.z_known = true;
            self.unretract();
            // The sharpest change of the Z share of the speed along the ring.
            // On the words as written (3 decimals), as the printer and the print check see them.
            let q = |v: f64| (v * 1000.0).round() / 1000.0;
            let sins: Vec<f64> = ring.pts.windows(2).zip(ring.z.windows(2)).map(|(p, z)| {
                let (dx, dy) = (q(p[1].x_mm()) - q(p[0].x_mm()), q(p[1].y_mm()) - q(p[0].y_mm()));
                let (lxy, dz) = (dx.hypot(dy), q(b.z + z[1]) - q(b.z + z[0]));
                let l = (lxy * lxy + dz * dz).sqrt();
                if l > 1e-9 { dz / l } else { 0.0 }
            }).collect();
            let dsin = sins.windows(2).map(|w| (w[1] - w[0]).abs()).chain(sins.first().map(|s| s.abs())).fold(0.0f64, f64::max);
            let speed = self.path_speed(&ring, self.filament, first_layer);
            let speed = if dsin > 1e-9 { speed.min(0.9 * jerk / dsin) } else { speed };
            let speed = ((speed * 60.0).floor() / 60.0).max(1.0 / 60.0);
            let accel = self.accel_for(ring.role, first_layer);
            self.extrude(&ring, speed, accel, b.z, first_layer, ring.height);
            top = top.max(ring.z.iter().fold(f64::MIN, |a, z| a.max(b.z + z)));
        }
        self.push("; BLOCK_END");
    }

    fn travel_to(&mut self, p: Pt, lz: f64, first_layer: bool, layer_start: bool, next_role: Role) {
        let (px, py) = (p.x_mm(), p.y_mm());
        let tf = self.travel_speed();
        let avoid_skip = self.avoid_on && std::mem::take(&mut self.avoid_skip);
        if !self.xy_known || !self.z_known {
            // XY unknown (after the start sequence, a change or a photo): go
            // there above the print, then come down.
            let hop = self.eff_m("z_hop", self.filament);
            self.lift_if_z_unknown(lz);
            if self.z_known && self.z < lz + hop - 1e-6 {
                self.push(&format!("G1 Z{}", fmt::xyz(lz + hop)));
                self.z = lz + hop;
            }
            self.push(&format!("G1 X{} Y{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::f(tf)));
            self.feed = tf;
            self.x = px;
            self.y = py;
            self.xy_known = true;
            let a = self.travel_accel(first_layer);
            self.set_accel(a);
            // Come down onto the layer from a safe height.
            self.push_words("G1", &[("Z", lz, 3)]);
            self.z = lz;
            self.z_known = true;
            self.unretract();
            return;
        }
        let dist = ((px - self.x).powi(2) + (py - self.y).powi(2)).sqrt();
        // A ramp carries straight on from where the last one ended (a vase's
        // next revolution, the step to its inner wall): no retraction, no lift.
        if self.descend_after_xy && self.ramp_open && self.retracted <= 0.0 && dist < 2.0 && lz >= self.z - 1e-9 {
            if dist > 1e-9 && (lz - self.z).abs() > 1e-7 {
                self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("Z", lz, 3)]);
            } else if dist > 1e-9 {
                self.push_words("G1", &[("X", px, 3), ("Y", py, 3)]);
            } else if (lz - self.z).abs() > 1e-7 {
                self.push_words("G1", &[("Z", lz, 3)]);
            }
            self.x = px;
            self.y = py;
            self.z = lz;
            return;
        }
        // Consecutive pieces of a wall only change feed/flow. There is no
        // travel or travel acceleration between the pieces of a speed ramp.
        if !layer_start && dist < 1e-9 && self.retracted <= 0.0 && self.z_known && (self.z - lz).abs() < 1e-9 {
            return;
        }
        let min_travel = self.eff_m("retraction_minimum_travel", self.filament);
        let from = (self.x, self.y);
        let mut route = if self.avoid_on && !avoid_skip { self.avoid_route(from, (px, py)) } else { None };
        let length = route.as_ref().map_or(dist, |r| travel_length(from, r, (px, py)));
        // From fill to fill without crossing a wall: no retraction (`reduce_infill_retraction_mode`).
        let fill = |r: Role| matches!(r, Role::SparseInfill | Role::InternalSolidInfill | Role::FloatingVerticalShell | Role::GapFill);
        let reduce = self.memo(10, "reduce_infill_retraction_mode", self.filament, || match self.job.s("reduce_infill_retraction_mode") {
            "Disabled" => 0.0,
            "Enabled" => 1.0,
            // Auto: only for a filament that does not stick to the nozzle; one that does
            // (`filament_metal_stickiness`) strings and retracts every time.
            _ => if matches!(self.job.fil_str("filament_metal_stickiness", self.filament), "" | "None" | "Low") { 1.0 } else { 0.0 },
        }) != 0.0;
        let hop = self.eff_m("z_hop", self.filament);
        // Never on the first layer: a dry travel at that height drags across what is printed.
        let reduce = reduce && !first_layer && fill(self.last_role) && fill(next_role);
        let all_inside = (reduce || self.avoid_on && hop > 0.0) && if let Some(points) = &route {
            let mut a = from;
            points.iter().copied().chain(std::iter::once((px, py))).all(|b| {
                let inside = self.travel_inside(a, b);
                a = b;
                inside
            })
        } else { self.travel_inside(from, (px, py)) };
        let need_retract = layer_start || self.retracted > 0.0 || (!(reduce && all_inside) && length > min_travel);
        if self.avoid_on && hop > 0.0 && all_inside { self.wipe = None; }
        if need_retract && self.retracted <= 0.0 {
            if self.b_m("fab_wipe_retract_at_retraction_speed") {
                let a = self.travel_accel(first_layer);
                self.set_accel(a);
            }
            self.retract(first_layer);
        }
        if self.avoid_on && !avoid_skip && (self.x, self.y) != from {
            route = self.avoid_route((self.x, self.y), (px, py));
        }
        // At a layer start the other slicer's trees lift above the new layer; the default
        // family's reference lifts above the layer just printed.
        let hop = if layer_start && hop > 0.0 && self.b_m("fab_layer_start_lift_above_new_layer") { hop + (lz - self.z).max(0.0) } else { hop };
        let zt = self.z + hop;
        let lift = self.lift_allowed(lz) && hop > 0.0 && zt > lz;
        let dist = route.as_ref().map_or_else(|| (px - self.x).hypot(py - self.y), |r| travel_length((self.x, self.y), r, (px, py)));
        // Short hops onto an outer wall use the gentle acceleration.
        let short_acc = self.ext_m("travel_short_distance_acceleration", self.job.ext_of(self.filament));
        let acc = if !first_layer && !lift && next_role == Role::OuterWall && dist < 2.0 && short_acc > 0.0 { short_acc } else { self.travel_accel(first_layer) };
        self.set_accel(acc);
        if let Some(route) = route {
            self.travel_route(&route, (px, py), lz, hop, lift, layer_start);
            self.unretract();
            return;
        }
        if lift {
            let kind = self.lift_type(self.filament);
            let run = hop / 3.0f64.to_radians().tan();
            let spiral = kind == Lift::Spiral || kind == Lift::Auto && (layer_start || self.lift_overhang((px, py), lz, run));
            if spiral || kind == Lift::Normal {
                if spiral { self.spiral_lift((px, py), hop) } else { self.lift_eager(hop, self.lift_feed(), true) }
                // The feed word only when the lift ran at another feed.
                if (self.feed - tf).abs() > 1e-6 {
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(self.z), fmt::f(tf)));
                } else {
                    self.push(&format!("G1 X{} Y{} Z{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(self.z)));
                }
            } else {
                // Rise at 3 degrees along the travel, then level.
                if dist > run {
                    let t = run / dist;
                    let mx = self.x + (px - self.x) * t;
                    let my = self.y + (py - self.y) * t;
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(mx), fmt::xyz(my), fmt::xyz(zt), fmt::f(tf)));
                    self.push(&format!("G1 X{} Y{} Z{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(zt)));
                } else {
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(zt), fmt::f(tf)));
                }
                self.z = zt;
            }
            self.feed = tf;
            self.push_words("G1", &[("Z", lz, 3)]);
            self.z = lz;
        } else {
            // Rise before the XY move; a 3D path that starts lower than the
            // last one ended comes down only at its own start, never onto
            // the bead just laid.
            let down_after = self.descend_after_xy && lz < self.z - 1e-9 && dist > 1e-9;
            if !down_after && (self.z - lz).abs() > 1e-9 {
                self.push_words("G1", &[("Z", lz, 3)]);
                self.z = lz;
            }
            if dist > 1e-9 {
                self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("F", tf, 3)]);
                self.feed = tf;
            }
            if down_after {
                self.push_words("G1", &[("Z", lz, 3)]);
                self.z = lz;
            }
        }
        self.x = px;
        self.y = py;
        self.unretract();
    }

    /// Lift on the first hop, then follow the route at that height.
    fn travel_route(&mut self, route: &[(f64, f64)], to: (f64, f64), lz: f64, hop: f64, lift: bool, layer_start: bool) {
        let tf = self.travel_speed();
        let down_after = self.descend_after_xy && lz < self.z - 1e-9;
        if lift {
            let (px, py) = route[0];
            let dist = (px - self.x).hypot(py - self.y);
            let zt = self.z + hop;
            let kind = self.lift_type(self.filament);
            let run = hop / 3.0f64.to_radians().tan();
            let spiral = kind == Lift::Spiral || kind == Lift::Auto && (layer_start || self.lift_overhang((px, py), lz, run));
            if spiral || kind == Lift::Normal {
                if spiral { self.spiral_lift((px, py), hop) } else { self.lift_eager(hop, self.lift_feed(), true) }
                if (self.feed - tf).abs() > 1e-6 {
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(self.z), fmt::f(tf)));
                } else {
                    self.push(&format!("G1 X{} Y{} Z{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(self.z)));
                }
            } else {
                if dist > run {
                    let t = run / dist;
                    let mx = self.x + (px - self.x) * t;
                    let my = self.y + (py - self.y) * t;
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(mx), fmt::xyz(my), fmt::xyz(zt), fmt::f(tf)));
                    self.push(&format!("G1 X{} Y{} Z{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(zt)));
                } else {
                    self.push(&format!("G1 X{} Y{} Z{} F{}", fmt::xyz(px), fmt::xyz(py), fmt::xyz(zt), fmt::f(tf)));
                }
                self.z = zt;
            }
            self.feed = tf;
            self.x = px;
            self.y = py;
        } else if !down_after && (self.z - lz).abs() > 1e-9 {
            self.push_words("G1", &[("Z", lz, 3)]);
            self.z = lz;
        }
        for (x, y) in route.iter().copied().skip(usize::from(lift)).chain(std::iter::once(to)) {
            if (self.feed - tf).abs() > 1e-6 {
                self.push_words("G1", &[("X", x, 3), ("Y", y, 3), ("F", tf, 3)]);
            } else {
                self.push_words("G1", &[("X", x, 3), ("Y", y, 3)]);
            }
            self.feed = tf;
            self.x = x;
            self.y = y;
        }
        if lift || down_after {
            self.push_words("G1", &[("Z", lz, 3)]);
            self.z = lz;
        }
    }

    /// Probe the slope's first run against nearby overhangs and supports.
    fn lift_overhang(&self, to: (f64, f64), lz: f64, run: f64) -> bool {
        let (dx, dy) = (to.0 - self.x, to.1 - self.y);
        let dist = dx.hypot(dy);
        let t = if dist > run { run / dist } else { 1.0 };
        let (a, b) = (Pt::from_mm(self.x, self.y), Pt::from_mm(self.x + dx * t, self.y + dy * t));
        let bb = BBox::of(&[a, b]);
        let ab = b.sub(a);
        for (z, area) in self.lift_areas.iter().take(self.layer_index + 1).skip(self.object_layer_start).rev() {
            if *z < lz - 0.4 - 1e-9 { break; }
            if *z > lz + 1e-9 { continue; }
            let Some(area) = area else { continue };
            if !bb.overlaps(&area.bbox) { continue; }
            if area.grid.contains_any(a) || area.grid.contains_any(b) { return true; }
            let mut hit = false;
            area.grid.visit_box(bb, |c, d, _| {
                if hit { return; }
                let cd = d.sub(c);
                let den = cross(ab, cd);
                if den == 0 { return; }
                let ac = c.sub(a);
                let (t, u) = (cross(ac, cd), cross(ac, ab));
                let inside = |v: i128| if den > 0 { v > 0 && v < den } else { v < 0 && v > den };
                if inside(t) && inside(u) { hit = true; }
            });
            if hit { return true; }
        }
        false
    }

    /// Does the straight travel from `a` to `b` stay inside the layer's parts?
    fn travel_inside(&self, a: (f64, f64), b: (f64, f64)) -> bool {
        let Some(g) = &self.inner else { return false };
        let (pa, pb) = (Pt::from_mm(a.0, a.1), Pt::from_mm(b.0, b.1));
        if !g.contains_any(pa) || !g.contains_any(pb) {
            return false;
        }
        let ab = pb.sub(pa);
        let mut crosses = false;
        g.visit_box(BBox::of(&[pa, pb]), |c, d, _| {
            if crosses {
                return;
            }
            let cd = d.sub(c);
            let den = cross(ab, cd);
            if den == 0 {
                return;
            }
            let ac = c.sub(pa);
            let (t, u) = (cross(ac, cd), cross(ac, ab));
            let inside = |v: i128| if den > 0 { v > 0 && v < den } else { v < 0 && v > den };
            if inside(t) && inside(u) {
                crosses = true;
            }
        });
        !crosses
    }

    /// Intermediate travel points around the layer's inset walls.
    fn avoid_route(&self, a: (f64, f64), b: (f64, f64)) -> Option<Vec<(f64, f64)>> {
        if !self.avoid_on || !self.xy_known || self.avoid_skip || matches!(self.last_role, Role::Skirt | Role::Brim) { return None; }
        let direct = (b.0 - a.0).hypot(b.1 - a.1);
        if direct <= self.eff_m("retraction_minimum_travel", self.filament) { return None; }
        let avoid = self.avoid.get(self.layer_index)?.as_ref()?;
        let (pa, pb) = (Pt::from_mm(a.0, a.1), Pt::from_mm(b.0, b.1));
        if !travel_crosses(self.inner.as_ref()?, pa, pb) { return None; }
        let (sa, sb) = (avoid.snap(pa), avoid.snap(pb));
        let mut hits = Vec::new();
        avoid.edges.visit_box(BBox::of(&[sa, sb]), |c, d, (r, e)| {
            if let Some((t, exiting)) = travel_crossing(sa, sb, c, d) {
                hits.push((t, r, e, lerp(sa, sb, t), exiting));
            }
        });
        hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        // A segment may occupy several grid cells.
        hits.dedup_by(|a, b| a.1 == b.1 && a.2 == b.2);
        let mut points = vec![sa];
        let (mut i, mut detoured) = (0, false);
        while i < hits.len() {
            let (_, r, e, p, exiting) = hits[i];
            if exiting {
                if let Some(j) = hits[i + 1..].iter().rposition(|h| h.1 == r && !h.4).map(|j| i + 1 + j) {
                    let (_, _, end, q, _) = hits[j];
                    if end != e {
                        let ring = &avoid.rings[r as usize];
                        let n = ring.len();
                        let arc = |forward: bool| {
                            let mut path = vec![p];
                            let mut k = if forward { (e as usize + 1) % n } else { e as usize };
                            let last = if forward { end as usize } else { (end as usize + 1) % n };
                            loop {
                                path.push(ring[k]);
                                if k == last { break; }
                                k = if forward { (k + 1) % n } else { (k + n - 1) % n };
                            }
                            path.push(q);
                            path
                        };
                        let (forward, backward) = (arc(true), arc(false));
                        let length = |pts: &[Pt]| pts.windows(2).map(|w| w[0].dist(w[1])).sum::<f64>();
                        points.extend(if length(&forward) <= length(&backward) { forward } else { backward });
                        detoured = true;
                        i = j + 1;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if !detoured { return None; }
        points.push(sb);
        points.dedup();
        let on_start = sa != pa || avoid.inside.contains_any(pa);
        let on_end = sb != pb || avoid.inside.contains_any(pb);
        let eligible = |k: usize| (k != 0 || on_start) && (k + 1 != points.len() || on_end);
        let mut kept = vec![points[0]];
        let mut k = 0;
        // Each successful query advances; each kept point costs at most one failure.
        while k + 1 < points.len() {
            let mut next = k + 1;
            if eligible(k) {
                let mut m = next;
                while m < points.len() && eligible(m) && avoid.visible(points[k], points[m]) {
                    next = m;
                    m += 1;
                }
            }
            kept.push(points[next]);
            k = next;
        }
        // Snapped endpoints stay; original endpoints are supplied by travel_to.
        let start = usize::from(kept.first() == Some(&pa));
        let end = kept.len() - usize::from(kept.last() == Some(&pb));
        if start >= end { return None; }
        let route: Vec<_> = kept[start..end].iter().map(|p| (p.x_mm(), p.y_mm())).collect();
        let limit = self.job.cfg.abs_or_pct("max_travel_detour_distance", 0, direct);
        if limit > 0.0 && travel_length(a, &route, b) - direct > limit { return None; }
        Some(route)
    }

    fn extrude(&mut self, path: &ExtrusionPath, speed: f64, accel: f64, lz: f64, first_layer: bool, layer_h: f64) {
        // Only this call may use the fit made ahead for it.
        let fit_key = self.fit_key.take();
        if path.role != self.role {
            self.feature(path.role);
        }
        if path.mode != self.mode {
            self.push(&format!("; FAB_MODE: {}", path.mode));
            self.mode = path.mode;
        }
        if path.core != self.core {
            self.push(if path.core { "; FAB_CORE: 1" } else { "; FAB_CORE: 0" });
            self.core = path.core;
        }
        // Model and support beads can have different heights at the same Z.
        let h = if path.height > 0.0 { path.height } else { layer_h };
        // A variable-width bead tags each width as it prints it.
        if (path.width - self.width).abs() > 1e-6 && segment_width(path, 0, h).is_none() {
            self.push_tag("; LINE_WIDTH: ", path.width);
            self.width = path.width;
        }
        if (h - self.height).abs() > 1e-6 {
            self.push_tag("; LAYER_HEIGHT: ", h);
            self.height = h;
        }
        let _ = lz;
        self.unretract();
        let bridge_fan = match (path.role, self.internal_bridge_fan()) {
            (Role::InternalBridge, Some(s)) if self.internal_bridges() => Some(s),
            _ => self.wants_overhang_fan(path).then(|| self.overhang_fan.unwrap_or(self.fan)),
        };
        if let Some(s) = bridge_fan {
            self.push(&format!("M106 S{}", fmt::fan(s)));
            self.overhang_fan_on = true;
        }
        if path.role == Role::FloatingVerticalShell {
            // The set speed, marked so later passes leave it alone.
            let fv = speed * 60.0;
            let line = self.firmware.fixed_feed(fv);
            self.push(&line);
            self.feed = fv;
        } else {
            self.set_feed_line(speed * 60.0);
        }
        self.set_accel(accel);
        let f = self.filament;
        let mut ratio = self.fil_m("filament_flow_ratio", f) * self.f_m("print_flow_ratio").max(0.0);
        if ratio <= 0.0 {
            ratio = 1.0;
        }
        ratio *= self.cal_flow;
        if first_layer {
            ratio *= self.f_m("initial_layer_flow_ratio").max(0.0).max(1e-6);
        }
        if path.role == Role::TopSurface {
            let t = self.ext_m("top_solid_infill_flow_ratio", self.job.ext_of(f));
            if t > 0.0 {
                ratio *= t;
            }
        }
        let e_per_mm = path.area * ratio / self.memo(6, "filament_diameter", f, || self.job.filament_area(f));
        // Arcs only from where the head is: the fit's first point is the start of every arc word.
        let at_start = path.pts.first().is_some_and(|p| (p.x_mm() - self.x).hypot(p.y_mm() - self.y) < 0.5 * crate::arcfit::QUANTUM);
        // The reference keeps brim corners as segments (no arcs).
        if !path.z.is_empty() || !path.flow.is_empty() {
            self.extrude_3d(path, lz, e_per_mm);
            self.after_extrude(path);
            return;
        }
        if path.dry.is_empty() && path.role != Role::Brim && self.b_m("enable_arc_fitting") && path.pts.len() >= 3 && at_start {
            let pts: Vec<[f64; 2]> = path.pts.iter().map(|p| [p.x_mm(), p.y_mm()]).collect();
            let tol = { let r = self.f_m("resolution"); if r > 0.0 { r } else { 0.012 } };
            let made = fit_key.and_then(|k| self.fits.remove(&k));
            for seg in made.unwrap_or_else(|| crate::arcfit::fit_arcs_within(&pts, tol, crate::arcfit::max_sweep_for(path.role.name()))) {
                match seg {
                    crate::arcfit::Seg::Line { end } => {
                        let d = ((end[0] - self.x).powi(2) + (end[1] - self.y).powi(2)).sqrt();
                        if d < 1e-6 {
                            continue;
                        }
                        self.push_words("G1", &[("X", end[0], 3), ("Y", end[1], 3), ("E", d * e_per_mm, 5)]);
                    }
                    crate::arcfit::Seg::Arc { end, center, ccw, len, .. } => {
                        self.push_arc(end, center, ccw, len * e_per_mm);
                    }
                }
                let end = match seg { crate::arcfit::Seg::Line { end } | crate::arcfit::Seg::Arc { end, .. } => end };
                self.x = end[0];
                self.y = end[1];
            }
            self.after_extrude(path);
            return;
        }
        for (i, p) in path.pts.iter().enumerate().skip(1) {
            let (px, py) = (p.x_mm(), p.y_mm());
            let d = ((px - self.x).powi(2) + (py - self.y).powi(2)).sqrt();
            if d < 1e-6 {
                continue;
            }
            if path.dry.get(i - 1).copied().unwrap_or(false) {
                self.push_words("G1", &[("X", px, 3), ("Y", py, 3)]);
            } else {
                self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("E", d * e_per_mm, 5)]);
            }
            self.x = px;
            self.y = py;
        }
        self.after_extrude(path);
    }

    /// A 3D or variable-flow path: absolute Z = layer z + `z[i]`, E over the
    /// 3D length times the segment's flow, never arc-fitted. A segment whose
    /// rise would beat the machine's Z speed at the path feed gets its own
    /// slower feed (restored after it).
    fn extrude_3d(&mut self, path: &ExtrusionPath, lz: f64, e_per_mm: f64) {
        let vz_max = self.f_m("machine_max_speed_z");
        let feed = self.feed;
        for (i, p) in path.pts.iter().enumerate().skip(1) {
            let (px, py) = (p.x_mm(), p.y_mm());
            let pz = lz + path.z.get(i).copied().unwrap_or(0.0);
            let dxy = ((px - self.x).powi(2) + (py - self.y).powi(2)).sqrt();
            let dz = pz - self.z;
            let d = (dxy * dxy + dz * dz).sqrt();
            if d < 1e-6 {
                continue;
            }
            let z_word = (dz.abs() > 1e-7).then_some(pz);
            if path.dry.get(i - 1).copied().unwrap_or(false) {
                match z_word {
                    Some(z) => self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("Z", z, 3)]),
                    None => self.push_words("G1", &[("X", px, 3), ("Y", py, 3)]),
                }
            } else {
                if let Some(w) = segment_width(path, i - 1, self.height) {
                    if (w - self.width).abs() > 1e-6 {
                        self.push_tag("; LINE_WIDTH: ", w);
                        self.width = w;
                    }
                }
                let e = d * e_per_mm * path.flow.get(i - 1).copied().unwrap_or(1.0).max(0.0);
                // Z speed: F * |dz| / d <= vz_max (mm/s).
                let slow = (vz_max > 0.0 && dz.abs() > 1e-9 && feed / 60.0 * dz.abs() / d > vz_max).then(|| vz_max * d / dz.abs() * 60.0);
                if let Some(f) = slow {
                    self.push_words("G1", &[("F", f.floor().max(1.0), 0)]);
                }
                match z_word {
                    Some(z) => self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("Z", z, 3), ("E", e, 5)]),
                    None => self.push_words("G1", &[("X", px, 3), ("Y", py, 3), ("E", e, 5)]),
                }
                if e > 0.0 { self.printed_top = self.printed_top.max(self.z).max(pz); }
                if slow.is_some() {
                    self.push_words("G1", &[("F", feed, 3)]);
                }
            }
            self.x = px;
            self.y = py;
            self.z = pz;
        }
    }

    fn after_extrude(&mut self, path: &ExtrusionPath) {
        self.last_role = path.role;
        if self.overhang_fan_on {
            // Back to the layer's fan at the end of the bridge or overhang.
            self.overhang_fan_on = false;
            self.push(&format!("M106 S{}", fmt::fan(self.fan)));
        }
        self.ramp_open = !path.z.is_empty();
        // A 3D path is not wiped: walking back along it at the end's height would drag the tip
        // through its own bead where the path ran higher. It retracts in place.
        if !path.z.is_empty() {
            self.wipe = None;
            return;
        }
        // What a following retraction wipes along: on along the loop the path
        // ends on, else back along the path.
        let pts = if path.loop_pts.len() > 2 {
            path.loop_pts[1..].to_vec()
        } else {
            let mut v: Vec<Pt> = path.pts.iter().rev().skip(1).copied().collect();
            if v.is_empty() {
                v.push(path.first());
            }
            v
        };
        self.wipe = Some(Wipe { pts, speed: path.speed });
    }

    fn accel_for(&self, role: Role, first_layer: bool) -> f64 {
        let e = self.job.ext_of(self.filament);
        if first_layer {
            return self.ext_m("initial_layer_acceleration", e);
        }
        let role = if role == Role::OverhangWall && self.b_m("fab_overhang_wall_bridge_acceleration") { Role::Bridge } else { role };
        // An object with its own accelerations reads them from its own settings.
        if let Some(own) = self.object_accels.get(self.accel_object).and_then(|a| a.as_ref()).filter(|_| !matches!(role, Role::PrimeTower | Role::Skirt)) {
            return own.for_role(role, e);
        }
        let def = self.ext_m("default_acceleration", e);
        let pick = |key: &'static str| -> f64 {
            self.memo(5, key, e, || {
                let raw = self.job.cfg.raw(key, e);
                let v = self.job.ext(key, e);
                let v = if raw.ends_with('%') { def * v / 100.0 } else { v };
                if v > 0.0 {
                    v
                } else {
                    def
                }
            })
        };
        match role {
            Role::Bridge | Role::InternalBridge => self.memo(5, "bridge_acceleration", e, || bridge_accel(self.job, e, pick("outer_wall_acceleration"), def)),
            Role::OuterWall | Role::OverhangWall => pick("outer_wall_acceleration"),
            Role::InnerWall => pick("inner_wall_acceleration"),
            Role::TopSurface => pick("top_surface_acceleration"),
            Role::SparseInfill => pick("sparse_infill_acceleration"),
            _ => def,
        }
    }

    /// Print the skirt before the layer's object paths.
    fn skirt_layer(&mut self, li: usize, lz: f64, first: bool, layer_started: &mut bool) {
        let Some(skirt) = self.skirt.take() else { return };
        let h = skirt.heights[li];
        if h > 0.0 {
            let area = flow::cross_section(skirt.width, h);
            let speed = skirt_speed(self.job, self.filament, first, area);
            let accel = self.accel_for(Role::Skirt, first);
            let nozzle = self.job.nozzle_diameter(self.job.ext_of(self.filament)).max(0.1);
            let gap = self.job.cfg.abs_or_pct("seam_gap", 0, nozzle) * SCALE;
            for ring in &skirt.loops {
                let i = nearest_vertex(&ring.pts, Pt::from_mm(self.x, self.y));
                let mut pl = ring.split_at(i);
                let loop_pts = pl.pts.clone();
                pl.clip_end(gap);
                let path = ExtrusionPath { role: Role::Skirt, pts: pl.pts, closed: true, loop_pts, width: skirt.width, height: h, area, speed, ..ExtrusionPath::default() };
                if path.pts.len() < 2 { continue; }
                if self.by_object && !self.xy_known { self.object_travel(path.first(), first); }
                self.descend_after_xy = self.by_object;
                self.travel_to(path.first(), lz, first, !*layer_started && !first, Role::Skirt);
                self.descend_after_xy = false;
                *layer_started = true;
                self.extrude(&path, speed, accel, lz, first, h);
            }
        }
        self.skirt = Some(skirt);
    }

    /// The tower layer's height: the step from the layer below, which is less than the
    /// thickest object's layer when objects print at different layer heights.
    fn tower_h(&self) -> f64 {
        self.tower_height.max(0.05)
    }

    /// The part fan (`M106 S` value) and the auxiliary fan of layer `li`
    /// (0-based) from its estimated time (R5.3 §3): full below the slowdown
    /// time, blended down to the minimum (or off, without
    /// `reduce_fan_stop_start_freq`) by the cooling time, off for the first
    /// layers, ramped up to `full_fan_speed_layer`.
    fn fan_for_layer(&self, li: usize, layer_time: f64) -> (f64, i32) {
        let f = self.filament;
        let mut close = self.fil_m("close_fan_the_first_x_layers", f).max(0.0) as usize;
        let full_layer = self.fil_m("full_fan_speed_layer", f).max(0.0) as usize;
        if close == 0 && full_layer > 0 {
            close = 1;
        }
        let (fmin, fmax) = (self.fil_m("fan_min_speed", f), self.fil_m("fan_max_speed", f));
        let slow_t = self.fil_m("slow_down_layer_time", f);
        let cool_t = self.fil_m("fan_cooling_layer_time", f);
        let first_x = if self.job.cfg.has("first_x_layer_part_fan_speed") { self.fil_m("first_x_layer_part_fan_speed", f) } else { 0.0 };
        let base = if self.job.eff_bool("reduce_fan_stop_start_freq", f) || self.job.cfg.bool_at("reduce_fan_stop_start_freq", f) { fmin } else { 0.0 };
        let mut pct = if li < close {
            first_x
        } else if layer_time < slow_t {
            fmax
        } else if layer_time < cool_t && cool_t > slow_t {
            let t = (layer_time - slow_t) / (cool_t - slow_t);
            // Whole percent, rounded down; a blend's rounding error below a whole percent (equal
            // ends give 99.999…) is not a percent less.
            (t * fmin + (1.0 - t) * fmax + 1e-9).floor()
        } else {
            base
        };
        if li >= close && li + 1 < full_layer && full_layer > close {
            let factor = (li + 1 - close) as f64 / (full_layer - close) as f64;
            pct = (first_x * (1.0 - factor) + pct * factor).round();
        }
        let part = (255.0 * pct / 100.0).clamp(0.0, 255.0);
        let close_aux = self.fil_m("close_additional_fan_first_x_layers", f) as usize;
        let aux_pct = if li < close_aux { 0.0 } else { self.fil_m("additional_cooling_fan_speed", f) };
        let aux = if self.b_m("auxiliary_fan") || self.job.cfg.has("auxiliary_fan") { (aux_pct * 2.55).floor() as i32 } else { 0 };
        (part, aux)
    }

    /// The overhang/bridge fan of layer `li` (`M106 S` value): on from the
    /// layer after the closed ones, when the filament enables it and it is
    /// above the layer's part fan.
    fn overhang_fan_for_layer(&self, li: usize, part: f64) -> Option<f64> {
        let f = self.filament;
        let close = self.fil_m("close_fan_the_first_x_layers", f).max(0.0) as usize;
        if li < close || !self.job.cfg.bool_at("enable_overhang_bridge_fan", f) {
            return None;
        }
        let s = (255.0 * self.fil_m("overhang_fan_speed", f) / 100.0).clamp(0.0, 255.0);
        (s > part + 1e-9).then_some(s)
    }

    /// The fan of an internal bridge whose job sets one (0-100 %), with the bridge fan on: that
    /// speed exactly. None (and -1) leaves it to the overhang fan's rule.
    fn internal_bridge_fan(&self) -> Option<f64> {
        let f = self.filament;
        let v = self.memo(11, "internal_bridge_fan_speed", f, || {
            if self.job.cfg.has("internal_bridge_fan_speed") && self.job.cfg.bool_at("enable_overhang_bridge_fan", f) { self.job.cfg.f64_at("internal_bridge_fan_speed", f) } else { -1.0 }
        });
        (v >= 0.0).then(|| (255.0 * v / 100.0).clamp(0.0, 255.0))
    }

    /// Whether this job prints internal bridges as their own role (its presets set
    /// `internal_bridge_speed`). Without it, nothing treats a path as an internal bridge.
    fn internal_bridges(&self) -> bool {
        self.memo(12, "internal_bridge_speed", 0, || if self.job.cfg.has("internal_bridge_speed") { 1.0 } else { 0.0 }) != 0.0
    }

    /// The overhang degree from which the overhang fan runs
    /// (`overhang_fan_threshold`: 0 % = any overhang … 95 % = fully over air).
    fn overhang_fan_degree(&self) -> f32 {
        let t = self.job.cfg.raw("overhang_fan_threshold", self.filament).trim().trim_end_matches('%').parse::<f64>().unwrap_or(95.0);
        if t <= 0.0 {
            0.01
        } else if t <= 10.0 {
            1.0
        } else if t <= 25.0 {
            2.0
        } else if t <= 50.0 {
            3.0
        } else if t <= 75.0 {
            4.0
        } else {
            5.0
        }
    }

    /// Is the overhang fan wanted for this path: a bridge, a wall over air,
    /// or a wall at or past the threshold degree.
    fn wants_overhang_fan(&self, path: &ExtrusionPath) -> bool {
        if self.overhang_fan.is_none() {
            return false;
        }
        match path.role {
            Role::Bridge | Role::OverhangWall => true,
            Role::InternalBridge => self.internal_bridges(),
            Role::OuterWall | Role::InnerWall => path.overhang > 0.0 && path.overhang >= self.overhang_fan_degree(),
            _ => false,
        }
    }

    fn set_fans(&mut self, part: f64, aux: i32) {
        self.layer_fans = (part, aux);
        self.overhang_fan_on = false;
        if (part - self.fan).abs() > 1e-9 {
            self.push(&format!("M106 S{}", fmt::fan(part)));
            self.fan = part;
        }
        if aux != self.fan_aux {
            let line = self.firmware.aux_fan(aux);
            self.push(&line);
            self.fan_aux = aux;
        }
    }

    fn end_label(&mut self, object: Option<usize>) {
        if let Some(line) = self.firmware.label_end(object.map(|o| self.object_names[o].as_str())) {
            self.push(&line);
        }
    }

    fn open_all_label(&mut self, multi: bool, all_mask: u64) {
        if multi {
            if let Some(o) = self.open_label.take() {
                self.push(&format!("; stop printing object, unique label id: {}", self.label_ids[o]));
                self.end_label(Some(o));
            }
            if let Some(line) = self.firmware.label_objects(all_mask) {
                self.push(&line);
            }
        }
    }

    fn close_all_label(&mut self, multi: bool) {
        if multi {
            self.end_label(None);
        }
    }

    /// The timelapse template: inline at the head, or from the parking spot.
    #[allow(clippy::too_many_arguments)]
    fn timelapse(&mut self, tpl: &str, job: &Job, ln: usize, lz: f64, max_z: f64, most_used_phys: usize, inline: bool, multi: bool, all_mask: u64, _li: usize) {
        if tpl.is_empty() {
            return;
        }
        let multi = multi || self.firmware.labels_every_object();
        let fil = self.filament;
        let out = self.render("time_lapse_gcode", tpl, |env| {
            vars::set_layer_vars(env, job, ln, lz, max_z, fil, most_used_phys, inline);
        });
        let named = self.firmware.0 == family::Flavor::Named;
        let moves = out.lines().any(|l| {
            let t = l.trim_start();
            ["G0", "G1", "G2", "G3", "G28", "M9711"].iter().any(|g| t.starts_with(g) && t[g.len()..].starts_with([' ', '\t']) || t == *g)
        }) || named && out.lines().any(named_line_moves);
        if moves { self.lift_if_z_unknown(lz); }
        if moves && !inline {
            // The head parks for the photo: retract and lift first.
            let first = ln == 0;
            self.retract(first);
            let hop = self.eff_m("z_hop", self.filament);
            if hop > 0.0 && self.xy_known && self.lift_allowed(lz) {
                self.lift_eager(hop, self.lift_feed(), self.lift_type(self.filament) == Lift::Normal);
            }
        }
        if multi {
            self.open_all_label(true, all_mask);
        }
        self.push_block(&out);
        self.push("");
        if moves {
            // The photo moved the head: come back to the layer height and
            // forget where XY is. Before the layer's first move the head's
            // height is not known yet: come back lifted above the layer,
            // never to Z 0 (the print safety check refuses that, rightly).
            if self.z < lz - 1e-9 {
                self.z = lz + job.eff("z_hop", self.filament).max(0.2);
            }
            self.push(&format!("G1 Z{:.3}", self.z));
            self.xy_known = false;
            self.z_known = true;
            // No wipe retraces a path from a position the printer chose.
            if named { self.wipe = None; }
        }
        if multi {
            self.close_all_label(true);
        }
    }

    fn begin_change_speed_guard(&mut self) {
        if !self.change_speed_guarded && self.b_m("fab_toolchange_speed_guard") {
            self.push("M220 B");
            self.push("M220 S100");
            self.change_speed_guarded = true;
        }
    }

    fn end_change_speed_guard(&mut self) {
        if std::mem::take(&mut self.change_speed_guarded) {
            self.push("M220 R");
        }
    }

    /// Change to filament `to`: ram the old filament on the tower (nozzle
    /// changes only), run the printer's change template, prime the new
    /// filament on the tower and close the tower wall.
    #[allow(clippy::too_many_arguments)]
    fn tool_change(&mut self, to: usize, job: &Job, li: usize, lz: f64, max_z: f64, first: bool, multi: bool, all_mask: u64, layer_start: bool, last_in_layer: bool) {
        self.begin_change_speed_guard();
        let from = self.filament;
        let (e_from, e_to) = (job.ext_of(from), job.ext_of(to));
        let nozzle_change = e_from != e_to;
        self.next_filament_hint = to;
        self.change_idx += 1;
        let h = self.tower_h();
        self.open_all_label(multi, all_mask);
        // The tower before the change: the wall while its filament is in,
        // then the outgoing nozzle's ramming rows.
        let mut prime_start = Pt::from_mm(self.x, self.y);
        let prime_plan = self.prime_plan(to, h);
        if let Some((ys, right)) = &prime_plan {
            let (x0, x1) = self.tower.as_ref().unwrap().row_span(self.tower.as_ref().unwrap().lw);
            prime_start = Pt::from_mm(if *right { x0 } else { x1 }, ys[0]);
        }
        if self.tl.is_some() {
            let wall_f = self.tower.as_ref().map(|t| t.wall_filament).unwrap_or(usize::MAX);
            if from == wall_f && !self.tl.as_ref().unwrap().wall_done {
                let skip = prime_plan.as_ref().map(|(ys, right)| (*right, ys[0]));
                self.tower_wall(lz, first, h, skip, layer_start);
            }
            if nozzle_change {
                // The outgoing block's free space becomes grid first when no
                // later change of this layer uses it; ramming comes last.
                let t = self.tower.clone().unwrap();
                let bf = t.block_of(self.category(from));
                let ram_depth = self.ram_rows(&t, from, h) as f64 * (t.rw + t.gap);
                if !self.block_used_later(bf, true) {
                    self.tower_grid(bf, ram_depth, lz, first, h, layer_start);
                }
                self.tower_ram(from, lz, first, h, layer_start);
            }
        }
        // Retract for the change (the wipe takes most of it).
        let parked_only = self.b_m("fab_toolchange_prime_parked_only");
        let tc_len = if parked_only { self.eff_m("retract_length_toolchange", from) } else { job.cfg.f64_at("retract_length_toolchange", e_from) };
        let wipe_speed = self.f_m("prime_tower_max_speed");
        self.wipe_speed_override = (wipe_speed > 0.0).then_some(wipe_speed);
        self.retract_by(if parked_only { tc_len } else { tc_len.max(self.retract_len()) }, first);
        if parked_only && tc_len > self.retracted {
            let feed = self.retract_speed() * 60.0;
            self.push_words("G1", &[("E-", tc_len - self.retracted, 5), ("F", feed, 3)]);
            self.feed = feed;
            self.retracted = tc_len;
        }
        self.wipe_speed_override = None;
        // What each extruder has drawn back stays as it is left: the printer is told, and it is
        // pushed back when that extruder prints again.
        self.drawn_back[e_from] = self.retracted;
        if parked_only {
            self.parked_prime[e_from] = self.retracted + self.eff_m("retract_restart_extra_toolchange", from);
        }
        let new_retracted = self.drawn_back[e_to];
        let tower = self.tower.is_some();
        let tower_hop = if tower { self.f_m("prime_tower_lift_height") } else { 0.0 };
        let hop = if tower_hop > 0.0 { tower_hop } else { self.eff_m("z_hop", from) };
        if hop > 0.0 && self.lift_allowed(lz) {
            let feed = if tower { self.f_m("prime_tower_lift_speed") * 60.0 } else { self.lift_feed() };
            self.lift_eager(hop, feed, tower && self.lift_type(from) == Lift::Normal);
        }
        // The purge the printer's change runs: from what the target nozzle last held, less what
        // the paths right after the change take (the flush options). The time model is told.
        let purge = self.changes.change(self.loaded.get(e_to).copied().flatten(), to).purge_mm3;
        let into = self.flush_into.get_mut(li).and_then(|c| c.iter().position(|e| e.0 == to).map(|i| c.remove(i).1)).unwrap_or(0.0);
        if into > 0.0 {
            self.push_tag("; FLUSH_INTO: ", into);
        }
        let flush_vol = (purge - into).max(0.0);
        let flush_len = flush_vol / job.filament_area(to);
        let fe = job.cfg.raw("filament_end_gcode", from).to_string();
        let out = self.render("filament_end_gcode", &fe, |env| {
            env.set("current_filament_id", TVar::int(from as i64));
            env.set("current_extruder_id", TVar::int(e_from as i64));
            env.set("current_nozzle_id", TVar::int(e_from as i64));
            env.set("layer_num", TVar::int(li as i64));
        });
        self.push_block(&out);
        let cg = job.cfg.str("change_filament_gcode").to_string();
        let tc = self.toolchanges + 1;
        let tw_c = self.tower.as_ref().map(|t| [t.x + t.w / 2.0, t.y + t.d / 2.0]).unwrap_or([0.0, 0.0]);
        let avoid_x = self.tower.as_ref().map(|t| t.x + t.w + 3.0).unwrap_or(0.0);
        let temp = |f: usize| if first { job.fil("nozzle_temperature_initial_layer", f) } else { job.fil("nozzle_temperature", f) };
        let feed = |f: usize| -> i64 {
            let v = job.fil("filament_max_volumetric_speed", f);
            let r = (60.0 * v / job.filament_area(f)) as i64;
            if r == 0 {
                100
            } else {
                r
            }
        };
        let (px, py) = (prime_start.x_mm(), prime_start.y_mm());
        let vfrom = job.cfg.raw("filament_extruder_variant", from).to_string();
        let vto = job.cfg.raw("filament_extruder_variant", to).to_string();
        // The new filament reaches the tower this much cooler (R5.3 §1.5); never on the
        // first layer or without a tower.
        let has_tower = self.tower.is_some();
        let cool: Vec<f64> = (0..job.filaments()).map(|f| if first || !has_tower { 0.0 } else { job.fil("filament_cooling_before_tower", f) }).collect();
        let change_z = if self.by_object { max_z + if hop > 0.0 { hop } else { 0.4 } } else { lz };
        let out = self.render("change_filament_gcode", &cg, |env| {
            env.current_extruder_id = from;
            let i = |v: i64| TVar::int(v);
            env.set("current_extruder_id", i(e_from as i64));
            env.set("next_extruder_id", i(e_to as i64));
            env.set("current_nozzle_id", i(e_from as i64));
            env.set("next_nozzle_id", i(e_to as i64));
            env.set("current_filament_id", i(from as i64));
            env.set("next_filament_id", i(to as i64));
            env.set("previous_extruder", i(from as i64));
            env.set("next_extruder", i(to as i64));
            env.set("next_hotend", i(-1));
            // Named once a change has happened with no tower to prime on.
            env.set("current_hotend", i(if has_tower || tc == 1 { -1 } else { e_from as i64 }));
            env.set("layer_num", i(li as i64));
            env.set("layer_z", TVar::f64(lz));
            env.set("max_layer_z", TVar::f64(max_z));
            env.set("relative_e_axis", TVar::boolean(true));
            env.set("toolchange_count", i(tc as i64));
            env.set("fan_speed", i(0));
            env.set("old_retract_length", TVar::f64(job.eff("retraction_length", from)));
            env.set("new_retract_length", TVar::f64(job.eff("retraction_length", to)));
            env.set("old_retract_length_toolchange", TVar::f64(job.cfg.f64_at("retract_length_toolchange", e_from)));
            env.set("new_retract_length_toolchange", TVar::f64(job.cfg.f64_at("retract_length_toolchange", e_to)));
            env.set("new_extruder_retracted_length", TVar::f64(new_retracted));
            env.set("old_filament_temp", i(temp(from).round() as i64));
            env.set("new_filament_temp", i(temp(to).round() as i64));
            env.set("x_after_toolchange", TVar::f64(px));
            env.set("y_after_toolchange", TVar::f64(py));
            env.set("z_after_toolchange", TVar::f64(change_z));
            env.set("first_flush_volume", TVar::f64(flush_len / 2.0));
            env.set("second_flush_volume", TVar::f64(flush_len / 2.0));
            env.set("old_filament_e_feedrate", i(feed(from)));
            env.set("new_filament_e_feedrate", i(feed(to)));
            env.set("old_extruder_variant", TVar::string(vfrom.clone()));
            env.set("new_extruder_variant", TVar::string(vto.clone()));
            for k in 1..=3 {
                env.set(&format!("travel_point_{k}_x"), TVar::f64(0.0));
                env.set(&format!("travel_point_{k}_y"), TVar::f64(0.0));
            }
            env.set("wipe_avoid_perimeter", TVar::boolean(false));
            env.set("wipe_avoid_pos_x", TVar::f64(avoid_x));
            env.set("wipe_tower_center_pos_x", TVar::f64(tw_c[0]));
            env.set("wipe_tower_center_pos_y", TVar::f64(tw_c[1]));
            env.set("wipe_tower_center_pos_valid", TVar::boolean(true));
            env.set("filament_cooling_before_tower", TVar::f64s(&cool));
            env.set("is_prime_tower_interface", TVar::boolean(false));
            env.set("filament_tower_interface_purge_volume", TVar::f64(0.0));
            env.set("filament_tower_interface_print_temp", i(-1));
            env.set("flush_length", TVar::f64(flush_len));
            let count = if flush_vol > 0.0 { ((flush_vol / 135.0).round() as i64).clamp(1, 4) } else { 0 };
            for k in 1..=4 {
                let v = if (k as i64) <= count { flush_len / count as f64 } else { 0.0 };
                env.set(&format!("flush_length_{k}"), TVar::f64(v));
            }
            env.set("retraction_distance_when_cut", TVar::f64(job.fil("retraction_distances_when_cut", from)));
            env.set("long_retraction_when_cut", TVar::boolean(job.cfg.bool_at("long_retractions_when_cut", e_from)));
            env.set("retraction_distance_when_ec", TVar::f64(job.fil("retraction_distances_when_ec", from)));
            env.set("long_retraction_when_ec", TVar::boolean(job.cfg.bool_at("long_retractions_when_ec", from)));
        });
        // The remaining-changes counter follows the tool command.
        let left = self.changes_left.saturating_sub(1);
        let tcmd = format!("T{to}");
        let mut text = String::with_capacity(out.len() + 16);
        let mut has_tool = false;
        for l in out.lines() {
            text.push_str(l);
            text.push('\n');
            if l == tcmd || l.starts_with(&format!("{tcmd} ")) {
                if let Some(line) = self.firmware.after_tool(left) {
                    text.push_str(&line);
                    text.push('\n');
                }
                has_tool = true;
            }
        }
        // A template without the tool command gets one (R1 §8.5).
        if !has_tool {
            text.push_str(&format!("{tcmd}\n"));
            if let Some(line) = self.firmware.after_tool(left) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        self.push_block(&text);
        let fs = job.cfg.raw("filament_start_gcode", to).to_string();
        let out = self.render("filament_start_gcode", &fs, |env| {
            env.set("filament_extruder_id", TVar::int(to as i64));
            env.set("current_filament_id", TVar::int(to as i64));
            env.set("layer_num", TVar::int(li as i64));
        });
        self.push_block(&out);
        // The firmware loaded, purged and parked: state is unknown.
        self.filament = to;
        // Change templates may set their own temperature and PA. Restore
        // the calibration overrides before priming the new filament.
        if let Some(temp) = self.cal_temp {
            self.nozzle_temperature(temp);
        }
        self.pressure_advance();
        if let Some(slot) = self.loaded.get_mut(e_to) {
            *slot = Some(to);
        }
        self.toolchanges += 1;
        self.changes_left = left;
        self.retracted = 0.0;
        self.restart_extra = 0.0;
        self.xy_known = false;
        // The change sequence leaves the head above the print.
        self.z_known = false;
        self.accel = 0.0;
        self.feed = 0.0;
        self.wipe = None;
        self.fan = -1.0;
        self.fan_aux = -1;
        // The change turned the fans off: back on at the layer's fan for the new filament.
        let (p, a) = self.fan_for_layer(self.object_layer, self.layer_time);
        self.set_fans(p, a);
        self.overhang_fan = self.overhang_fan_for_layer(self.object_layer, p);
        // Prime the new filament on the tower, then the wall (when this is
        // its filament) and the empty grid over the depth left.
        if let Some((ys, right)) = prime_plan {
            self.tower_prime(to, &ys, right, lz, first, h, tc, nozzle_change);
            let wall_f = self.tower.as_ref().map(|t| t.wall_filament).unwrap_or(usize::MAX);
            if to == wall_f && !self.tl.as_ref().unwrap().wall_done {
                self.tower_wall(lz, first, h, None, false);
            }
            if let Some(t) = self.tower.clone() {
                let bt = t.block_of(self.category(to));
                if !self.block_used_later(bt, false) {
                    self.tower_grid(bt, 0.0, lz, first, h, false);
                }
            }
            if last_in_layer {
                self.tower_finish(lz, first, h);
            }
        } else {
            // No tower to prime on: the printer sets the temperature, and the first extrusion
            // pushes back what this extruder has drawn back.
            let line = self.firmware.loaded_temperature(self.firmware.tool(job, self.filament), self.cal_temp.unwrap_or(temp(to)), Some("set nozzle temperature"));
            self.push(&line);
            if parked_only {
                self.retracted = std::mem::take(&mut self.parked_prime[e_to]);
                self.restart_extra = 0.0;
            } else if new_retracted > 0.0 {
                self.retracted = new_retracted;
                self.restart_extra = self.eff_m("retract_restart_extra", to);
            } else {
                self.retract(first);
            }
            self.restore_change_speed = self.b_m("fab_toolchange_speed_guard");
        }
        self.close_all_label(multi);
    }

    /// A tower layer with no change: the empty grid over the whole block,
    /// then the wall.
    #[allow(clippy::too_many_arguments)]
    fn tower_layer(&mut self, job: &Job, li: usize, lz: f64, first: bool, multi: bool, all_mask: u64, layer_start: bool) {
        let _ = (job, li);
        if self.tower.is_none() || self.tl.is_none() {
            return;
        }
        let h = self.tower_h();
        self.open_all_label(multi, all_mask);
        self.tower_empty = true;
        self.tower_finish_at(lz, first, h, layer_start);
        self.tower_empty = false;
        self.close_all_label(multi);
    }

    /// Extrude a tower polyline (the head is at `pts[0]`): full flow.
    fn tower_extrude(&mut self, pts: &[Pt], width: f64, h: f64, speed: f64, first_layer: bool) {
        self.ramp_open = false;
        if pts.len() < 2 {
            return;
        }
        if self.role != Role::PrimeTower {
            self.feature(Role::PrimeTower);
        }
        if (width - self.width).abs() > 1e-6 {
            self.push_tag("; LINE_WIDTH: ", width);
            self.width = width;
        }
        if (h - self.height).abs() > 1e-6 {
            self.push_tag("; LAYER_HEIGHT: ", h);
            self.height = h;
        }
        self.unretract();
        let acc = self.accel_for(Role::PrimeTower, first_layer);
        self.set_accel(acc);
        self.set_feed_line(speed * 60.0);
        let epm = tw::bead_area(width, h) / self.job.filament_area(self.filament);
        let at_start = (pts[0].x_mm() - self.x).hypot(pts[0].y_mm() - self.y) < 0.5 * crate::arcfit::QUANTUM;
        if pts.len() >= 3 && self.b_m("enable_arc_fitting") && at_start {
            let mm: Vec<[f64; 2]> = pts.iter().map(|p| [p.x_mm(), p.y_mm()]).collect();
            let tol = { let r = self.f_m("resolution"); if r > 0.0 { r } else { 0.012 } };
            for seg in crate::arcfit::fit_arcs_within(&mm, tol, crate::arcfit::max_sweep_for(Role::PrimeTower.name())) {
                match seg {
                    crate::arcfit::Seg::Line { end } => {
                        let d = ((end[0] - self.x).powi(2) + (end[1] - self.y).powi(2)).sqrt();
                        if d < 1e-6 {
                            continue;
                        }
                        self.push(&format!("G1 X{} Y{} E{}", fmt::xyz(end[0]), fmt::xyz(end[1]), fmt::e(d * epm)));
                        if d * epm > 0.0 { self.printed_top = self.printed_top.max(self.z); }
                        self.x = end[0];
                        self.y = end[1];
                    }
                    crate::arcfit::Seg::Arc { end, center, ccw, len, .. } => {
                        self.push_arc(end, center, ccw, len * epm);
                        self.x = end[0];
                        self.y = end[1];
                    }
                }
            }
        } else {
        for p in &pts[1..] {
            let (px, py) = (p.x_mm(), p.y_mm());
            let d = ((px - self.x).powi(2) + (py - self.y).powi(2)).sqrt();
            if d < 1e-6 {
                continue;
            }
            self.push(&format!("G1 X{} Y{} E{}", fmt::xyz(px), fmt::xyz(py), fmt::e(d * epm)));
            if d * epm > 0.0 { self.printed_top = self.printed_top.max(self.z); }
            self.x = px;
            self.y = py;
        }
        }
        self.last_role = Role::PrimeTower;
        let mut back: Vec<Pt> = pts.iter().rev().skip(1).copied().collect();
        if back.is_empty() {
            back.push(pts[0]);
        }
        self.wipe = Some(Wipe { pts: back, speed });
    }

    /// A plain move inside the tower (no retraction, no lift).
    fn tower_move(&mut self, x: f64, y: f64, feed: Option<f64>) {
        self.ramp_open = false;
        let mut l = format!("G1 X{} Y{}", fmt::xyz(x), fmt::xyz(y));
        if let Some(f) = feed {
            if (f - self.feed).abs() > 1e-6 {
                l.push_str(&format!(" F{}", fmt::f(f)));
                self.feed = f;
            }
        }
        self.push(&l);
        self.x = x;
        self.y = y;
    }

    /// The tower speed (mm/s) for a bead: `want`, within the tower's and the
    /// filament's limits (first layer: the first-layer speed).
    fn tower_cap(&self, want: f64, area: f64, first_layer: bool) -> f64 {
        let f = self.filament;
        let mut v = want.min(if self.f_m("prime_tower_max_speed") > 0.0 { self.f_m("prime_tower_max_speed") } else { 90.0 });
        if first_layer {
            v = v.min(self.ext_m("initial_layer_speed", self.job.ext_of(f)).max(1.0));
        }
        let vol = self.fil_m("filament_max_volumetric_speed", f);
        if vol > 0.0 && area > 0.0 {
            v = v.min(vol / area);
        }
        v
    }

    /// The adhesion category of a filament (its tower block).
    fn category(&self, f: usize) -> i64 {
        self.job.cfg.i64_at("filament_adhesiveness_category", f)
    }

    /// Whether a change after the current one on this layer lays rows in
    /// block `b` (`ramming_now`: the current change's ramming still to come
    /// counts as later too).
    fn block_used_later(&self, b: usize, include_current_prime: bool) -> bool {
        let Some(t) = self.tower.as_ref() else { return false };
        let start = if include_current_prime { self.change_idx.saturating_sub(1) } else { self.change_idx };
        for (k, &(from, to)) in self.layer_changes.iter().enumerate().skip(start) {
            let current = k + 1 == self.change_idx;
            if !current && self.job.ext_of(from) != self.job.ext_of(to) && t.block_of(self.category(from)) == b {
                return true;
            }
            if t.block_of(self.category(to)) == b {
                return true;
            }
        }
        false
    }

    /// The prime rows of a change to `to` on this tower layer: their Y
    /// centres and whether they start on the left. None without a tower
    /// layer.
    fn prime_plan(&self, to: usize, h: f64) -> Option<(Vec<f64>, bool)> {
        let t = self.tower.as_ref()?;
        let tl = self.tl.as_ref()?;
        let from = self.filament;
        let mut right = tl.dir_right;
        let b = t.block_of(self.category(to));
        let mut cursor = tl.cursor[b];
        if self.job.ext_of(from) != self.job.ext_of(to) {
            // The ramming rows come first; in the same block they come
            // before the prime rows.
            let nr = self.ram_rows(t, from, h);
            if nr % 2 == 1 {
                right = !right;
            }
            if t.block_of(self.category(from)) == b {
                cursor += nr as f64 * (t.rw + t.gap);
            }
        }
        let (x0, x1) = t.row_span(t.lw);
        let np = tw::row_count(self.fil_m("filament_prime_volume", to), tw::bead_area(t.lw, h), x1 - x0).max(1);
        let first = cursor + t.lw / 2.0;
        Some(((0..np).map(|k| first + k as f64 * (t.lw + t.gap)).collect(), right))
    }

    fn ram_rows(&self, t: &Tower, from: usize, h: f64) -> usize {
        let (x0, x1) = t.row_span(t.rw);
        tw::row_count(ram_volume(self.job, from), tw::bead_area(t.rw, h), x1 - x0)
    }

    /// The tower wall, entering at `skip` (left side?, y) through a gap.
    fn tower_wall(&mut self, lz: f64, first: bool, h: f64, skip: Option<(bool, f64)>, layer_start: bool) {
        let Some(t) = self.tower.clone() else { return };
        self.push("; WIPE_TOWER_START");
        let runs = t.wall(skip, 2.5, lz);
        let speed = self.tower_cap(90.0, tw::bead_area(t.lw, h), first);
        for (i, r) in runs.iter().enumerate() {
            if i == 0 && self.tower_empty && !layer_start && self.role == Role::PrimeTower && self.retracted <= 0.0 && self.xy_known && self.z_known {
                // On from the grid: a plain move inside the tower.
                let ta = self.travel_accel(first);
                self.set_accel(ta);
                self.tower_move(r[0].x_mm(), r[0].y_mm(), None);
            } else if i == 0 {
                self.travel_to(r[0], lz, first, layer_start, Role::PrimeTower);
            } else {
                // Across the gap: retracted, slowly.
                let len = self.retract_len();
                self.push(&format!("G1 E-{} F{}", fmt::e(len), fmt::f(self.retract_speed() * 60.0)));
                self.retracted = len;
                self.feed = self.retract_speed() * 60.0;
                let ta = self.travel_accel(first);
                self.set_accel(ta);
                self.tower_move(r[0].x_mm(), r[0].y_mm(), Some(600.0));
            }
            self.tower_extrude(r, t.lw, h, speed, first);
        }
        if first {
            // A brim round the tower on the first layer.
            let height = self.job.cfg.f64("prime_tower_brim_width");
            let bw = if height >= 0.0 { height } else { (t.last_layer as f64 * self.height.max(0.05) / 100.0 * 8.0).min(8.0) };
            let loops = (bw / t.lw - 1e-9).ceil().max(0.0) as usize;
            for k in 1..=loops {
                let o = k as f64 * t.lw;
                for e in t.outline(o - t.lw / 2.0, lz) {
                    let mut r = e.contour.pts.clone();
                    if r.len() < 3 {
                        continue;
                    }
                    r.push(r[0]);
                    self.travel_to(r[0], lz, first, false, Role::PrimeTower);
                    self.tower_extrude(&r, t.lw, h, speed, first);
                }
            }
        }
        self.push("; WIPE_TOWER_END");
        if let Some(tl) = self.tl.as_mut() {
            tl.wall_done = true;
        }
    }

    /// Ramming rows of the outgoing filament, left to right first.
    fn tower_ram(&mut self, from: usize, lz: f64, first: bool, h: f64, layer_start: bool) {
        let Some(t) = self.tower.clone() else { return };
        let nr = self.ram_rows(&t, from, h);
        if nr == 0 {
            return;
        }
        let to_ext = |s: &Self| s.job.ext_of(from);
        let (x0, x1) = t.row_span(t.rw);
        let tl = self.tl.clone().unwrap();
        let bf = t.block_of(self.category(from));
        let mut y = tl.cursor[bf] + t.rw / 2.0;
        let mut right = tl.dir_right;
        let mut pts = Vec::new();
        for k in 0..nr {
            let (a, b) = if right { (x0, x1) } else { (x1, x0) };
            if k > 0 {
                y += t.rw + t.gap;
            }
            pts.push(Pt::from_mm(a, y));
            pts.push(Pt::from_mm(b, y));
            right = !right;
        }
        self.push("; WIPE_TOWER_START");
        self.travel_to(pts[0], lz, first, layer_start, Role::PrimeTower);
        let e_from = to_ext(self);
        let to = self.next_filament_hint;
        self.push(&format!("; NOZZLE_CHANGE_START OF{from} NF{to} ON{e_from} NN{}", self.job.ext_of(to)));
        let rv = self.fil_m("filament_ramming_volumetric_speed", from);
        let vol = if rv > 0.0 { rv } else { self.fil_m("filament_max_volumetric_speed", from) };
        let area = tw::bead_area(t.rw, h);
        let mut speed = if vol > 0.0 { vol / area } else { 60.0 };
        if first {
            speed = speed.min(self.ext_m("initial_layer_speed", e_from).max(1.0));
        }
        self.tower_extrude(&pts, t.rw, h, speed, first);
        self.push(&format!("; NOZZLE_CHANGE_END OF{from} NF{to} ON{e_from} NN{}", self.job.ext_of(to)));
        self.push("M400");
        if let Some(tl) = self.tl.as_mut() {
            tl.cursor[bf] = y - t.rw / 2.0 + t.rw + t.gap;
            tl.dir_right = right;
        }
    }

    /// Prime the new filament: round the tower to the first row, a short
    /// start, a scrub on the spot, then the rows.
    #[allow(clippy::too_many_arguments)]
    fn tower_prime(&mut self, to: usize, ys: &[f64], right: bool, lz: f64, first: bool, h: f64, tc: usize, nozzle_change: bool) {
        let Some(t) = self.tower.clone() else { return };
        let (x0, x1) = t.row_span(t.lw);
        let (wx0, wy0, wx1, _) = t.wall_rect();
        let m = 4.9;
        let xs = if right { x0 } else { x1 };
        let d = if right { 1.0 } else { -1.0 };
        let y = ys[0];
        // The route round the tower, from where the change left the head.
        let mut route: Vec<(f64, f64)> = Vec::new();
        let from_right = !self.xy_known || self.x > wx1;
        if right == from_right {
            // Entering on the far side: round the front.
            let (sx, ex) = if from_right { (wx1 + m, wx0 - m) } else { (wx0 - m, wx1 + m) };
            let cy = if self.xy_known { self.y } else { y };
            route.push((sx, cy));
            route.push((sx, wy0 - m));
            route.push((ex, wy0 - m));
            route.push((ex, y));
        } else {
            route.push((if right { wx0 - m } else { wx1 + m }, y));
        }
        let tf = self.travel_speed();
        let ta = self.travel_accel(first);
        self.set_accel(ta);
        self.lift_if_z_unknown(lz);
        for (rx, ry) in &route {
            self.tower_move(*rx, *ry, Some(tf));
        }
        self.xy_known = true;
        self.tower_move(xs, y, Some(tf));
        self.push_words("G1", &[("Z", lz, 3)]);
        self.z = lz;
        // Only a previously parked head needs its tool-change retraction pushed back.
        let e = self.job.ext_of(to);
        let parked_only = self.b_m("fab_toolchange_prime_parked_only");
        let tc_len = if parked_only { std::mem::take(&mut self.parked_prime[e]) } else {
            self.ext_m("retract_length_toolchange", e).max(self.retracted) + self.ext_m("retract_restart_extra_toolchange", e)
        };
        if !parked_only || tc_len > 0.0 {
            self.push(&format!("G1 E{} F{}", fmt::e(tc_len), fmt::f(self.deretract_speed() * 60.0)));
            self.feed = self.deretract_speed() * 60.0;
        }
        if tc_len > 0.0 { self.printed_top = self.printed_top.max(self.z); }
        self.retracted = 0.0;
        self.restart_extra = 0.0;
        self.push("G4 S0");
        let line = self.firmware.tower_wipe(tc, nozzle_change);
        self.push(&line);
        let area = tw::bead_area(t.lw, h);
        let sp = |s: &Self, k: usize| s.tower_cap(prime_row_speed(s.job, s.family, k, first, to), area, first);
        let p = xs + 3.0 * d;
        let v0 = sp(self, 0);
        self.tower_extrude(&[Pt::from_mm(xs, y), Pt::from_mm(p, y)], t.lw, h, v0, first);
        // Scrub the nozzle on the spot, retracted.
        let rl = self.eff_m("retraction_length", to);
        if rl > 0.0 {
            self.push(&format!("G1 E-{} F{}", fmt::e(rl), fmt::f(self.retract_speed() * 60.0)));
            self.feed = self.retract_speed() * 60.0;
        }
        self.set_accel(ta);
        self.tower_move(p - 4.5 * d, y, Some(600.0));
        self.tower_move(xs, y, Some(240.0));
        self.tower_move(xs + 0.5, y, Some(600.0));
        let (iy0, iy1) = (wy0 + t.lw, t.wall_rect().3 - t.lw);
        let (mut cx, mut cy) = (xs + 0.5, y);
        for (k, (dx, dy)) in [(0.0, 0.5), (-1.0, 0.0), (0.0, -1.0), (1.5, 0.0), (0.0, 1.5), (-2.0, 0.0), (0.0, -2.0)].iter().enumerate() {
            let (nx, ny) = (cx + dx, cy + dy);
            if ny < iy0 || ny > iy1 {
                break;
            }
            let _ = k;
            self.tower_move(nx, ny, None);
            cx = nx;
            cy = ny;
        }
        let v0f = v0 * 60.0;
        self.tower_move(p, y, Some(v0f));
        if rl > 0.0 {
            self.push(&format!("G1 E{} F{}", fmt::e(rl), fmt::f(self.deretract_speed() * 60.0)));
            self.printed_top = self.printed_top.max(self.z);
            self.feed = self.deretract_speed() * 60.0;
        }
        // The change brought the nozzle to the tower cooler by
        // `filament_cooling_before_tower`: back to the print temperature
        // before the prime lines (not on the first layer, where it was not cooled).
        if !first && self.job.fil("filament_cooling_before_tower", to) > 0.0 {
            let tool = self.firmware.tool(self.job, to);
            let temp = self.cal_temp.filter(|_| self.job.ext_of(to) == self.job.ext_of(self.filament)).unwrap_or_else(|| self.job.fil("nozzle_temperature", to));
            let line = self.firmware.idle_temperature(tool, temp, "Wipe tower reheat before wipe");
            self.push(&line);
        }
        // The rows.
        let mut dir_right = right;
        let mut pts_row0 = vec![Pt::from_mm(p, y), Pt::from_mm(if right { x1 } else { x0 }, y)];
        self.tower_extrude(&pts_row0, t.lw, h, v0, first);
        dir_right = !dir_right;
        pts_row0.clear();
        for (k, &ry) in ys.iter().enumerate().skip(1) {
            let (a, b) = if dir_right { (x0, x1) } else { (x1, x0) };
            let v = sp(self, k);
            // The step to the next row prints at the last row's speed.
            let step = [Pt::from_mm(self.x, self.y), Pt::from_mm(a, ry)];
            let vs = self.feed / 60.0;
            self.tower_extrude(&step, t.lw, h, vs.max(1.0), first);
            self.tower_extrude(&[Pt::from_mm(a, ry), Pt::from_mm(b, ry)], t.lw, h, v, first);
            dir_right = !dir_right;
        }
        self.end_change_speed_guard();
        self.push("; WIPE_TOWER_END");
        let bt = t.block_of(self.category(to));
        if let Some(tl) = self.tl.as_mut() {
            tl.cursor[bt] = ys.last().copied().unwrap_or(y) - t.lw / 2.0 + t.lw + t.gap;
            tl.dir_right = dir_right;
        }
    }

    /// After the layer's last change: every block's empty grid, and the
    /// wall if it is still missing.
    fn tower_finish(&mut self, lz: f64, first: bool, h: f64) {
        self.tower_finish_at(lz, first, h, false);
    }

    fn tower_finish_at(&mut self, lz: f64, first: bool, h: f64, layer_start: bool) {
        let Some(t) = self.tower.clone() else { return };
        let mut started = layer_start;
        for b in 0..t.blocks.len() {
            if self.tower_grid(b, 0.0, lz, first, h, started) {
                started = false;
            }
        }
        if !self.tl.as_ref().map(|tl| tl.wall_done).unwrap_or(true) {
            self.tower_wall(lz, first, h, None, started);
        }
    }

    /// Block `b`'s free space, past `reserve` mm, as an empty grid (once a
    /// layer). Returns whether it printed anything.
    fn tower_grid(&mut self, b: usize, reserve: f64, lz: f64, first: bool, h: f64, layer_start: bool) -> bool {
        let Some(t) = self.tower.clone() else { return false };
        let Some(tl) = self.tl.clone() else { return false };
        if tl.grid_done[b] {
            return false;
        }
        let blk = &t.blocks[b];
        let ya = tl.cursor[b] + reserve + t.lw / 2.0;
        let yb = blk.y0 + blk.depth - t.lw / 2.0;
        if let Some(tl) = self.tl.as_mut() {
            tl.grid_done[b] = true;
        }
        if yb - ya < t.lw * 0.5 {
            return false;
        }
        let rib = { let r = self.f_m("prime_tower_rib_width"); if r > 0.0 { r } else { 8.0 } };
        let (frame, ribs) = t.grid(ya, yb, rib);
        self.push("; WIPE_TOWER_START");
        if !layer_start && self.role == Role::PrimeTower && self.retracted <= 0.0 && self.xy_known && self.z_known {
            // On from the prime block: a plain move inside the tower, at the tower's speed.
            let ta = self.travel_accel(first);
            self.set_accel(ta);
            let v = if self.f_m("prime_tower_max_speed") > 0.0 { self.f_m("prime_tower_max_speed") } else { 90.0 };
            self.tower_move(frame[0].x_mm(), frame[0].y_mm(), Some(v * 60.0));
        } else {
            self.travel_to(frame[0], lz, first, layer_start, Role::PrimeTower);
        }
        let v = if self.tower_empty {
            self.tower_cap(90.0, tw::bead_area(t.lw, h), first)
        } else if self.role == Role::PrimeTower && self.feed > 0.0 {
            (self.feed / 60.0).max(1.0)
        } else {
            self.tower_cap(tw::PRIME_SPEED, tw::bead_area(t.lw, h), first)
        };
        self.tower_extrude(&frame, t.lw, h, v, first);
        for (a, c) in ribs {
            if self.tower_empty {
                let ta = self.travel_accel(first);
                self.set_accel(ta);
            }
            self.tower_move(a.x_mm(), a.y_mm(), None);
            self.tower_extrude(&[a, c], t.lw, h, v, first);
        }
        self.push("; WIPE_TOWER_END");
        true
    }

    /// Retract by `len`, wiping first.
    fn retract_by(&mut self, len: f64, first_layer: bool) {
        if self.retracted > 0.0 {
            return;
        }
        let saved = self.eff_m("retraction_length", self.filament);
        self.retract_override = Some(len);
        self.retract(first_layer);
        self.retract_override = None;
        let _ = saved;
    }
}

/// Bridge percentages use the resolved outer-wall acceleration of this extruder.
fn bridge_accel(job: &Job, e: usize, outer: f64, default: f64) -> f64 {
    if !job.cfg.has("bridge_acceleration") { return default; }
    let v = job.ext("bridge_acceleration", e);
    let v = if job.cfg.raw("bridge_acceleration", e).ends_with('%') { outer * v / 100.0 } else { v };
    if v > 0.0 { v } else { default }
}

/// An object's accelerations, resolved with its own settings, for each extruder: a painted
/// object prints through more than one.
struct ObjectAccels(Vec<Accels>);

struct Accels {
    default: f64,
    outer: f64,
    inner: f64,
    top: f64,
    sparse: f64,
    bridge: f64,
}

impl ObjectAccels {
    fn of(job: &Job, o: &makepad_fab_toolpath::ObjectInfo) -> Option<ObjectAccels> {
        const KEYS: [&str; 6] = ["default_acceleration", "outer_wall_acceleration", "inner_wall_acceleration", "top_surface_acceleration", "sparse_infill_acceleration", "bridge_acceleration"];
        if !o.overrides.iter().any(|(k, _)| KEYS.contains(&k.as_str())) {
            return None;
        }
        let own = job.with_overrides(&o.overrides);
        let at = |e: usize| {
            let default = own.ext("default_acceleration", e);
            let read = |key: &str| {
                let v = own.ext(key, e);
                let v = if own.cfg.raw(key, e).ends_with('%') { default * v / 100.0 } else { v };
                if v > 0.0 { v } else { default }
            };
            let outer = read("outer_wall_acceleration");
            Accels { default, outer, inner: read("inner_wall_acceleration"), top: read("top_surface_acceleration"), sparse: read("sparse_infill_acceleration"), bridge: bridge_accel(&own, e, outer, default) }
        };
        Some(ObjectAccels((0..own.extruders()).map(at).collect()))
    }

    /// The acceleration of `role` on extruder `e`.
    fn for_role(&self, role: Role, e: usize) -> f64 {
        let a = &self.0[e.min(self.0.len() - 1)];
        match role {
            Role::Bridge | Role::InternalBridge => a.bridge,
            Role::OuterWall | Role::OverhangWall => a.outer,
            Role::InnerWall => a.inner,
            Role::TopSurface => a.top,
            Role::SparseInfill => a.sparse,
            _ => a.default,
        }
    }
}

struct Skirt {
    /// Outermost first, without closing points.
    loops: Vec<Polygon>,
    width: f64,
    /// Zero on merged layers that have no skirt.
    heights: Vec<f64>,
}

impl Skirt {
    fn layer_time(&self, job: &Job, li: usize, f: usize, first: bool) -> f64 {
        let h = self.heights[li];
        if h <= 0.0 { return 0.0; }
        let nozzle = job.nozzle_diameter(job.ext_of(f)).max(0.1);
        let gap = job.cfg.abs_or_pct("seam_gap", 0, nozzle).max(0.0);
        let length: f64 = self.loops.iter().map(|r| (r.perimeter() / SCALE - gap).max(0.0)).sum();
        length / skirt_speed(job, f, first, flow::cross_section(self.width, h))
    }
}

fn skirt_speed(job: &Job, f: usize, first: bool, area: f64) -> f64 {
    let mut speed = job.ext(if first { "initial_layer_speed" } else { "support_speed" }, job.ext_of(f)).max(1e-6);
    let vol = job.fil("filament_max_volumetric_speed", f);
    if vol > 0.0 && area > 0.0 { speed = speed.min(vol / area); }
    speed
}

/// The ribbed tower footprint including its first-layer brim.
fn tower_first_footprint(job: &Job, t: &Tower) -> Vec<Pt> {
    let bw = { let b = job.cfg.f64("prime_tower_brim_width"); if b >= 0.0 { b } else { (t.last_layer as f64 * job.f("layer_height").max(0.05) / 100.0 * 8.0).min(8.0) } };
    let loops = (bw / t.lw - 1e-9).ceil().max(0.0);
    let pts: Vec<Pt> = t.outline(loops * t.lw, t.z0).into_iter().flat_map(|s| s.contour.pts).collect();
    if !pts.is_empty() { return pts; }
    let b = t.bbox();
    Polygon::rect(Pt::from_mm(b[0], b[1]), Pt::from_mm(b[2], b[3])).pts
}

/// One hull around the skirt layers, supports, brim and tower.
fn plan_skirt(job: &Job, tp: &Toolpaths, tower: Option<&Tower>, f: usize) -> Option<Skirt> {
    let n = job.cfg.i64_at("skirt_loops", 0).max(0) as usize;
    let height = job.cfg.i64_at("skirt_height", 0).max(0) as usize;
    let shield = job.s("draft_shield");
    if n == 0 || (height == 0 && shield == "disabled") || tp.layers.is_empty() || (job.s("print_sequence") == "by object" && tp.objects.len() > 1) { return None; }
    let infinite = shield == "enabled";
    let mut heights = vec![0.0; tp.layers.len()];
    let (mut count, mut last, mut z) = (0, 0, 0.0);
    for (li, layer) in tp.layers.iter().enumerate() {
        if li != 0 && !layer.objects.iter().any(|o| !o.support_only && o.has_paths()) { continue; }
        heights[li] = if li == 0 { layer.height } else { layer.print_z - z };
        z = layer.print_z;
        last = li;
        count += 1;
        if !infinite && count >= height.max(1) { break; }
    }
    let cutoff = tp.layers[last].print_z;
    let hulls = makepad_fab_toolpath::par::map(last + 1, |li| {
        let layer = &tp.layers[li];
        let mut pts = Vec::new();
        if layer.print_z <= cutoff {
            for ol in &layer.objects {
                pts.extend(ol.slices.iter().flat_map(|s| s.contour.pts.iter().copied()));
                for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                    if matches!(p.role, Role::Support | Role::SupportInterface | Role::SupportTransition) || (li == 0 && p.role == Role::Brim) {
                        pts.extend_from_slice(&p.pts);
                    }
                }
            }
        }
        convex_hull(&mut pts)
    });
    let mut pts: Vec<Pt> = hulls.into_iter().flatten().collect();
    if let Some(t) = tower {
        pts.extend(tower_first_footprint(job, t));
    }
    let hull = convex_hull(&mut pts);
    skirt_from_hull(job, hull, f, tp.layers[0].height, n, heights)
}

fn plan_object_skirt(job: &Job, layer: &Layer, object: usize, blocks: &[NpBlock], f: usize) -> Option<Skirt> {
    let n = sequential::skirt_loops(job);
    if n == 0 || sequential::has_brim(layer, object) { return None; }
    let mut pts: Vec<Pt> = layer.objects.iter().flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)).flat_map(|p| p.pts.iter().copied()).collect();
    pts.extend(blocks.iter().filter(|b| b.layer == layer.index && b.object == object).flat_map(|b| &b.paths).flat_map(|p| p.pts.iter().copied()));
    let hull = convex_hull(&mut pts);
    skirt_from_hull(job, hull, f, layer.height, n, vec![layer.height])
}

fn skirt_from_hull(job: &Job, hull: Vec<Pt>, f: usize, h: f64, n: usize, heights: Vec<f64>) -> Option<Skirt> {
    use makepad_fab_slicer::offset::{offset_polygons, Join};
    if hull.len() < 3 { return None; }
    let width = sequential::skirt_width(job, f);
    let spacing = flow::spacing(width, h);
    let hull = [Polygon::new(hull)];
    let mut loops = Vec::new();
    for i in (1..=n).rev() {
        let d = job.f("skirt_distance") + (i as f64 - 0.5) * spacing;
        for ex in offset_polygons(&hull, d * SCALE, Join::Round(0.1 * SCALE)) {
            let mut ring = Polygon::new(simplify_ring(&ex.contour.pts, 0.05 * SCALE));
            if ring.len() < 3 { continue; }
            if ring.is_ccw() == job.b("print_in_clockwise") { ring.reverse(); }
            loops.push(ring);
        }
    }
    if loops.is_empty() { None } else { Some(Skirt { loops, width, heights }) }
}

/// Print time of a tower layer with `changes` on it, as the writer lays it
/// out: per change the outgoing nozzle's ramming rows, the scrub and the
/// new filament's prime rows (slow while the nozzle reheats), then every
/// block's empty grid over the depth left, and the wall. The cooling time
/// of the layer counts this and nothing for the change itself: in the
/// reference the fan of a change layer follows the layer's moves alone
/// (a PETG layer of 20.6 s of moves with a change takes the fan of 21.0 to
/// 21.7 s, not of the 5.6 s switch on top).
fn tower_layer_time(job: &Job, family: &family::Family, t: &Tower, changes: &[(usize, usize)], f: usize, first: bool, h: f64, z: f64) -> f64 {
    if changes.is_empty() {
        return tower_empty_time(job, t, f, first, h, z);
    }
    let cap = |want: f64, area: f64, f: usize| -> f64 {
        let mut v = want.min(if job.f("prime_tower_max_speed") > 0.0 { job.f("prime_tower_max_speed") } else { 90.0 });
        if first {
            v = v.min(job.ext("initial_layer_speed", job.ext_of(f)).max(1.0));
        }
        let vol = job.fil("filament_max_volumetric_speed", f);
        if vol > 0.0 && area > 0.0 {
            v = v.min(vol / area);
        }
        v.max(1.0)
    };
    let category = |f: usize| job.cfg.i64_at("filament_adhesiveness_category", f);
    let mut cursor: Vec<f64> = t.blocks.iter().map(|b| b.y0).collect();
    let mut time = 0.0;
    let mut last = f;
    for &(from, to) in changes {
        if job.ext_of(from) != job.ext_of(to) {
            // Ramming: rows of the ramming bead joined end to end, at the ramming flow.
            let (x0, x1) = t.row_span(t.rw);
            let area = tw::bead_area(t.rw, h);
            let nr = tw::row_count(ram_volume(job, from), area, x1 - x0);
            if nr > 0 {
                let rv = job.fil("filament_ramming_volumetric_speed", from);
                let vol = if rv > 0.0 { rv } else { job.fil("filament_max_volumetric_speed", from) };
                let mut v = if vol > 0.0 { vol / area } else { 60.0 };
                if first {
                    v = v.min(job.ext("initial_layer_speed", job.ext_of(from)).max(1.0));
                }
                let len = nr as f64 * (x1 - x0) + (nr - 1) as f64 * (t.rw + t.gap);
                time += len / v.max(1.0);
                cursor[t.block_of(category(from))] += nr as f64 * (t.rw + t.gap);
            }
        }
        // Priming: the 3 mm start, the scrub on the spot (4.5 mm out at 10 mm/s,
        // 1.5 back at 4 mm/s, 0.5 on and a 9.5 mm square spiral at 10), back to
        // the start, then the rows.
        let (x0, x1) = t.row_span(t.lw);
        let area = tw::bead_area(t.lw, h);
        let np = tw::row_count(job.fil("filament_prime_volume", to), area, x1 - x0).max(1);
        let speed = |k: usize| cap(prime_row_speed(job, family, k, first, to), area, to);
        time += 3.0 / speed(0) + 4.5 / 10.0 + 1.5 / 4.0 + 10.0 / 10.0 + 2.5 / speed(0);
        time += (x1 - x0 - 3.0).max(0.0) / speed(0);
        for k in 1..np {
            time += (t.lw + t.gap) / speed(k - 1) + (x1 - x0) / speed(k);
        }
        cursor[t.block_of(category(to))] += np as f64 * (t.lw + t.gap);
        last = to;
    }
    let len = |pts: &[Pt]| pts.windows(2).map(|w| w[0].dist(w[1])).sum::<f64>() / SCALE;
    let rib = { let r = job.f("prime_tower_rib_width"); if r > 0.0 { r } else { 8.0 } };
    let grid_v = cap(tw::PRIME_SPEED, tw::bead_area(t.lw, h), last);
    for (b, blk) in t.blocks.iter().enumerate() {
        let (ya, yb) = (cursor[b] + t.lw / 2.0, blk.y0 + blk.depth - t.lw / 2.0);
        if yb - ya < t.lw * 0.5 {
            continue;
        }
        let (frame, ribs) = t.grid(ya, yb, rib);
        // The frame, the ribs and the moves between them, all at the grid's speed.
        let mut moves = 0.0;
        let mut at = frame.last().copied().unwrap_or_default();
        for (a, c) in &ribs {
            moves += at.dist(*a) / SCALE + a.dist(*c) / SCALE;
            at = *c;
        }
        time += (len(&frame) + moves) / grid_v;
    }
    let wall: f64 = t.wall(None, 2.5, z).iter().map(|r| len(r)).sum();
    time + wall / cap(90.0, tw::bead_area(t.lw, h), t.wall_filament)
}

/// Print time of the tower's layer when no change happens on it: every
/// block's empty grid and the wall, in filament `f`.
fn tower_empty_time(job: &Job, t: &Tower, f: usize, first: bool, h: f64, z: f64) -> f64 {
    let area = tw::bead_area(t.lw, h);
    let cap = |want: f64| -> f64 {
        let mut v = want.min(if job.f("prime_tower_max_speed") > 0.0 { job.f("prime_tower_max_speed") } else { 90.0 });
        if first {
            v = v.min(job.ext("initial_layer_speed", job.ext_of(f)).max(1.0));
        }
        let vol = job.fil("filament_max_volumetric_speed", f);
        if vol > 0.0 {
            v = v.min(vol / area);
        }
        v.max(1.0)
    };
    let len = |pts: &[Pt]| pts.windows(2).map(|w| w[0].dist(w[1])).sum::<f64>() / SCALE;
    let rib = { let r = job.f("prime_tower_rib_width"); if r > 0.0 { r } else { 8.0 } };
    let mut time = 0.0;
    for b in &t.blocks {
        let (ya, yb) = (b.y0 + t.lw / 2.0, b.y0 + b.depth - t.lw / 2.0);
        if yb - ya < t.lw * 0.5 {
            continue;
        }
        let (frame, ribs) = t.grid(ya, yb, rib);
        let l = len(&frame) + ribs.iter().map(|(a, c)| a.dist(*c) / SCALE).sum::<f64>();
        time += l / cap(tw::PRIME_SPEED);
    }
    let wall: f64 = t.wall(None, 2.5, z).iter().map(|r| len(r)).sum();
    time + wall / cap(90.0)
}

/// The speed cap that makes a layer last at least `target` seconds.
fn cooling_cap(paths: &[(f64, f64)], fixed: f64, target: f64, min_speed: f64) -> Option<f64> {
    let time = |cap: f64| -> f64 { fixed + paths.iter().map(|(l, v)| l / v.min(cap).max(1e-6)).sum::<f64>() };
    let vmax = paths.iter().map(|p| p.1).fold(0.0, f64::max);
    if paths.is_empty() || time(vmax) >= target {
        return None;
    }
    if time(min_speed) <= target {
        return Some(min_speed);
    }
    let (mut lo, mut hi) = (min_speed, vmax);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if time(mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

/// What to emit on a layer: the ordered paths with their object.
struct Planned<'p> {
    object: usize,
    path: &'p ExtrusionPath,
    reverse: bool,
    seat: Option<u32>,
}

impl Planned<'_> {
    fn first(&self) -> Pt {
        if let Some(i) = self.seat { return loop_ring(self.path)[i as usize]; }
        if self.reverse { self.path.last() } else { self.path.first() }
    }

    fn last(&self) -> Pt {
        if let Some(i) = self.seat {
            let ring = loop_ring(self.path);
            let mut gap = loop_gap(self.path);
            let mut j = i as usize;
            for _ in 0..ring.len() {
                if gap <= 0.0 { return ring[j]; }
                let k = (j + ring.len() - 1) % ring.len();
                let len = ring[k].dist(ring[j]);
                if len > gap { return lerp(ring[k], ring[j], (len - gap) / len); }
                gap -= len;
                j = k;
            }
            return ring[i as usize];
        }
        if self.reverse { self.path.first() } else { self.path.last() }
    }

    fn key(&self) -> (usize, bool, Option<u32>) {
        (self.path as *const ExtrusionPath as usize, self.reverse, self.seat)
    }

    fn oriented(&self) -> ExtrusionPath {
        let mut path = self.path.clone();
        if let Some(i) = self.seat {
            let ring = loop_ring(self.path);
            path.loop_pts = (0..=ring.len()).map(|k| ring[(i as usize + k) % ring.len()]).collect();
            let mut pl = Polyline::new(path.loop_pts.clone());
            pl.clip_end(loop_gap(self.path));
            path.pts = pl.pts;
        } else if self.reverse {
            path.pts.reverse();
            path.z.reverse();
            path.flow.reverse();
        }
        path
    }
}

fn loop_ring(path: &ExtrusionPath) -> &[Pt] {
    let pts = &path.loop_pts;
    &pts[..pts.len() - usize::from(pts.len() > 1 && pts.first() == pts.last())]
}

/// The gap already removed by the planner, in units.
fn loop_gap(path: &ExtrusionPath) -> f64 {
    let ring = loop_ring(path);
    let len: f64 = (0..ring.len()).map(|i| ring[i].dist(ring[(i + 1) % ring.len()])).sum();
    (len - path.pts.windows(2).map(|p| p[0].dist(p[1])).sum::<f64>()).max(0.0)
}

fn nearest_vertex(ring: &[Pt], from: Pt) -> usize {
    (0..ring.len()).min_by_key(|&i| ring[i].dist2(from)).unwrap_or(0)
}

/// Balance corners and travel, with nozzle-length arms traversed counter-clockwise.
fn nearest_seam(ring: &[Pt], from: Pt, nozzle: f64) -> Pt {
    let n = ring.len();
    let ccw = (0..n).map(|i| cross(ring[i], ring[(i + 1) % n])).sum::<i128>() > 0;
    let bump = |x: f64, f: f64| (1.0 / (f * x * x + 1.0)).exp_m1() / (std::f64::consts::E - 1.0);
    let mut best = (0, f64::MAX);
    for i in 0..n {
        let arm = |forward: bool| {
            let (mut j, mut length) = (i, 0.0);
            for _ in 1..n {
                let k = if forward { (j + 1) % n } else { (j + n - 1) % n };
                length += ring[j].dist(ring[k]);
                j = k;
                if length >= nozzle * SCALE { break; }
            }
            ring[j]
        };
        let (u, v) = (ring[i].sub(arm(!ccw)), arm(ccw).sub(ring[i]));
        let a = (cross(u, v) as f64).atan2(dot(u, v) as f64);
        let score = bump(a, 3.0) + 1.0 / (2.0 + (-a).exp()) + 1.0 - bump(ring[i].dist(from) / SCALE, 0.005);
        if score < best.1 { best = (i, score); }
    }
    ring[best.0]
}

/// One object's brim paths, retaining the kernel's loop order.
fn order_brim<'p>(ol: &'p makepad_fab_toolpath::ObjectLayer, cur: &mut Pt, out: &mut Vec<Planned<'p>>) {
    for path in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| p.role == Role::Brim) {
        out.push(Planned { object: ol.object, path, reverse: false, seat: None });
        *cur = path.last();
    }
}

/// One object's remaining paths after the layer's brims, islands nearest
/// first; walls in their order, fills nearest-next. Open sparse chains may
/// start at either end; carry that direction into travel estimates,
/// extrusion and wiping.
fn order_object<'p>(ol: &'p makepad_fab_toolpath::ObjectLayer, cur: &mut Pt, out: &mut Vec<Planned<'p>>, nearest: Option<f64>) {
    let n = ol.islands.len();
    let mut done = vec![false; n];
    for _ in 0..n {
        let mut best = None;
        let mut bd = i128::MAX;
        for (k, isl) in ol.islands.iter().enumerate() {
            if done[k] {
                continue;
            }
            match isl.walls.iter().chain(&isl.fills).find(|p| p.role != Role::Brim).map(|p| p.first()) {
                Some(p0) => {
                    let d = p0.dist2(*cur);
                    if d < bd {
                        bd = d;
                        best = Some(k);
                    }
                }
                None if best.is_none() => best = Some(k),
                None => {}
            }
        }
        let Some(k) = best else { break };
        done[k] = true;
        let isl = &ol.islands[k];
        let seam = nearest.and_then(|nozzle| {
            if !isl.walls.iter().filter(|p| p.role != Role::Brim).all(|p| p.closed && p.dry.is_empty() && p.z.is_empty() && p.flow.is_empty() && p.mode == 0 && p.loop_pts.len() >= 4) { return None; }
            isl.walls.iter().find(|p| p.role == Role::OuterWall).map(|p| nearest_seam(loop_ring(p), *cur, nozzle))
        });
        for w in isl.walls.iter().filter(|p| p.role != Role::Brim) {
            let seat = seam.map(|p| nearest_vertex(loop_ring(w), p) as u32);
            let planned = Planned { object: ol.object, path: w, reverse: false, seat };
            *cur = planned.last();
            out.push(planned);
        }
        let mut used = vec![false; isl.fills.len()];
        // Arc-overhang fronts print after the island's other fills, in their
        // own order (each front leans on the one before); ironing passes
        // over the finished top last.
        let is_front = |f: &ExtrusionPath| f.mode == makepad_fab_toolpath::exp::mode::ARC_FRONT;
        let phased = isl.fills.iter().any(|f| f.role == Role::Ironing || is_front(f));
        let phase_of = |f: &ExtrusionPath| if f.role == Role::Ironing { 2 } else if is_front(f) { 1 } else { 0 };
        for phase in 0..if phased { 3 } else { 1 } {
            if phase == 1 {
                for (j, f) in isl.fills.iter().enumerate() {
                    if !used[j] && is_front(f) && f.role != Role::Brim {
                        used[j] = true;
                        let planned = Planned { object: ol.object, path: f, reverse: false, seat: None };
                        *cur = planned.last();
                        out.push(planned);
                    }
                }
                continue;
            }
            for _ in 0..isl.fills.len() {
                let mut bi = None;
                let mut bd = i128::MAX;
                let mut reverse = false;
                for (j, f) in isl.fills.iter().enumerate() {
                    if used[j] || f.role == Role::Brim || (phased && phase_of(f) != phase) {
                        continue;
                    }
                    let d0 = f.first().dist2(*cur);
                    let d1 = if f.role == Role::SparseInfill && !f.closed && f.dry.is_empty() { f.last().dist2(*cur) } else { i128::MAX };
                    let d = d0.min(d1);
                    if d < bd {
                        bd = d;
                        bi = Some(j);
                        reverse = d1 < d0;
                    }
                }
                let Some(j) = bi else { break };
                used[j] = true;
                let planned = Planned { object: ol.object, path: &isl.fills[j], reverse, seat: None };
                *cur = planned.last();
                out.push(planned);
            }
        }
    }
}

/// The filaments a layer prints, in order of first use.
fn layer_filaments(layer: &Layer) -> Vec<usize> {
    let mut fils: Vec<usize> = Vec::new();
    for ol in layer.objects.iter().filter(|o| o.has_paths()) {
        if !fils.contains(&ol.filament) {
            fils.push(ol.filament);
        }
    }
    fils
}

/// The order a layer prints its filaments (`present`) in: each nozzle's together, the nozzle
/// printing now (`loaded`'s) first, so the head switches at most once a nozzle; on each nozzle
/// the order whose changes purge least from the filament it holds (`held`), counting the next
/// layer's first change on it (`next`: the filaments that layer prints), so a nozzle ends on one
/// the next layer prints and that layer starts on it with no change. Each change counts its
/// purge (`changes`) and one mm³ more, so where changes purge nothing the fewest win; ties keep
/// the order of first use.
fn filament_order(present: &[usize], loaded: usize, held: &[Option<usize>], next: &[usize], changes: &ChangeModel) -> Vec<usize> {
    let mut nozzles: Vec<usize> = vec![changes.ext_of(loaded)];
    for &f in present {
        if !nozzles.contains(&changes.ext_of(f)) {
            nozzles.push(changes.ext_of(f));
        }
    }
    let cost = |from: Option<usize>, to: usize| if from == Some(to) { 0.0 } else { changes.change(from, to).purge_mm3 + 1.0 };
    let mut out = Vec::with_capacity(present.len());
    for e in nozzles {
        let fs: Vec<usize> = present.iter().copied().filter(|&f| changes.ext_of(f) == e).collect();
        let then: Vec<usize> = next.iter().copied().filter(|&f| changes.ext_of(f) == e).collect();
        let from = held.get(e).copied().flatten();
        let total = |order: &[usize]| -> f64 {
            let mut at = from;
            let mut sum = 0.0;
            for &f in order {
                sum += cost(at, f);
                at = Some(f);
            }
            match at {
                Some(last) if !then.is_empty() && !then.contains(&last) => sum + then.iter().map(|&f| cost(Some(last), f)).fold(f64::INFINITY, f64::min),
                _ => sum,
            }
        };
        if fs.len() <= 1 {
            out.extend(fs);
        } else if fs.len() <= 6 {
            // every order (at most 720), in lexicographic order of first use: the first best wins
            let mut idx: Vec<usize> = (0..fs.len()).collect();
            let mut best = (total(&fs), fs.clone());
            while next_permutation(&mut idx) {
                let order: Vec<usize> = idx.iter().map(|&i| fs[i]).collect();
                let c = total(&order);
                if c < best.0 {
                    best = (c, order);
                }
            }
            out.extend(best.1);
        } else {
            // the cheapest change each time
            let (mut left, mut at) = (fs, from);
            while !left.is_empty() {
                let i = (0..left.len()).min_by(|&a, &b| cost(at, left[a]).total_cmp(&cost(at, left[b]))).unwrap();
                let f = left.remove(i);
                out.push(f);
                at = Some(f);
            }
        }
    }
    out
}

/// What a print's filament changes come to, as [`ChangePlan`] makes them: what the time model
/// charges for them (the purges at their flush speeds and the profile's switch, load and unload
/// times), the same for every nozzle choice weighed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ChangeCount {
    /// Changes where a nozzle takes another filament out.
    pub swaps: usize,
    /// Changes onto another nozzle.
    pub head_switches: usize,
    /// The purges' volume (mm³) and mass (g).
    pub purge_mm3: f64,
    pub purge_g: f64,
    /// The changes' time (s).
    pub seconds: f64,
}

/// The rule a print's filaments change by: each layer's in [`filament_order`] from what the
/// nozzles hold, and what each change costs. The writer prints by it, and a choice of nozzles is
/// weighed by it ([`plan_changes`]), so the two never disagree.
#[derive(Clone, Debug)]
pub struct ChangePlan {
    loaded: usize,
    held: Vec<Option<usize>>,
    pub count: ChangeCount,
}

impl ChangePlan {
    /// Printing `first`, which the start sequence loaded.
    pub fn new(changes: &ChangeModel, first: usize) -> Self {
        let mut held = vec![None; changes.nozzles()];
        if let Some(h) = held.get_mut(changes.ext_of(first)) {
            *h = Some(first);
        }
        ChangePlan { loaded: first, held, count: ChangeCount::default() }
    }

    /// The filament printing now.
    pub fn loaded(&self) -> usize {
        self.loaded
    }

    /// The filament each nozzle holds.
    pub fn held(&self) -> &[Option<usize>] {
        &self.held
    }

    /// The order a layer prints its filaments (`present`) in; `next`: the next layer's.
    pub fn order(&self, changes: &ChangeModel, present: &[usize], next: &[usize]) -> Vec<usize> {
        filament_order(present, self.loaded, &self.held, next, changes)
    }

    /// The print goes on with `f`: a change unless it prints with it now.
    pub fn take(&mut self, changes: &ChangeModel, f: usize) {
        if f == self.loaded {
            return;
        }
        let e = changes.ext_of(f);
        if e != changes.ext_of(self.loaded) {
            self.count.head_switches += 1;
            self.count.seconds += changes.switch_s;
        }
        if self.held.len() <= e {
            self.held.resize(e + 1, None);
        }
        let held = self.held[e];
        if held != Some(f) {
            self.count.seconds += changes.load_s + if held.is_some() { changes.unload_s } else { 0.0 };
        }
        if held.is_some_and(|h| h != f) {
            let c = changes.change(held, f);
            self.count.swaps += 1;
            self.count.purge_mm3 += c.purge_mm3;
            self.count.purge_g += changes.grams(f, c.purge_mm3);
            self.count.seconds += c.purge_s;
        }
        self.held[e] = Some(f);
        self.loaded = f;
    }
}

/// Each layer's filaments in order of first use: the sets the writer plans its changes over.
pub fn layer_sets(tp: &Toolpaths) -> Vec<Vec<usize>> {
    tp.layers.iter().map(layer_filaments).collect()
}

/// The changes a print of `sets` ([`layer_sets`]) makes with its filaments on the nozzles
/// `changes` puts them on, from the first layer's first filament.
pub fn plan_changes(sets: &[Vec<usize>], changes: &ChangeModel) -> ChangeCount {
    let Some(&first) = sets.iter().find_map(|s| s.first()) else { return ChangeCount::default() };
    let mut plan = ChangePlan::new(changes, first);
    for (i, set) in sets.iter().enumerate() {
        for f in plan.order(changes, set, sets.get(i + 1).map_or(&[][..], |n| n.as_slice())) {
            plan.take(changes, f);
        }
    }
    plan.count
}

/// The next lexicographic permutation of `v` in place; false (and `v` unchanged) after the last.
fn next_permutation(v: &mut [usize]) -> bool {
    let Some(i) = (1..v.len()).rev().find(|&i| v[i - 1] < v[i]) else { return false };
    let j = (i..v.len()).rev().find(|&j| v[j] > v[i - 1]).unwrap();
    v.swap(i - 1, j);
    v[i..].reverse();
    true
}

/// A layer's paths grouped by filament, in the order `fils` gives (its [`filament_order`]).
/// All brims precede model paths, including painted walls on other filaments; an object's brim
/// uses its base filament.
fn order_layer<'p>(layer: &'p Layer, start: Pt, fils: Vec<usize>, nearest: Option<&Job>) -> Vec<(usize, Vec<Planned<'p>>)> {
    let mut fils = fils;
    let mut cur = start;
    let mut out: Vec<(usize, Vec<Planned<'p>>)> = Vec::new();
    for &f in &fils {
        let mut paths = Vec::new();
        for ol in layer.objects.iter().filter(|ol| ol.filament == f) {
            order_brim(ol, &mut cur, &mut paths);
        }
        if !paths.is_empty() {
            out.push((f, paths));
        }
    }
    // Continue the model on the filament that printed the last brim.
    if let Some((last, _)) = out.last() {
        let i = fils.iter().position(|f| f == last).unwrap();
        let f = fils.remove(i);
        fils.insert(0, f);
    }
    let has_brim = !out.is_empty();
    for f in fils {
        let mut paths = Vec::new();
        for ol in layer.objects.iter().filter(|ol| ol.filament == f) {
            order_object(ol, &mut cur, &mut paths, nearest.map(|job| job.nozzle_diameter(job.ext_of(f)).max(0.1)));
        }
        if out.last().is_some_and(|(last, _)| *last == f) {
            out.last_mut().unwrap().1.extend(paths);
        } else if !has_brim || !paths.is_empty() {
            out.push((f, paths));
        }
    }
    out
}

/// Where a path may take a change's purge, by its object's flush options: 0 its sparse infill
/// (`flush_into_infill`), 1 the inside of an object marked for it (`flush_into_objects`: inner
/// walls, sparse and internal solid infill, internal bridges), 2 its support
/// (`flush_into_support`). Never what shows (outer walls, top and bottom skins, bridges, gap
/// fill, ironing), a support interface, a brim, or a 3D or experimental path.
fn sink_rank(path: &ExtrusionPath, sinks: [bool; 3]) -> Option<usize> {
    if !path.z.is_empty() || path.mode != 0 {
        return None;
    }
    match path.role {
        Role::SparseInfill if sinks[0] => Some(0),
        Role::SparseInfill | Role::InnerWall | Role::InternalSolidInfill | Role::InternalBridge if sinks[1] => Some(1),
        Role::Support | Role::SupportTransition if sinks[2] => Some(2),
        _ => None,
    }
}

/// The mm³ a path lays down.
fn path_volume(p: &ExtrusionPath) -> f64 {
    p.pts.windows(2).enumerate()
        .filter(|(i, _)| !p.dry.get(*i).copied().unwrap_or(false))
        .map(|(i, s)| s[0].dist(s[1]) / SCALE * p.area * p.flow.get(i).copied().unwrap_or(1.0))
        .sum()
}

/// A layer's changes print their purge into what doesn't show first ([`sink_rank`]): the paths
/// that may take it, from the changing group on and of a filament of the same type on the same
/// nozzle, move to the front of the changing group (after its brim) until they hold the
/// change's purge: sparse infill first, then marked objects, then support, each from the
/// changing group's own paths first. A group they leave empty goes, and its change with it. A
/// change ahead of another filament's brim takes nothing (brims print before the model).
/// Returns per change, in order, (the filament changed to, mm³ taken): the printer purges the
/// rest.
fn flush_into(groups: &mut Vec<(usize, Vec<Planned<'_>>)>, held: &[Option<usize>], loaded: usize, job: &Job, changes: &makepad_fab_settings::change::ChangeModel, sinks: &[[bool; 3]]) -> Vec<(usize, f64)> {
    let mut held = held.to_vec();
    let mut cur = loaded;
    let mut out = Vec::new();
    let mut gi = 0;
    while gi < groups.len() {
        let to = groups[gi].0;
        if to == cur {
            gi += 1;
            continue;
        }
        let e = job.ext_of(to);
        let purge = changes.change(held.get(e).copied().flatten(), to).purge_mm3;
        if let Some(h) = held.get_mut(e) {
            *h = Some(to);
        }
        cur = to;
        let brims_after = groups[gi + 1..].iter().any(|g| g.1.iter().any(|p| p.path.role == Role::Brim));
        let mut taken = Vec::new();
        let mut took_from = Vec::new();
        let mut volume = 0.0;
        if purge > 0.0 && !brims_after {
            let kind = job.fil_str("filament_type", to).to_string();
            'fill: for rank in (0..3).filter(|&r| sinks.iter().any(|s| s[r])) {
                for g in gi..groups.len() {
                    if volume >= purge {
                        break 'fill;
                    }
                    let f = groups[g].0;
                    if job.ext_of(f) != e || job.fil_str("filament_type", f) != kind {
                        continue;
                    }
                    let paths = std::mem::take(&mut groups[g].1);
                    let mut kept = Vec::with_capacity(paths.len());
                    for p in paths {
                        if volume < purge && sink_rank(p.path, sinks.get(p.object).copied().unwrap_or_default()) == Some(rank) {
                            volume += path_volume(p.path);
                            taken.push(p);
                            if !took_from.contains(&g) {
                                took_from.push(g);
                            }
                        } else {
                            kept.push(p);
                        }
                    }
                    groups[g].1 = kept;
                }
            }
        }
        if !taken.is_empty() {
            let own = &mut groups[gi].1;
            let at = own.iter().position(|p| p.path.role != Role::Brim).unwrap_or(own.len());
            let rest = own.split_off(at);
            own.extend(taken);
            own.extend(rest);
            took_from.sort_unstable();
            for &g in took_from.iter().rev().filter(|&&g| g > gi) {
                if groups[g].1.is_empty() {
                    groups.remove(g);
                }
            }
        }
        out.push((to, volume.min(purge)));
        gi += 1;
    }
    out
}

/// What filament `f` rams on the tower before its head hands over (mm³): the default family's
/// ramming length of filament, and a multitool printer's ramming volume when its multitool
/// ramming is on. Each family's presets set only their own: the other slicer's trees set no
/// ramming length, and the default family's presets have no multitool keys, which read as off.
fn ram_volume(job: &Job, f: usize) -> f64 {
    let multitool = if job.cfg.bool_at("filament_multitool_ramming", f) { job.fil("filament_multitool_ramming_volume", f) } else { 0.0 };
    job.fil("filament_change_length", f) * job.filament_area(f) + multitool
}

/// The speed (mm/s) of prime row `k` after a change to filament `f`, before the tower's and the
/// filament's limits. A family that holds the fifth row prints its rows at set speeds; one that
/// climbs prints them at shares of the layer's target (the first layer's speed, else the lower of
/// the tower's purge speed and the first tool's sparse infill speed), then 50 mm/min faster each
/// row up to the target, each at a whole mm/min.
fn prime_row_speed(job: &Job, family: &family::Family, k: usize, first: bool, f: usize) -> f64 {
    match family.prime_rows {
        family::PrimeRows::HoldFifth => tw::PRIME_ROW_SPEEDS.get(k).copied().unwrap_or(tw::PRIME_SPEED),
        family::PrimeRows::Climb => {
            let or = |v: f64, unset: f64| if v > 0.0 { v } else { unset };
            let target = 60.0 * if first { job.ext("initial_layer_speed", job.ext_of(f)) } else { or(job.f("prime_tower_max_speed"), 90.0).min(or(job.ext("sparse_infill_speed", 0), 80.0)) };
            let feed = match tw::PRIME_ROW_SHARES.get(k) {
                Some(share) => share * target,
                None => (tw::PRIME_ROW_SHARES[3] * target + 50.0 * (k - 3) as f64).min(target),
            };
            feed.round() / 60.0
        }
    }
}

/// Size and place the prime tower: deep enough for the most ramming and
/// priming any layer needs, inside every used nozzle's reach and clear of
/// the objects.
fn plan_tower(job: &Job, tp: &Toolpaths, used: &[usize], changes: &[Vec<(usize, usize)>], last: usize) -> Tower {
    // Rows hold a volume at a bead height: the thinnest step that changes filament sets the depth.
    let step = |i: usize| tp.layers.get(i).map_or(f64::MAX, |l| l.print_z - if i == 0 { 0.0 } else { tp.layers[i - 1].print_z });
    let thinnest = changes.iter().enumerate().skip(1).filter(|(_, c)| !c.is_empty()).map(|(i, _)| step(i)).fold(f64::MAX, f64::min);
    let h = job.f("layer_height").min(thinnest).max(0.05);
    let nd = job.nozzle_diameter(job.ext_of(used[0]));
    let lw = nd * 1.25;
    // Ramming bead width by nozzle size.
    let rw = if nd <= 0.25 { 0.5 } else if nd <= 0.45 { 1.0 } else if nd <= 0.65 { 1.2 } else { 1.4 };
    let gap_pct = { let g = job.cfg.f64("prime_tower_infill_gap"); if g > 0.0 { g } else { 150.0 } };
    let gap = lw * (gap_pct / 100.0 - 1.0).max(0.0);
    let cat = |f: usize| job.cfg.i64_at("filament_adhesiveness_category", f);
    let mut cats: Vec<i64> = used.iter().map(|&f| cat(f)).collect();
    cats.sort();
    cats.dedup();
    let height = tp.layers.get(last).map(|l| l.print_z).unwrap_or(10.0);
    let z0 = tp.layers.first().map_or(0.0, |l| l.print_z);
    // Per category, the depth the busiest layer needs, at a given width.
    let block_depths = |wc: f64| -> Vec<f64> {
        let ar = tw::bead_area(rw, h);
        let ap = tw::bead_area(lw, h);
        let (lr, lp) = (wc - lw - rw, wc - 2.0 * lw);
        let mut depth = vec![0.0f64; cats.len()];
        for c in changes {
            let mut secs = Vec::new();
            for &(from, to) in c {
                if job.ext_of(from) != job.ext_of(to) {
                    let n = tw::row_count(ram_volume(job, from), ar, lr);
                    secs.push(Section { category: cat(from), rows: n, bead: rw });
                }
                secs.push(Section { category: cat(to), rows: tw::row_count(job.fil("filament_prime_volume", to), ap, lp).max(1), bead: lw });
            }
            for (c, d) in tw::layer_depths(&secs, gap) {
                if let Some(i) = cats.iter().position(|&x| x == c) {
                    depth[i] = depth[i].max(d);
                }
            }
        }
        depth
    };
    // A ribbed tower is about square: the narrowest width (in half
    // millimetres) that holds the depth it needs, rows running along it.
    let total = |wc: f64| (block_depths(wc).iter().sum::<f64>() + lw).max(tw::min_depth(height));
    let mut wc = 5.0;
    while wc < 120.0 && total(wc) + 1.5 > wc + 1e-9 {
        wc += 0.5;
    }
    let depths = block_depths(wc);
    let dc = total(wc);
    let (w, d) = (wc + lw, dc + lw);
    // The most used filament prints the wall.
    let mut count = vec![0usize; job.filaments().max(1)];
    for l in &tp.layers {
        let mut fs: Vec<usize> = l.objects.iter().filter(|o| o.has_paths()).map(|o| o.filament).collect();
        fs.sort();
        fs.dedup();
        for f in fs {
            if f < count.len() {
                count[f] += 1;
            }
        }
    }
    let wall_filament = (0..count.len()).max_by_key(|&f| (count[f], usize::MAX - f)).unwrap_or(0);
    let ribs = if job.b("prime_tower_rib_wall") {
        let rw_ = { let r = job.f("prime_tower_rib_width"); if r > 0.0 { r } else { 8.0 } };
        Some((rw_, job.f("prime_tower_extra_rib_length").max(0.0)))
    } else {
        None
    };
    // How far the first layer, its ribs and brim included, stands out of the box on each side
    // (left, front, right, back): all of it must print on the plate.
    let margin = {
        let probe = Tower { x: 0.0, y: 0.0, w, d, last_layer: last, lw, rw, gap, wall_filament, blocks: Vec::new(), ribs, z0, top: height };
        let b = BBox::of(&tower_first_footprint(job, &probe));
        [-b.min.x_mm(), -b.min.y_mm(), b.max.x_mm() - w, b.max.y_mm() - d].map(|m| m.max(0.0))
    };
    // The configured spot, unless a used nozzle cannot reach it or it
    // touches an object.
    // The configured spot is the corner of the ribbed outline, which stands
    // out of the block by about 1.8 mm.
    let mut x = job.cfg.f64("wipe_tower_x") + 1.734;
    let mut y = job.cfg.f64("wipe_tower_y") + 1.83;
    let mut lo = (f64::MIN, f64::MIN);
    let mut hi = (f64::MAX, f64::MAX);
    for &f in used {
        let (a, b) = job.reach(job.ext_of(f));
        lo = (lo.0.max(a.0), lo.1.max(a.1));
        hi = (hi.0.min(b.0), hi.1.min(b.1));
    }
    let back = margin[3].max(12.0);
    let overlaps = |x: f64, y: f64| tp.objects.iter().any(|o| x < o.max[0] + 5.0 && x + w > o.min[0] - 5.0 && y < o.max[1] + 5.0 && y + d > o.min[1] - 5.0);
    let fits = |x: f64, y: f64| x - margin[0].max(2.0) >= lo.0 && x + w + margin[2].max(2.0) <= hi.0 && y - margin[1].max(2.0) >= lo.1 && y + d + back <= hi.1;
    if !fits(x, y) || overlaps(x, y) {
        x = (lo.0 + hi.0) / 2.0 - w / 2.0;
        y = 250.0f64.min(hi.1 - back - d);
        let mut tries = 0;
        while (overlaps(x, y) || !fits(x, y)) && tries < 40 {
            y -= 10.0;
            if y < lo.1 + 2.0 {
                y = hi.1 - back - d;
                x -= 30.0;
            }
            tries += 1;
        }
    }
    let mut blocks = Vec::new();
    let mut yb = y + lw;
    for (i, &c) in cats.iter().enumerate() {
        blocks.push(Block { category: c, y0: yb, depth: depths[i] });
        yb += depths[i];
    }
    // Spare depth (a tall tower's minimum) goes to the last block.
    if let Some(b) = blocks.last_mut() {
        b.depth += (y + d - lw) - yb;
    }
    Tower { x, y, w, d, last_layer: last, lw, rw, gap, wall_filament, blocks, ribs, z0, top: height }
}

/// A line's payload lives in its chunk's shared buffers: no allocation per move.
enum Line {
    Text(std::ops::Range<usize>),
    Words { head: &'static str, words: std::ops::Range<usize> },
}

#[derive(Default)]
struct Chunk {
    lines: Vec<Line>,
    text: String,
    words: Vec<(&'static str, f64, usize)>,
}

struct ReadChunk {
    text: String,
    ends: Vec<usize>,
    ops: Vec<process::Op>,
    nonfinite: usize,
}

impl Chunk {
    /// The same bytes push_words/feed wrote, then the same read-back parser.
    fn read(self) -> ReadChunk {
        fmt::take_nonfinite();
        let mut text = String::with_capacity(self.text.len() + self.lines.len() * 32);
        let mut ends = Vec::with_capacity(self.lines.len());
        for line in self.lines {
            match line {
                Line::Text(range) => text.push_str(&self.text[range]),
                Line::Words { head, words } => {
                    text.push_str(head);
                    for &(k, v, d) in &self.words[words] {
                        text.push(' ');
                        text.push_str(k);
                        fmt::trim_into(&mut text, v, d);
                    }
                }
            }
            text.push('\n');
            ends.push(text.len());
        }
        let nonfinite = fmt::take_nonfinite();
        let mut ops = Vec::with_capacity(text.len() / 24);
        { let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeReadBack); makepad_fab_toolpath::par::plate_span("gcode", || process::parse_chunk(&text, &mut ops)); }
        ReadChunk { text, ends, ops, nonfinite }
    }
}

/// Lines on their way from the writer to the time model, in order; `done`
/// once the writer has finished. Readers ([`PARSERS`] of them, and the model
/// itself when they lag) format and parse chunks apart from the machine;
/// the model interprets the read chunks strictly in order.
struct Pipe {
    q: Mutex<PipeState>,
    cv: Condvar,
}

#[derive(Default)]
struct PipeState {
    /// Chunks not taken yet, with their sequence numbers.
    raw: std::collections::VecDeque<(u64, Chunk)>,
    sent: u64,
    done: bool,
    /// Chunks read ahead, by sequence number.
    read: std::collections::BTreeMap<u64, ReadChunk>,
}

/// Roughly the line count of the former 64 KiB text chunks; a flush size, not a limit.
const CHUNK_LINES: usize = (1 << 16) / 32;
/// The line before the machine's end sequence.
const END_MARK: &str = "; MACHINE_END_GCODE_START";
/// Helpers reading chunks ahead of the model.
const PARSERS: usize = 2;

impl Pipe {
    fn new() -> Pipe {
        Pipe { q: Mutex::new(PipeState::default()), cv: Condvar::new() }
    }
    fn send(&self, chunk: Chunk, done: bool) {
        let mut q = self.q.lock().unwrap();
        if !chunk.lines.is_empty() {
            let n = q.sent;
            q.raw.push_back((n, chunk));
            q.sent += 1;
        }
        q.done |= done;
        self.cv.notify_all();
    }
    /// A reader: read chunks ahead until the writer is done and none are left, or until `stop`.
    fn read_ahead(&self, stop: &(dyn Fn() -> bool + Sync)) {
        let mut q = self.q.lock().unwrap();
        loop {
            if stop() {
                return;
            }
            if let Some((n, c)) = q.raw.pop_front() {
                drop(q);
                let c = c.read();
                q = self.q.lock().unwrap();
                q.read.insert(n, c);
                self.cv.notify_all();
                continue;
            }
            if q.done {
                return;
            }
            q = self.cv.wait(q).unwrap();
        }
    }
    /// Chunk `n`, read; `None` once the writer is done and every chunk is
    /// taken. The model reads it itself when it is still on the queue (the
    /// queue's front is always the lowest chunk not taken).
    fn recv(&self, n: u64) -> Option<ReadChunk> {
        let mut q = self.q.lock().unwrap();
        loop {
            if let Some(r) = q.read.remove(&n) {
                return Some(r);
            }
            if q.raw.front().is_some_and(|f| f.0 == n) {
                let (_, c) = q.raw.pop_front().unwrap();
                drop(q);
                let c = c.read();
                return Some(c);
            }
            if q.done && n >= q.sent {
                return None;
            }
            q = self.cv.wait(q).unwrap();
        }
    }
}

/// The time model over every line the writer makes, in order; with the chunks it read, which
/// are the text. Once `stop` is true it ends between chunks with nothing: the writing is
/// thrown away.
fn time_model(job: &Job, labels: &[u32], pipe: &Pipe, stop: &(dyn Fn() -> bool + Sync)) -> (Timeline, Vec<String>, Vec<usize>, usize) {
    let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeTime);
    let mut proc = Processor::new(job);
    proc.set_object_labels(labels);
    let mut chunks = Vec::new();
    let mut ends = Vec::new();
    let mut bytes = 0;
    let mut nonfinite = 0;
    loop {
        if stop() {
            return (Timeline::default(), Vec::new(), Vec::new(), 0);
        }
        let Some(ReadChunk { text: c, ends: local_ends, ops, nonfinite: n }) = pipe.recv(chunks.len() as u64) else { break };
        let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeModelOps);
        makepad_fab_toolpath::par::plate_span("gcode", || {
            for op in &ops {
                proc.op(op, &c);
            }
        });
        ends.extend(local_ends.into_iter().map(|end| bytes + end));
        bytes += c.len();
        nonfinite += n;
        chunks.push(c);
    }
    let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeModelOps);
    (makepad_fab_toolpath::par::plate_span("gcode", || proc.finish()), chunks, ends, nonfinite)
}

enum Half<'a> {
    Written(Box<Writer<'a>>, usize, Vec<usize>, bool, f64),
    Timed(Timeline, Vec<String>, Vec<usize>, usize),
    Read,
}

/// Write the toolpaths as G-code. The writer never reads the time model
/// back while writing, so the model interprets the lines beside it, one
/// chunk behind, on the second index of a parallel map; the next indices
/// read chunks ahead for it. The writer (index 0, claimed first) never
/// waits, the model only waits on a writer or a reader that is running
/// (it reads a chunk itself when no reader has taken it), and on one
/// thread the model simply runs after the writer.
///
/// `stop` is asked between layers, and by the time model and the readers between chunks: once
/// true the writing ends, the chunks it sent are left unread, and the output is a `STOPPED`
/// warning, nothing else. A part that saw it made too little to put together, so the warning
/// stands even if `stop` turns false again.
pub fn write(tp: &Toolpaths, job: &Job, generator: &str, stop: &(dyn Fn() -> bool + Sync)) -> GcodeOutput {
    let Some(family) = family::of(&job.cfg) else {
        return GcodeOutput { text: String::new(), timeline: Timeline::default(), stats: Stats::default(), warnings: vec![family::UNSUPPORTED.into()] };
    };
    let pipe = Pipe::new();
    let labels: Vec<u32> = (0..tp.objects.len()).map(|i| 100 + i as u32 * 11).collect();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let halt = || {
        let s = stop();
        if s {
            stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        s
    };
    let mut halves = makepad_fab_toolpath::par::map(2 + PARSERS, |i| {
        if i == 0 {
            // Ends the model's wait even if writing panics.
            struct Done<'p>(&'p Pipe);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.send(Chunk::default(), true);
                }
            }
            let _done = Done(&pipe);
            let (w, nl, used, multi, max_z) = makepad_fab_toolpath::par::plate_span("gcode", || write_lines(tp, job, family, Sink::Text(&pipe), &halt));
            Half::Written(Box::new(w), nl, used, multi, max_z)
        } else if i == 1 {
            let (timeline, chunks, ends, nonfinite) = time_model(job, &labels, &pipe, &halt);
            Half::Timed(timeline, chunks, ends, nonfinite)
        } else {
            pipe.read_ahead(&halt);
            Half::Read
        }
    });
    halves.truncate(2);
    let (Some(Half::Timed(timeline, chunks, ends, nonfinite)), Some(Half::Written(mut w, nl, used, multi, max_z))) = (halves.pop(), halves.pop()) else { unreachable!() };
    if halt() || stopped.load(std::sync::atomic::Ordering::Relaxed) {
        return GcodeOutput { text: String::new(), timeline: Timeline::default(), stats: Stats::default(), warnings: vec![STOPPED.to_string()] };
    }
    let n = w.nonfinite + nonfinite;
    if n > 0 { w.errors.push(format!("the G-code has {n} non-finite numbers (a NaN or infinite coordinate, feed or extrusion)")); }
    assemble(tp, job, generator, *w, nl, used, multi, max_z, timeline, &chunks, &ends)
}

/// Bytes `from..to` of the text that `chunks` (starting at `starts`) hold back to back.
fn push_text(out: &mut String, chunks: &[String], starts: &[usize], from: usize, to: usize) {
    let mut k = starts.partition_point(|&s| s <= from).saturating_sub(1);
    let mut a = from;
    while a < to {
        let s = starts[k];
        let e = (s + chunks[k].len()).min(to);
        out.push_str(&chunks[k][a - s..e - s]);
        a = e;
        k += 1;
    }
}

/// A second-family sequence line that may leave the head elsewhere: any command but the
/// settings, dwells and mode switches the print safety check knows and lets keep the position.
/// Any other command, a named routine or an unknown M-code, may be a printer macro, whose moves
/// are the printer's own.
fn named_line_moves(line: &str) -> bool {
    let mut words = line.split(';').next().unwrap_or("").split_whitespace();
    let Some(cmd) = words.next().map(str::to_ascii_uppercase) else { return false };
    let keeps = match cmd.as_str() {
        "G4" | "G17" | "G21" | "G90" | "G91" | "M73" | "M82" | "M83" | "M104" | "M106" | "M107" | "M109" | "M140" | "M141" | "M190"
        | "M191" | "M201" | "M203" | "M204" | "M205" | "M220" | "M400" | "M900" | "SET_VELOCITY_LIMIT" | "SET_PRESSURE_ADVANCE"
        | "EXCLUDE_OBJECT_DEFINE" | "EXCLUDE_OBJECT_START" | "EXCLUDE_OBJECT_END" => true,
        "G92" => words.all(|w| w.starts_with(['E', 'e'])),
        _ => false,
    };
    !keeps
}

fn template_moves(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim_start();
        let Some(t) = t.strip_prefix('G').or_else(|| t.strip_prefix('g')) else { return false };
        let n = t.bytes().take_while(u8::is_ascii_digit).count();
        matches!(t[..n].parse::<u32>(), Ok(0..=3 | 28..=30))
    })
}

fn write_lines<'a>(tp: &Toolpaths, job: &'a Job, family: &'static family::Family, sink: Sink<'a>, stop: &(dyn Fn() -> bool + Sync)) -> (Writer<'a>, usize, Vec<usize>, bool, f64) {
    fmt::take_nonfinite();
    let by_object = sequential::active(tp, job);
    let sequence = if by_object { sequential::layer_order(tp) } else { Vec::new() };
    // The pauses for items printed into the objects ([`Toolpaths::pauses`]), by the layer they
    // come before; printing by object, each object's items before its own layer.
    let mut pause_at: std::collections::BTreeMap<usize, Vec<(usize, usize)>> = std::collections::BTreeMap::new();
    for p in tp.pauses() {
        for (o, k) in p.items {
            let li = if by_object { sequence.iter().position(|&(l, ob, _)| l == p.layer && ob == o) } else { Some(p.layer) };
            if let Some(li) = li {
                pause_at.entry(li).or_default().push((o, k));
            }
        }
    }
    let reordered = by_object.then(|| sequential::reordered(tp, &sequence));
    let tp = reordered.as_ref().unwrap_or(tp);
    let object_layer = |li: usize| if by_object { sequence[li].2 } else { li };
    let mut w = Writer::new(job, family, sink);
    w.by_object = by_object;
    if by_object { w.push("; timelapse off: printing by object"); }
    let prof_plan = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodePlan);
    let cfg = &job.cfg;
    let nl = tp.layers.len();
    let mut windows = vec![(0, nl); nl];
    if by_object {
        let mut start = 0;
        for li in 0..nl {
            if li + 1 == nl || object_layer(li + 1) == 0 {
                windows[start..=li].fill((start, li + 1));
                start = li + 1;
            }
        }
    }
    // Used filaments in first-use order.
    let mut used: Vec<usize> = Vec::new();
    for l in &tp.layers {
        for ol in l.objects.iter().filter(|o| o.has_paths()) {
            let f = ol.filament;
            if !used.contains(&f) {
                used.push(f);
            }
        }
        for b in tp.np_blocks.iter().filter(|b| b.layer == l.index && !b.paths.is_empty()) {
            let f = tp.objects[b.object].filament;
            if !used.contains(&f) { used.push(f); }
        }
    }
    if used.is_empty() {
        used.push(0);
    }
    w.filament = used[0];
    let multi = tp.objects.len() > 1 || w.firmware.labels_every_object();
    // Object label ids for skipping.
    w.label_ids = (0..tp.objects.len()).map(|i| 100 + i as u32 * 11).collect();
    w.object_names = tp.objects.iter().enumerate().map(|(i, o)| firmware::object_name(&o.name, i)).collect();
    w.object_accels = tp.objects.iter().map(|o| ObjectAccels::of(job, o)).collect();
    // Plan every layer's filament order, path order and cooling first: the
    // outer-wall speed of a layer depends on its neighbours, and the prime
    // tower on every change.
    let mut plans: Vec<Vec<(usize, Vec<Planned>)>> = Vec::with_capacity(nl);
    let mut caps: Vec<Option<f64>> = Vec::with_capacity(nl);
    let mut est_times: Vec<f64> = Vec::with_capacity(nl);
    let mut layer_work: Vec<(Vec<(f64, f64)>, f64, usize)> = Vec::with_capacity(nl);
    let mut changes_per_layer: Vec<Vec<(usize, usize)>> = Vec::with_capacity(nl);
    // Per layer, per change: where the head leaves the part and where it resumes.
    let mut change_hops: Vec<Vec<(Pt, Pt)>> = Vec::with_capacity(nl);
    let mut cur = Pt::default();
    // What each nozzle holds as the plan goes, for each layer's order and what its changes purge.
    let change_model = w.changes.clone();
    let mut plan = ChangePlan::new(&change_model, used[0]);
    // What may take a change's purge, per object (its own settings over the job's).
    let sinks: Vec<[bool; 3]> = tp.objects.iter().map(|o| {
        let own = (!o.overrides.is_empty()).then(|| job.with_overrides(&o.overrides));
        let j = own.as_ref().unwrap_or(job);
        ["flush_into_infill", "flush_into_objects", "flush_into_support"].map(|k| j.b(k))
    }).collect();
    let sinking = !w.changes.free() && sinks.iter().flatten().any(|&s| s);
    let nearest = (job.s("seam_position") == "nearest").then_some(job);
    let present: Vec<Vec<usize>> = tp.layers.iter().map(layer_filaments).collect();
    for (li, layer) in tp.layers.iter().enumerate() {
        if stop() {
            w.nonfinite = fmt::take_nonfinite();
            return (w, nl, used, multi, 0.0);
        }
        let order = plan.order(&change_model, &present[li], present.get(li + 1).map_or(&[][..], |n| n.as_slice()));
        let mut groups = order_layer(layer, cur, order, nearest);
        w.flush_into.push(if sinking { flush_into(&mut groups, plan.held(), plan.loaded(), job, &change_model, &sinks) } else { Vec::new() });
        let mut changes = Vec::new();
        let mut adjustable = Vec::new();
        let mut fixed = 0.0;
        let tspeed = w.travel_speed() / 60.0;
        let first = object_layer(li) == 0;
        if by_object && first && li > 0 {
            if let Some(p) = groups.iter().find_map(|(_, paths)| paths.first()) { cur = p.first(); }
        }
        let ta = w.travel_accel(first);
        let mut travel_t = 0.0;
        let mut hops = Vec::new();
        for (f, paths) in &groups {
            if *f != plan.loaded() {
                changes.push((plan.loaded(), *f));
                hops.push((cur, paths.first().map_or(cur, |p| p.first())));
                plan.take(&change_model, *f);
            }
            for p in paths {
                let d = cur.dist(p.first()) / SCALE;
                // Travel from rest to rest: accelerating dominates short hops.
                if d > 0.0 {
                    travel_t += crate::time::trapezoid(d, 0.0, tspeed.max(1.0), 0.0, ta.max(1.0)).0;
                }
                let speed = w.path_speed(p.path, *f, first);
                if p.path.role == Role::FloatingVerticalShell {
                    fixed += p.path.length_mm() / speed.max(1e-6);
                } else {
                    adjustable.push((p.path.length_mm(), speed));
                }
                cur = p.last();
            }
        }
        if by_object {
            for b in tp.np_blocks.iter().filter(|b| b.layer == li && !b.paths.is_empty()) {
                let f = tp.objects[b.object].filament;
                if f != plan.loaded() {
                    changes.push((plan.loaded(), f));
                    plan.take(&change_model, f);
                }
                for p in &b.paths {
                    fixed += p.length_mm() / w.path_speed(p, f, first).max(1e-6);
                    if let Some(&last) = p.pts.last() { cur = last; }
                }
            }
        }
        let f = groups.first().map(|g| g.0).unwrap_or(w.filament);
        layer_work.push((adjustable, fixed + travel_t, f));
        changes_per_layer.push(changes);
        change_hops.push(hops);
        plans.push(groups);
    }
    // Every planned path's arc fit, in parallel: the fit depends only on the
    // path and the resolution, so the writer below only formats.
    if w.b_m("enable_arc_fitting") {
        let tol = { let r = w.f_m("resolution"); if r > 0.0 { r } else { 0.012 } };
        let todo: Vec<&Planned> = plans.iter().flat_map(|g| g.iter()).flat_map(|(_, ps)| ps.iter()).filter(|p| p.path.dry.is_empty() && p.path.z.is_empty() && p.path.flow.is_empty() && p.path.role != Role::Brim && p.path.pts.len() >= 3).collect();
        let made: Vec<Vec<crate::arcfit::Seg>> = makepad_fab_toolpath::par::map(todo.len(), |i| {
            let p = todo[i];
            let seated = p.seat.map(|_| p.oriented());
            let path = seated.as_ref().unwrap_or(p.path);
            let mut pts: Vec<[f64; 2]> = path.pts.iter().map(|p| [p.x_mm(), p.y_mm()]).collect();
            if p.reverse { pts.reverse(); }
            crate::arcfit::fit_arcs_within(&pts, tol, crate::arcfit::max_sweep_for(path.role.name()))
        });
        w.fits = todo.iter().map(|p| p.key()).zip(made).collect();
    }
    if w.avoid_on {
        let widths: Vec<_> = (0..job.filaments()).map(|f| {
            let p = makepad_fab_toolpath::Params::from_job(job, f);
            (p.w_inner, p.w_outer)
        }).collect();
        let support = job.b("avoid_crossing_wall_includes_support");
        w.avoid = makepad_fab_toolpath::par::map(nl, |li| {
            if stop() { None } else { Avoid::new(&tp.layers[li], &widths, support) }
        });
    }
    // Each layer's parts, in parallel: a travel inside one, from fill to fill, crosses no outline.
    let mut inners: Vec<Option<makepad_fab_slicer::grid::SegGrid<u32>>> = makepad_fab_toolpath::par::map(nl, |li| {
        if stop() { return None; }
        let parts = makepad_fab_slicer::clip::union_ex(&tp.layers[li].objects.iter().flat_map(|o| o.slices.iter().cloned()).collect::<Vec<_>>());
        (!parts.is_empty()).then(|| makepad_fab_slicer::grid::SegGrid::of_regions(&parts, |k| k as u32))
    });
    // Auto lifts probe overhangs and supports from this and nearby layers.
    if used.iter().any(|&f| w.lift_type(f) == Lift::Auto && w.eff_m("z_hop", f) > 0.0) {
        use makepad_fab_slicer::{clip::difference_ex, grid::SegGrid, offset::{offset_ex, opening, DEFAULT_MITER}};
        let widths: Vec<f64> = (0..job.filaments()).map(|f| makepad_fab_toolpath::Params::from_job(job, f).w_outer).collect();
        let mut last = std::collections::HashMap::new();
        let mut previous: Vec<Vec<Option<usize>>> = Vec::with_capacity(nl);
        for (li, layer) in tp.layers.iter().enumerate() {
            previous.push(layer.objects.iter().map(|ol| last.get(&ol.object).copied()).collect());
            for ol in &layer.objects { last.insert(ol.object, li); }
        }
        w.lift_areas = makepad_fab_toolpath::par::map(nl, |li| {
            let layer = &tp.layers[li];
            if stop() { return (layer.print_z, None); }
            let mut regions = Vec::new();
            let mut support = Vec::new();
            for (oi, ol) in layer.objects.iter().enumerate() {
                if ol.support_only {
                    regions.extend(ol.slices.iter().cloned());
                } else {
                    if let Some(pi) = previous[li][oi] {
                        let below: Vec<ExPolygon> = tp.layers[pi].objects.iter().filter(|o| o.object == ol.object).flat_map(|o| o.slices.iter().cloned()).collect();
                        let width = widths[ol.filament] * SCALE;
                        let grown = offset_ex(&below, 0.3 * width, DEFAULT_MITER);
                        let overhang = difference_ex(&ol.slices, &grown);
                        regions.extend(opening(&overhang, 0.1 * width, 0.1 * width, DEFAULT_MITER));
                    }
                    for path in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)).filter(|p| matches!(p.role, Role::Support | Role::SupportInterface | Role::SupportTransition)) {
                        for p in path.pts.windows(2) { support.push((p[0], p[1])); }
                    }
                }
            }
            let mut grid = SegGrid::of_regions(&regions, |k| k);
            if !support.is_empty() {
                let mut segs = grid.segments().to_vec();
                // Both directions keep open support segments out of winding sums.
                for (a, b) in support {
                    segs.push((a, b, regions.len()));
                    segs.push((b, a, regions.len()));
                }
                grid = SegGrid::new(segs);
            }
            let area = if grid.is_empty() { None } else {
                let mut bbox = BBox::empty();
                for &(a, b, _) in grid.segments() { bbox.add(a); bbox.add(b); }
                Some(LiftArea { grid, bbox })
            };
            (layer.print_z, area)
        });
    }
    // The prime tower.
    let last_change = changes_per_layer.iter().rposition(|c| !c.is_empty());
    if let (Some(lc), true) = (last_change, !by_object && cfg.bool("enable_prime_tower")) {
        w.tower = Some(plan_tower(job, tp, &used, &changes_per_layer, lc));
    }
    w.skirt = plan_skirt(job, tp, w.tower.as_ref(), plans.first().and_then(|g| g.first()).map_or(w.filament, |g| g.0));
    let mut object_skirts: Vec<Option<Skirt>> = (0..nl).map(|li| {
        if !by_object || object_layer(li) != 0 { return None; }
        let f = plans[li].first().map_or(tp.objects[sequence[li].1].filament, |g| g.0);
        plan_object_skirt(job, &tp.layers[li], sequence[li].1, &tp.np_blocks, f)
    }).collect();
    // Cooling: a tower layer without a change prints the tower's empty
    // layer too, which the part cools through.
    for (li, (adjustable, mut fixed, f)) in layer_work.into_iter().enumerate() {
        if let Some(t) = &w.tower {
            if li <= t.last_layer {
                let h = tp.layers[li].height.max(0.05);
                // The tower prints every layer: its grid and wall, and on a change layer
                // each change's ramming and prime rows.
                fixed += tower_layer_time(job, family, t, &changes_per_layer[li], f, li == 0, h, tp.layers[li].print_z);
                // A change goes by the tower: the hop from the part to it and back
                // replaces the direct one the plan counted.
                let centre = Pt::from_mm(t.x + t.w / 2.0, t.y + t.d / 2.0);
                let (tspeed, ta) = ((w.travel_speed() / 60.0).max(1.0), w.travel_accel(li == 0).max(1.0));
                let hop = |a: Pt, b: Pt| crate::time::trapezoid(a.dist(b) / SCALE, 0.0, tspeed, 0.0, ta).0;
                for &(leave, resume) in &change_hops[li] {
                    fixed += hop(leave, centre) + hop(centre, resume) - hop(leave, resume);
                }
            }
        }
        if let Some(skirt) = &w.skirt { fixed += skirt.layer_time(job, li, f, object_layer(li) == 0); }
        if let Some(skirt) = &object_skirts[li] { fixed += skirt.layer_time(job, 0, f, true); }
        // The first layer slows for cooling like any other (R5.3 §4).
        let cap = if job.fil("slow_down_for_layer_cooling", f) > 0.0 { cooling_cap(&adjustable, fixed, job.fil("slow_down_layer_time", f), job.fil("slow_down_min_speed", f)) } else { None };
        est_times.push(fixed + adjustable.iter().map(|(l, v)| l / cap.map_or(*v, |c| v.min(c)).max(1e-6)).sum::<f64>());
        caps.push(cap);
    }
    let total_changes: usize = changes_per_layer.iter().map(|c| c.len()).sum();
    // Outer walls keep a steady speed up the object: from the third layer
    // on, they print no faster than the mean speed of the layers within the
    // transition distance (a layer without slowdown counts at the outer
    // wall's full speed).
    let continuous = job.b("z_direction_outwall_speed_continuous");
    let window = (job.fil("cooling_perimeter_transition_distance", w.filament) / job.f("layer_height").max(0.01)).round() as usize;
    let full_outer: Vec<f64> = plans
        .iter()
        .map(|groups| groups.iter().flat_map(|g| g.1.iter()).filter(|p| p.path.role == Role::OuterWall).map(|p| p.path.speed).fold(0.0, f64::max))
        .collect();
    let outer_caps: Vec<Option<f64>> = (0..nl)
        .map(|i| {
            if !continuous || object_layer(i) < 2 {
                return caps[i];
            }
            let lo = i.saturating_sub(window).max(windows[i].0 + 2);
            let hi = (i + window).min(windows[i].1 - 1);
            let (mut sum, mut cnt) = (0.0, 0.0);
            for j in lo..=hi {
                let v = caps[j].map_or(full_outer[j], |c| c.min(full_outer[j].max(c)));
                if v > 0.0 {
                    sum += v;
                    cnt += 1.0;
                }
            }
            if cnt == 0.0 {
                return caps[i];
            }
            let mean = sum / cnt;
            Some(caps[i].map_or(mean, |c| c.min(mean)))
        })
        .collect();
    // First layer bounds (the bed probe's area): the sliced regions, and every first-layer extrusion with its
    // half width, so supports with no region of their own (tree supports) and any skirt or brim are probed too.
    let mut fb = BBox::empty();
    let mut farea = 0.0;
    for (li, l0) in tp.layers.iter().enumerate().filter(|(li, _)| object_layer(*li) == 0) {
        for ol in &l0.objects {
            for s in ol.slices.iter() {
                fb.merge(&s.bbox());
                farea += s.area_mm2();
            }
            for p in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                if p.pts.is_empty() {
                    continue;
                }
                let mut b = BBox::of(&p.pts);
                let r = scaled(p.width / 2.0);
                b.min = Pt::new(b.min.x - r, b.min.y - r);
                b.max = Pt::new(b.max.x + r, b.max.y + r);
                fb.merge(&b);
            }
        }
        for b in tp.np_blocks.iter().filter(|b| b.layer == li) {
            for p in &b.paths {
                if p.pts.is_empty() { continue; }
                let mut bounds = BBox::of(&p.pts);
                let r = scaled(p.width / 2.0);
                bounds.min = bounds.min.sub(Pt::new(r, r));
                bounds.max = bounds.max.add(Pt::new(r, r));
                fb.merge(&bounds);
            }
        }
    }
    let (mut fmin, mut fmax) = ([fb.min.x_mm(), fb.min.y_mm()], [fb.max.x_mm(), fb.max.y_mm()]);
    if let Some(t) = &w.tower {
        let b = BBox::of(&tower_first_footprint(job, t));
        fmin = [fmin[0].min(b.min.x_mm()), fmin[1].min(b.min.y_mm())];
        fmax = [fmax[0].max(b.max.x_mm()), fmax[1].max(b.max.y_mm())];
    }
    for skirt in w.skirt.iter().chain(object_skirts.iter().filter_map(|s| s.as_ref())) {
        let b = skirt.loops[0].bbox();
        let r = skirt.width / 2.0;
        fmin = [fmin[0].min(b.min.x_mm() - r), fmin[1].min(b.min.y_mm() - r)];
        fmax = [fmax[0].max(b.max.x_mm() + r), fmax[1].max(b.max.y_mm() + r)];
    }
    let mut print_top = tp.layers.iter().map(|l| l.print_z).fold(0.0, f64::max);
    if by_object {
        for layer in &tp.layers {
            for p in layer.objects.iter().flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)) {
                print_top = p.z.iter().fold(print_top, |z, dz| z.max(layer.print_z + dz));
            }
        }
        for b in &tp.np_blocks {
            print_top = b.paths.iter().flat_map(|p| &p.z).fold(print_top.max(b.z), |z, dz| z.max(b.z + dz));
        }
    }
    let facts = PlateFacts {
        total_layers: nl,
        max_print_z: print_top,
        first_layer_min: fmin,
        first_layer_max: fmax,
        first_layer_area: farea,
        used: used.clone(),
        has_wipe_tower: w.tower.is_some(),
        wipe_tower_center: w.tower.as_ref().map(|t| [t.x + t.w / 2.0, t.y + t.d / 2.0]).unwrap_or([0.0, 0.0]),
        total_toolchanges: total_changes,
    };
    let mut start_env = Env::new(cfg, w.template_dialect);
    vars::set_start_vars(&mut start_env, job, family, &facts);
    w.start_vars = Some(start_env.vars);

    if cfg.str("gcode_flavor") != "klipper" {
        // Machine envelope (normal-mode limits of the first extruder).
        let e0 = job.ext_of(used[0]);
        let lim = |k: &str| cfg.f64_at(k, 2 * e0);
        // A limit the machine profile does not set is left to the firmware.
        let words = |pairs: &[(&str, f64)], fixed2: bool| -> String {
            pairs.iter().filter(|(_, v)| *v > 0.0).map(|(a, v)| if fixed2 { format!(" {a}{v:.2}") } else { format!(" {a}{}", fmt::trim(*v, 3)) }).collect()
        };
        for (cmd, pairs, fixed2) in [
            ("M201", vec![("X", lim("machine_max_acceleration_x")), ("Y", lim("machine_max_acceleration_y")), ("Z", lim("machine_max_acceleration_z")), ("E", lim("machine_max_acceleration_e"))], false),
            ("M203", vec![("X", lim("machine_max_speed_x")), ("Y", lim("machine_max_speed_y")), ("Z", lim("machine_max_speed_z")), ("E", lim("machine_max_speed_e"))], false),
            ("M204", vec![("P", lim("machine_max_acceleration_extruding")), ("R", lim("machine_max_acceleration_retracting")), ("T", lim("machine_max_acceleration_extruding"))], false),
            ("M205", vec![("X", lim("machine_max_jerk_x")), ("Y", lim("machine_max_jerk_y")), ("Z", lim("machine_max_jerk_z")), ("E", lim("machine_max_jerk_e"))], true),
        ] {
            let w_ = words(&pairs, fixed2);
            if !w_.is_empty() {
                w.push(&format!("{cmd}{w_}"));
            }
        }
    }
    w.set_fans(0.0, 0);
    w.feature(Role::Custom);
    let start_src = { let s = cfg.str("machine_start_gcode"); if s.is_empty() { fallback::START.to_string() } else { s.to_string() } };
    let out = w.render("machine_start_gcode", &start_src, |_| {});
    w.push_block(&out);
    w.push("; MACHINE_START_GCODE_END");
    let fsg = cfg.raw("filament_start_gcode", used[0]).to_string();
    let f0 = used[0];
    let out = w.render("filament_start_gcode", &fsg, |env| {
        env.set("filament_extruder_id", TVar::int(f0 as i64));
        env.set("current_filament_id", TVar::int(f0 as i64));
        env.set("layer_num", TVar::int(0));
    });
    w.push_block(&out);
    // A first-layer calibration PA takes precedence, but is emitted only
    // after that layer's own commands along with the other band settings.
    let first_z = tp.layers.first().map(|l| l.print_z).unwrap_or(0.0);
    if !w.cal_bands.iter().any(|b| b.z_from <= first_z + 1e-6 && b.pa.is_some()) {
        w.pressure_advance();
    }
    for line in w.firmware.begin(used[0]) {
        w.push(&line);
    }
    // The template left the head somewhere we do not track.
    w.xy_known = false;
    w.z_known = false;

    let layer_tpl = { let s = cfg.str("layer_change_gcode"); if s.is_empty() { fallback::LAYER.to_string() } else { s.to_string() } };
    // The printer's sequence before each layer change, when its profile has one.
    let before_layer_tpl = cfg.str("before_layer_change_gcode").to_string();
    let tl_tpl = cfg.str("time_lapse_gcode").to_string();
    let pause_tpl = cfg.str("machine_pause_gcode").to_string();
    let most_used_phys = job.physical_index(job.ext_of(used[0]));
    let mut max_z: f64 = 0.0;
    let multi_nozzle = {
        let mut e: Vec<usize> = used.iter().map(|&f| job.ext_of(f)).collect();
        e.sort();
        e.dedup();
        e.len() > 1
    };
    w.loaded = vec![None; job.extruders().max(1)];
    w.drawn_back = vec![0.0; job.fil_ext.iter().map(|e| e + 1).max().unwrap_or(0).max(job.extruders())];
    if w.b_m("fab_toolchange_prime_parked_only") { w.parked_prime = vec![0.0; w.drawn_back.len()]; }
    w.loaded[job.ext_of(used[0])] = Some(used[0]);
    w.changes_left = total_changes;
    let all_mask: u64 = (0..tp.objects.len().min(64)).fold(0u64, |m, i| m | (1u64 << i));
    let mut objects_finished = 0usize;
    let mut recovery_open = false;
    drop(prof_plan);
    let prof_layers = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeLayers);
    for (li, layer) in tp.layers.iter().enumerate() {
        if stop() {
            break;
        }
        let j = object_layer(li);
        let first = j == 0;
        let lz = layer.print_z;
        w.layer_index = li;
        w.object_layer = j;
        w.object_layer_start = windows[li].0;
        w.layer_time = est_times[li];
        if by_object && first {
            let object = sequence[li].1;
            w.accel_object = object;
            w.skirt = object_skirts[li].take();
            w.push(&format!("; PRINTING_OBJECT: {objects_finished}"));
            if li > 0 {
                let start = (li..windows[li].1).find_map(|k| {
                    plans[k].iter().find_map(|(f, paths)| paths.first().map(|p| (*f, p.first())))
                        .or_else(|| tp.np_blocks.iter().filter(|b| b.layer == k).flat_map(|b| &b.paths)
                            .find_map(|p| p.pts.first().map(|&p| (tp.objects[object].filament, p))))
                });
                let next = start.map_or(tp.objects[object].filament, |(f, _)| f);
                // The change's speed guard also covers the outgoing head's retraction.
                if next != w.filament { w.begin_change_speed_guard(); }
                w.retract(true);
                if next != w.filament {
                    w.tool_change(next, job, li, lz, max_z, true, multi, all_mask, true, false);
                }
                if let Some((_, start)) = start { w.object_travel(start, true); }
                let bed = used.iter().map(|&f| job.bed_temp(f, true)).fold(0.0, f64::max);
                w.push(&format!("M140 S{}", bed.round()));
                let line = w.firmware.loaded_temperature(w.firmware.tool(job, next), job.fil("nozzle_temperature_initial_layer", next), None);
                w.push(&line);
                let src = cfg.str("printing_by_object_gcode");
                let out = w.render("printing_by_object_gcode", src, |env| {
                    vars::set_layer_vars(env, job, li, lz, max_z, next, most_used_phys, false);
                    env.set("current_object_idx", TVar::int(objects_finished as i64));
                });
                w.push_block(&out);
                if w.sequence_moves(&out) { w.xy_known = false; w.z_known = false; }
            }
            w.ramp_open = false;
            w.fan = -1.0;
            w.fan_aux = -1;
            w.overhang_fan = None;
            w.overhang_fan_on = false;
        } else if by_object { w.skirt = None; }
        if let Some(items) = pause_at.get(&li) {
            let embeds: Vec<&makepad_fab_toolpath::ObjectEmbed> = items.iter().map(|&(o, k)| &tp.objects[o].embeds[k]).collect();
            let names: Vec<&str> = embeds.iter().map(|e| e.name.as_str()).collect();
            let n = embeds.len() as f64;
            let near = embeds.iter().fold((0.0, 0.0), |c, e| (c.0 + 0.5 * (e.min[0] + e.max[0]) / n, c.1 + 0.5 * (e.min[1] + e.max[1]) / n));
            let temp = job.fil(if j == 0 { "nozzle_temperature_initial_layer" } else { "nozzle_temperature" }, w.filament);
            let bed = used.iter().map(|&u| job.bed_temp(u, j == 0)).fold(0.0, f64::max);
            let fil = w.filament;
            let pause = if pause_tpl.trim().is_empty() {
                w.firmware.pause().to_string()
            } else {
                w.render("machine_pause_gcode", &pause_tpl, |env| vars::set_layer_vars(env, job, li, lz, max_z, fil, most_used_phys, false))
            };
            w.pause_for(&names, near, li, lz, &pause, temp, bed);
        }
        max_z = max_z.max(lz);
        w.sequential_z = max_z;
        w.inner = inners[li].take();
        w.push("; CHANGE_LAYER");
        w.push_tag("; Z_HEIGHT: ", lz);
        w.push_tag("; LAYER_HEIGHT: ", layer.height);
        w.height = layer.height;
        let step = lz - if first { 0.0 } else { tp.layers[li - 1].print_z };
        w.tower_height = if (step - layer.height).abs() < 1e-6 { layer.height } else { step };
        let ln = li;
        let fil = w.filament;
        let mut retracted_early = false;
        let named = w.firmware.0 == family::Flavor::Named;
        if !before_layer_tpl.is_empty() {
            let out = w.render("before_layer_change_gcode", &before_layer_tpl, |env| {
                vars::set_layer_vars(env, job, ln, lz, max_z, fil, most_used_phys, false);
            });
            let moves = w.sequence_moves(&out);
            // The second family: the layer change's retraction and wipe need the known position,
            // so a sequence that may move the head runs after them; the travel then rises before
            // it moves, and no wipe retraces a path from a position the printer chose.
            if moves && named && job.eff_bool("retract_when_changing_layer", w.filament) {
                if plans[li].first().is_some_and(|(f, _)| *f != w.filament) {
                    w.begin_change_speed_guard();
                }
                w.lift_if_z_unknown(lz);
                w.retract(first);
                retracted_early = true;
            }
            w.push_block(&out);
            if moves {
                w.xy_known = false;
                w.z_known = false;
                w.ramp_open = false;
                if named { w.wipe = None; }
            }
        }
        // A ramp that carries on from where the last layer ended (a spiral
        // vase's next revolution) keeps its pressure: no retraction.
        // Only on a single-filament print (no tower or change at the layer
        // start), and only when the layer-change sequence does not move the head.
        let out = w.render("layer_change_gcode", &layer_tpl, |env| {
            vars::set_layer_vars(env, job, ln, lz, max_z, fil, most_used_phys, false);
        });
        let template_moves = w.sequence_moves(&out);
        let continues = used.len() == 1 && !template_moves && w.ramp_open && plans[li].first().filter(|(f, _)| *f == w.filament).and_then(|(_, ps)| ps.first()).is_some_and(|p| {
            let q = p.first();
            !p.path.z.is_empty() && w.xy_known && (q.x_mm() - w.x).hypot(q.y_mm() - w.y) < 2.0
        });
        if !continues && !retracted_early && job.eff_bool("retract_when_changing_layer", w.filament) {
            // This retraction belongs to the incoming layer's first tool change, if any.
            if plans[li].first().is_some_and(|(f, _)| *f != w.filament) {
                w.begin_change_speed_guard();
            }
            w.lift_if_z_unknown(lz);
            w.retract(first);
        }
        w.push_block(&out);
        if template_moves {
            w.xy_known = false;
            w.z_known = false;
            w.ramp_open = false;
            if named { w.wipe = None; }
        }
        w.push("");
        let groups = std::mem::take(&mut plans[li]);
        let cap = caps[li];
        let est_time = est_times[li];
        let (part, aux) = w.fan_for_layer(j, est_time);
        if first {
            // The first layer states its fans whatever came before.
            w.fan = -1.0;
            w.fan_aux = -1;
        }
        w.set_fans(part, aux);
        w.overhang_fan = w.overhang_fan_for_layer(j, part);
        if j == 1 {
            for line in w.firmware.recovery(true) {
                w.push(line);
            }
            recovery_open = true;
            // Normal temperature from the second layer, for the loaded filament only: an idle
            // nozzle gets its own when it is pre-heated and changed to (R5.3 §1.3).
            let loaded = w.filament;
            for &uf in used.iter().filter(|&&uf| uf == loaded) {
                if w.cal_temp.is_some() && job.ext_of(uf) == job.ext_of(w.filament) {
                    continue;
                }
                let t0 = job.fil("nozzle_temperature_initial_layer", uf);
                let t1 = job.fil("nozzle_temperature", uf);
                if (t0 - t1).abs() > 0.5 {
                    let line = w.firmware.loaded_temperature(w.firmware.tool(job, w.filament), t1, Some("set nozzle temperature"));
                    w.push(&line);
                }
            }
            let b0 = used.iter().map(|&u| job.bed_temp(u, true)).fold(0.0, f64::max);
            let b1 = used.iter().map(|&u| job.bed_temp(u, false)).fold(0.0, f64::max);
            if !w.b_m("fab_bed_other_layers_always") && (b0 - b1).abs() > 0.5 {
                w.push(&format!("M140 S{}", b1.round()));
            }
        }
        if li == 1 && w.b_m("fab_bed_other_layers_always") {
            w.set_accel(w.travel_accel(first));
            let bed = used.iter().map(|&f| match job.s("curr_bed_type") {
                "Smooth PEI Plate" => job.fil("hot_plate_temp", f),
                "SuperTack Plate" => job.fil("supertack_plate_temp", f),
                _ => job.bed_temp(f, false),
            }).fold(0.0, f64::max);
            w.push(&format!("M140 S{}", bed.round()));
        }
        w.calibration_layer(lz);
        if let Some(t) = &w.tower {
            if li <= t.last_layer {
                w.tl = Some(TowerLayer::new(t));
            }
        }
        w.layer_changes = { let mut f = w.filament; groups.iter().filter_map(|g| { let c = (g.0 != f).then_some((f, g.0)); f = g.0; c }).collect() };
        w.change_idx = 0;
        let n_changes = w.layer_changes.len();
        let mut changes_done = 0;
        // Timelapse: from the parking spot when both nozzles are in use and
        // the layer has a tower (before the tower's first visit); otherwise
        // inline at the layer's farthest point.
        let parked = multi_nozzle && w.tl.is_some();
        let mut far = (0usize, 0usize, -1.0f64);
        if !by_object && !parked {
            for (gi, (_, paths)) in groups.iter().enumerate() {
                for (k, p) in paths.iter().enumerate() {
                    for q in &p.path.pts {
                        let d = q.x_mm().powi(2) + q.y_mm().powi(2);
                        if d > far.2 {
                            far = (gi, k, d);
                        }
                    }
                }
            }
        }
        let mut photo_done = by_object || tl_tpl.is_empty();
        let mut layer_started = false;
        let mut changed_this_layer = false;
        // A tower layer without a change prints the tower first.
        if n_changes == 0 && w.tl.is_some() {
            if parked && !photo_done {
                w.timelapse(&tl_tpl, job, ln, lz, max_z, most_used_phys, false, multi, all_mask, li);
                photo_done = true;
            }
            w.tower_layer(job, li, lz, first, multi, all_mask, true);
            layer_started = true;
            changed_this_layer = true;
        }
        let skirt_layer = if by_object { 0 } else { li };
        if groups.is_empty() { w.skirt_layer(skirt_layer, lz, first, &mut layer_started); }
        for (gi, (f, paths)) in groups.iter().enumerate() {
            if *f != w.filament {
                if parked && !photo_done {
                    w.timelapse(&tl_tpl, job, ln, lz, max_z, most_used_phys, false, multi, all_mask, li);
                    photo_done = true;
                }
                changes_done += 1;
                w.begin_change_speed_guard();
                w.lift_if_z_unknown(lz);
                w.tool_change(*f, job, li, lz, max_z, first, multi, all_mask, !layer_started, changes_done == n_changes);
                layer_started = true;
                changed_this_layer = true;
            }
            if gi == 0 { w.skirt_layer(skirt_layer, lz, first, &mut layer_started); }
            let mut cur_object: Option<usize> = None;
            for (k, p) in paths.iter().enumerate() {
                if cur_object != Some(p.object) {
                    if multi {
                        if let Some(o) = w.open_label.take() {
                            w.push(&format!("; stop printing object, unique label id: {}", w.label_ids[o]));
                            w.end_label(Some(o));
                        }
                    }
                    let id = w.label_ids[p.object];
                    w.push(&format!("; OBJECT_ID: {id}"));
                    cur_object = Some(p.object);
                    w.accel_object = p.object;
                }
                let layer_start = !layer_started && !first;
                // A 3D path starts at its own height (a ramp continues from where the last one ended).
                let start_z = lz + if p.reverse { p.path.z.last() } else { p.path.z.first() }.copied().unwrap_or(0.0);
                if by_object && !w.xy_known { w.object_travel(p.first(), first); }
                w.descend_after_xy = by_object || !p.path.z.is_empty();
                { let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeTravels); w.travel_to(p.first(), start_z, first, layer_start, p.path.role); }
                w.descend_after_xy = false;
                layer_started = true;
                if multi && w.open_label.is_none() {
                    let id = w.label_ids[p.object];
                    w.push(&format!("; start printing object, unique label id: {id}"));
                    let line = w.firmware.label_object(p.object, &w.object_names[p.object]);
                    w.push(&line);
                    w.open_label = Some(p.object);
                }
                let mut speed = w.path_speed(p.path, *f, first);
                // A floating shell keeps its set speed through cooling.
                let c = if p.path.role == Role::FloatingVerticalShell { None } else if p.path.role == Role::OuterWall { outer_caps[li] } else { cap };
                if let Some(c) = c {
                    if speed > c {
                        // A slowed feed is a whole mm/min.
                        speed = ((c * 60.0).round() / 60.0).max(1.0 / 60.0);
                    }
                }
                let accel = w.accel_for(p.path.role, first);
                w.fit_key = Some(p.key());
                let prof_extrude = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeExtrusions);
                if p.reverse || p.seat.is_some() {
                    let path = p.oriented();
                    w.extrude(&path, speed, accel, lz, first, layer.height);
                } else {
                    w.extrude(p.path, speed, accel, lz, first, layer.height);
                }
                drop(prof_extrude);
                if by_object {
                    max_z = p.path.z.iter().fold(max_z, |z, dz| z.max(lz + dz));
                    w.sequential_z = max_z;
                }
                if !photo_done && !parked && gi == far.0 && k == far.1 {
                    w.timelapse(&tl_tpl, job, ln, lz, max_z, most_used_phys, true, false, all_mask, li);
                    photo_done = true;
                }
            }
            if multi {
                if let Some(o) = w.open_label.take() {
                    w.push(&format!("; stop printing object, unique label id: {}", w.label_ids[o]));
                    w.end_label(Some(o));
                }
            }
        }
        // The non-planar blocks scheduled after this layer.
        for b in tp.np_blocks.iter().filter(|b| b.layer == li) {
            if by_object && !b.paths.is_empty() {
                let f = tp.objects[b.object].filament;
                if f != w.filament { w.tool_change(f, job, li, lz, max_z, first, multi, all_mask, !layer_started, true); }
                if !w.xy_known {
                    if let Some(p) = b.paths.iter().find_map(|p| p.pts.first()) { w.object_travel(*p, first); }
                }
                w.accel_object = b.object;
            }
            if multi {
                let id = w.label_ids[b.object];
                w.push(&format!("; start printing object, unique label id: {id}"));
                let line = w.firmware.label_object(b.object, &w.object_names[b.object]);
                w.push(&line);
            }
            w.emit_np_block(b, first);
            if by_object {
                max_z = b.paths.iter().flat_map(|p| &p.z).fold(max_z.max(b.z), |z, dz| z.max(b.z + dz));
                w.sequential_z = max_z;
            }
            if multi {
                w.push(&format!("; stop printing object, unique label id: {}", w.label_ids[b.object]));
                w.end_label(Some(b.object));
            }
        }
        // A tower layer without a change still gets printed.
        if !changed_this_layer && w.tl.is_some() {
            w.tower_layer(job, li, lz, first, multi, all_mask, !layer_started);
        }
        if !photo_done {
            w.timelapse(&tl_tpl, job, ln, lz, max_z, most_used_phys, !parked, multi, all_mask, li);
        }
        w.tl = None;
        if by_object && li + 1 == windows[li].1 {
            if recovery_open {
                for line in w.firmware.recovery(false) {
                    w.push(line);
                }
                recovery_open = false;
            }
            objects_finished += 1;
        }
    }
    drop(prof_layers);
    // End of print.
    if !by_object || recovery_open {
        for line in w.firmware.recovery(false) {
            w.push(line);
        }
    }
    w.lift_if_z_unknown(max_z);
    let first = by_object && nl > 0 && object_layer(nl - 1) == 0;
    w.retract(first);
    let ta = w.travel_accel(first);
    w.set_accel(ta);
    let hop = w.eff_m("z_hop", w.filament);
    if hop > 0.0 && w.lift_allowed(w.z) {
        w.lift_eager(hop, w.lift_feed(), w.lift_type(w.filament) == Lift::Normal);
    }
    w.fan = -1.0;
    w.fan_aux = -1;
    w.set_fans(0.0, 0);
    if let Some(line) = w.firmware.finish() { w.push(line); }
    w.feature(Role::Custom);
    w.push(END_MARK);
    for uf in [w.filament] {
        let src = cfg.raw("filament_end_gcode", uf).to_string();
        let out = w.render("filament_end_gcode", &src, |env| {
            env.set("current_filament_id", TVar::int(uf as i64));
            env.set("filament_extruder_id", TVar::int(uf as i64));
            env.set("layer_num", TVar::int(nl as i64));
            env.set("layer_z", TVar::f64(max_z));
            env.set("max_layer_z", TVar::f64(max_z));
        });
        w.push_block(&out);
    }
    let end_src = { let s = cfg.str("machine_end_gcode"); if s.is_empty() { fallback::END.to_string() } else { s.to_string() } };
    let zz = w.z;
    let out = w.render("machine_end_gcode", &end_src, |env| {
        env.set("layer_num", TVar::int(nl as i64));
        env.set("layer_z", TVar::f64(zz));
        env.set("max_layer_z", TVar::f64(max_z));
    });
    w.push_end_block(&out);
    w.push("");
    let last = std::mem::take(&mut w.chunk);
    if let Sink::Text(pipe) = &w.sink {
        pipe.send(last, true);
    }
    w.nonfinite = fmt::take_nonfinite();
    (w, nl, used, multi, max_z)
}

#[allow(clippy::too_many_arguments)]
/// The print's totals from its time model: `nl` layers up to `max_z`, filaments `used`.
fn stats_of(job: &Job, timeline: &Timeline, nl: usize, used: &[usize], label_ids: Vec<u32>, max_z: f64) -> Stats {
    let total = timeline.total;
    let mut stats = Stats {
        total_time: total,
        prepare_time: timeline.prepare,
        model_time: total - timeline.prepare,
        layers: nl,
        max_z,
        used: used.to_vec(),
        label_ids,
        ..Default::default()
    };
    let nf = job.filaments().max(1);
    for f in 0..nf {
        let mm = timeline.filament_mm.get(f).copied().unwrap_or(0.0);
        let mm3 = mm * job.filament_area(f);
        stats.filament_mm.push(mm);
        stats.filament_mm3.push(mm3);
        stats.filament_g.push(mm3 * job.fil("filament_density", f) / 1000.0);
    }
    stats.first_layer_time = timeline.layer_times.iter().find(|l| l.0 == 1).map(|l| l.2).unwrap_or(0.0);
    stats
}

/// [`write`]'s timeline and stats without the G-code: the writer's moves go to the time model
/// as numbers in line, never formatted or read back, and no text is kept. The print time,
/// filament and per-layer times of the toolpaths, on the calling thread (its arc fits fan
/// out as [`write`]'s do); `None` once `stop` is true (asked between layers), or an error for
/// an unclaimed printer model. With `stream`, the records go there part by part as the time model
/// makes them final ([`Processor::stream_to`]), and the timeline holds none.
pub fn estimate(tp: &Toolpaths, job: &Job, stop: &(dyn Fn() -> bool + Sync), stream: Option<crate::process::PartStream>) -> Result<Option<(Timeline, Stats)>, &'static str> {
    let family = family::of(&job.cfg).ok_or(family::UNSUPPORTED)?;
    let labels: Vec<u32> = (0..tp.objects.len()).map(|i| 100 + i as u32 * 11).collect();
    let mut model = Processor::new(job);
    model.set_object_labels(&labels);
    if let Some(stream) = stream {
        model.stream_to(stream);
    }
    let (mut w, nl, used, multi, max_z) = write_lines(tp, job, family, Sink::Model(Box::new(model)), stop);
    if stop() {
        return Ok(None);
    }
    let label_ids = if multi { std::mem::take(&mut w.label_ids) } else { Vec::new() };
    let Sink::Model(model) = w.sink else { unreachable!("estimate writes to the model") };
    let timeline = model.finish();
    let stats = stats_of(job, &timeline, nl, &used, label_ids, max_z);
    Ok(Some((timeline, stats)))
}

/// Use the convex hull of each object's first layer of extrusion (including non-planar
/// paths at that layer); if none is available, use its XY bounding-box corners. The centre
/// is the middle of the object's XY bounding box.
fn named_objects(tp: &Toolpaths) -> Vec<firmware::Object> {
    tp.objects.iter().enumerate().map(|(object, o)| {
        // The object's footprint on the bed: the hull of its slices on every layer (its own
        // regions, not a support's), else of its first layer's paths.
        let mut points: Vec<_> = tp.layers.iter().flat_map(|l| &l.objects).filter(|ol| ol.object == object && !ol.support_only).flat_map(|ol| ol.slices.iter()).flat_map(|s| s.contour.pts.iter().copied()).collect();
        for (li, layer) in tp.layers.iter().enumerate() {
            if !points.is_empty() { break; }
            for ol in layer.objects.iter().filter(|ol| ol.object == object) {
                for path in ol.islands.iter().flat_map(|i| i.walls.iter().chain(&i.fills)) {
                    points.extend_from_slice(&path.pts);
                }
            }
            for b in tp.np_blocks.iter().filter(|b| b.object == object && b.layer == li) {
                for path in &b.paths { points.extend_from_slice(&path.pts); }
            }
        }
        let polygon = if points.is_empty() {
            vec![[o.min[0], o.min[1]], [o.max[0], o.min[1]], [o.max[0], o.max[1]], [o.min[0], o.max[1]]]
        } else {
            convex_hull(&mut points).iter().map(|p| [p.x_mm(), p.y_mm()]).collect()
        };
        firmware::Object {
            name: firmware::object_name(&o.name, object),
            center: [(o.min[0] + o.max[0]) / 2.0, (o.min[1] + o.max[1]) / 2.0],
            polygon,
        }
    }).collect()
}

fn assemble(tp: &Toolpaths, job: &Job, generator: &str, mut w: Writer, nl: usize, used: Vec<usize>, multi: bool, max_z: f64, timeline: Timeline, chunks: &[String], ends: &[usize]) -> GcodeOutput {
    let _prof = makepad_fab_toolpath::prof::scope(makepad_fab_toolpath::prof::Stage::GcodeAssemble);
    let cfg = &job.cfg;
    // Times, then assembly with the M73 progress lines.
    let errors = std::mem::take(&mut w.errors);
    // The text is the chunks back to back, every line in one chunk with its newline.
    let starts: Vec<usize> = chunks.iter().scan(0, |a, c| {
        let s = *a;
        *a += c.len();
        Some(s)
    }).collect();
    let bytes = ends.last().copied().unwrap_or(0);
    let lines: Vec<&str> = {
        let mut v = Vec::with_capacity(ends.len());
        let (mut k, mut a) = (0, 0);
        for &e in ends {
            while e > starts[k] + chunks[k].len() {
                k += 1;
            }
            v.push(&chunks[k][a - starts[k]..e - 1 - starts[k]]);
            a = e;
        }
        v
    };
    // The estimate runs to the end of the file: the end sequence's moves and
    // waits count, except blocks its template defaults off (`M622.1 S0`).
    let end_line = w.end_line.unwrap_or(lines.len());
    let total = timeline.total;
    let stats = stats_of(job, &timeline, nl, &used, if multi { w.label_ids.clone() } else { Vec::new() }, max_z);

    // Header and config block.
    let used_sorted = {
        let mut u = used.clone();
        u.sort();
        u
    };
    let mut text = String::with_capacity(bytes + bytes / 16 + 64 * 1024);
    // Preset text, on its comment line whatever it holds.
    let header_filaments = if w.firmware.header_all_filaments() { (0..job.filaments()).collect() } else { used_sorted.clone() };
    let raw_list = |key: &str| header_filaments.iter().map(|&f| makepad_fab_settings::config::one_line(cfg.raw(key, f)).into_owned()).collect::<Vec<_>>().join(",");
    let head = firmware::Head {
        generator,
        stats: &stats,
        layers: nl,
        used: &used_sorted,
        labels: multi.then(|| (&w.label_ids[..], tp.objects.iter().map(|o| o.max[2] - o.min[2]).collect())),
        density: raw_list("filament_density"),
        diameter: raw_list("filament_diameter"),
        max_z,
        filaments: job.filaments(),
        changes: w.toolchanges,
        objects: if w.firmware.defines_objects() { named_objects(tp) } else { Vec::new() },
    };
    let config = |text: &mut String| {
        if let Some(t) = &w.tower {
            // The block states where the tower really went.
            let mut c = cfg.clone();
            c.set_vec("wipe_tower_x", vec![format!("{}", (t.x * 1000.0).round() / 1000.0)]);
            c.set_vec("wipe_tower_y", vec![format!("{}", (t.y * 1000.0).round() / 1000.0)]);
            c.config_block_lines(text);
        } else {
            cfg.config_block_lines(text);
        }
    };
    w.firmware.head(&mut text, &head, config);
    let base = text.matches('\n').count() as u32;
    // The executable block straight into the text, after the header.
    let body = &mut text;
    let r0 = (total / 60.0).floor() as i64;
    let firmware = w.firmware;
    body.push_str(&firmware.progress(0, r0));
    body.push('\n');
    let (mut lp, mut lr) = (0i64, r0);
    // Final line number (0-based, within the executable block) of each line.
    let mut line_no: Vec<u32> = Vec::with_capacity(lines.len());
    let mut n_out = 1u32;
    // The idle nozzle's cooling and pre-heating, at their times.
    let inserts = crate::preheat::inserts(job, &firmware, &lines, &timeline.line_end, end_line, used.first().copied().unwrap_or(0), &w.blocks, w.tower.is_some());
    let mut next_insert = 0;
    // The lines go over in runs, one copy each, broken only after a line that gets lines inserted.
    let mut copied = 0;
    for (i, l) in lines.iter().enumerate() {
        line_no.push(n_out);
        n_out += 1;
        let insert = next_insert < inserts.len() && inserts[next_insert].0 == i;
        let mut progress = None;
        if i < end_line {
            let t = timeline.line_end.get(i).copied().unwrap_or(0.0);
            let p = if total > 0.0 { ((t / total) * 100.0).floor() as i64 } else { 100 }.min(100);
            let r = ((total - t).max(0.0) / 60.0).floor() as i64;
            let is_move = || l.starts_with("G1 ") || l.starts_with("G2 ") || l.starts_with("G3 ") || l.starts_with("G0 ");
            if (p != lp || r != lr) && p < 100 && is_move() {
                progress = Some((p, r));
            }
        }
        if !insert && progress.is_none() {
            continue;
        }
        push_text(body, chunks, &starts, copied, ends[i]);
        copied = ends[i];
        while next_insert < inserts.len() && inserts[next_insert].0 == i {
            for extra in &inserts[next_insert].1 {
                body.push_str(extra);
                body.push('\n');
                n_out += 1;
            }
            next_insert += 1;
        }
        if let Some((p, r)) = progress {
            body.push_str(&firmware.progress(p, r));
            body.push('\n');
            n_out += 1;
            lp = p;
            lr = r;
        }
    }
    push_text(body, chunks, &starts, copied, bytes);
    body.push_str(&firmware.progress(100, 0));
    body.push('\n');

    let mut timeline = timeline;
    for r in timeline.moves_mut() {
        r.gcode_line = base + line_no.get(r.gcode_line as usize).copied().unwrap_or(n_out);
    }
    firmware.tail(&mut text, &head, config);
    let _ = flow::spacing;
    GcodeOutput { text, timeline, stats, warnings: errors }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_fab_settings::config::Config;
    use makepad_fab_toolpath::{Island, ObjectEmbed, ObjectInfo, ObjectLayer};

    fn change_job(heads: usize, order: &[usize], tower: bool) -> (Config, Toolpaths) {
        let mut cfg = Config::new();
        for (k, v) in [
            ("machine_start_gcode", "G90\nM83\nT0\n"), ("machine_end_gcode", "; done\n"),
            ("change_filament_gcode", "; change\nT[next_extruder]\n"), ("layer_change_gcode", "; after layer\n"),
            ("filament_diameter", "1.75"), ("initial_layer_flow_ratio", "1"), ("layer_height", "0.2"),
            ("travel_speed", "100"), ("travel_speed_z", "10"), ("retraction_length", "1"),
            ("retract_length_toolchange", "10"), ("retract_restart_extra_toolchange", "0.5"),
            ("retraction_speed", "30"), ("deretraction_speed", "20"), ("retraction_minimum_travel", "1"),
            ("default_acceleration", "6000"), ("outer_wall_acceleration", "5000"), ("inner_wall_acceleration", "4000"),
            ("initial_layer_acceleration", "1000"), ("travel_acceleration", "7000"), ("bridge_acceleration", "50%"),
            ("initial_layer_speed", "20"), ("filament_prime_volume", "2"), ("prime_tower_width", "20"),
            ("printable_area", "0x0,200x0,200x200,0x200"), ("curr_bed_type", "Textured PEI Plate"),
            ("nozzle_temperature_initial_layer", "210"), ("nozzle_temperature", "205"),
        ] { cfg.set_str(k, v); }
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); heads]);
        cfg.set_vec("filament_map", (1..=heads).map(|e| e.to_string()).collect());
        cfg.set_str("enable_prime_tower", if tower { "1" } else { "0" });
        let wall = |role| ExtrusionPath {
            role, pts: vec![Pt::from_mm(50.0, 50.0), Pt::from_mm(60.0, 50.0)],
            width: 0.4, height: 0.2, area: 0.08, speed: 10.0, ..Default::default()
        };
        let tp = Toolpaths {
            objects: vec![ObjectInfo { name: "part".into(), filament: 0, min: [50.0, 50.0, 0.0], max: [60.0, 50.0, order.len() as f64 * 0.2], overrides: Vec::new(), embeds: Vec::new() }],
            layers: order.iter().enumerate().map(|(li, &filament)| Layer {
                index: li, print_z: (li + 1) as f64 * 0.2, height: 0.2,
                objects: vec![ObjectLayer { filament, islands: vec![std::sync::Arc::new(Island { walls: vec![wall(Role::OuterWall)], fills: Vec::new() })], ..Default::default() }],
                ..Default::default()
            }).collect(),
            ..Default::default()
        };
        (cfg, tp)
    }

    #[test]
    fn toolchange_without_tree_keys_keeps_its_text() {
        for tower in [false, true] {
            let (cfg, tp) = change_job(2, &[0, 1, 0, 1], tower);
            let out = write(&tp, &Job::new(cfg), "test", &|| false);
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            // Complete output captured before these changes, with no hop to isolate the gates
            // from the universal layer-lift fix. Includes every command, comment and newline.
            let expected = if tower { (7557, 0x49c9fe32e6e1abdau64) } else { (3037, 0xca3fa542f5767573u64) };
            let hash = out.text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
            assert_eq!((out.text.len(), hash), expected, "the absent-key output stays byte-identical, tower={tower}");
        }
    }

    #[test]
    fn layer_start_lifts_clear_the_new_layer_and_keep_their_shape() {
        // With the other slicer's trees a layer start lifts to the new layer's Z + z_hop (0.8);
        // the default family lifts above the layer just printed (0.6), as its reference does.
        for tree in [false, true] {
            for shape in ["Normal Lift", "Spiral Lift", "Slope Lift"] {
                for moving_template in [false, true] {
                    let (mut cfg, tp) = change_job(1, &[0, 0], false);
                    cfg.set_str("z_hop", "0.4");
                    cfg.set_str("z_hop_types", shape);
                    if tree { cfg.set_str("fab_layer_start_lift_above_new_layer", "1"); }
                    if moving_template { cfg.set_str("layer_change_gcode", "G1 X1 Y1\n"); }
                    let out = write(&tp, &Job::new(cfg), "test", &|| false);
                    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
                    let layer = out.text.split("; CHANGE_LAYER\n").nth(2).unwrap();
                    let travel = layer.split("; OBJECT_ID: 100\n").nth(1).unwrap().split("G1 F600\n").next().unwrap();
                    let moves: Vec<_> = travel.lines().filter(|l| l.starts_with("G1") || l.starts_with("G3")).collect();
                    let down = moves.iter().position(|l| *l == "G1 Z.4").expect("restore the new layer's Z");
                    let target = if tree || moving_template { 0.8 } else { 0.6 };
                    let z = |l: &str| l.split_whitespace().find_map(|w| w.strip_prefix('Z').and_then(|z| z.parse::<f64>().ok()));
                    let high = moves[..down].iter().position(|l| z(l).is_some_and(|z| z >= target - 1e-9)).expect("lift to the target");
                    assert!(moves[..down].iter().all(|l| z(l).is_none_or(|z| z <= target + 1e-9 || moving_template)), "no higher than the target: {moves:?}");
                    let word = |l: &str, k: char| l.split_whitespace().find_map(|w| w.strip_prefix(k).and_then(|v| v.parse::<f64>().ok())).unwrap();
                    if moving_template {
                        assert!(moves[high].starts_with("G1 Z"), "unknown position rises vertically: {moves:?}");
                        assert!(!moves.iter().any(|l| l.starts_with("G3")), "never arc from unknown XY");
                        assert!(moves[high + 1..down].iter().any(|l| l.starts_with("G1 X")), "XY follows the lift");
                    } else if shape == "Spiral Lift" {
                        assert!(moves[high].starts_with("G3 Z"), "spiral ends at the target: {moves:?}");
                        // The radius follows z_hop alone: 0.4 / (2 pi atan(3 deg)).
                        let r = word(moves[high], 'I').hypot(word(moves[high], 'J'));
                        assert!((r - 0.4 / (std::f64::consts::TAU * 3.0f64.to_radians().atan())).abs() < 2e-3, "spiral radius {r}");
                    } else if shape == "Normal Lift" {
                        assert!(moves[high].starts_with("G1 Z"), "straight lift: {moves:?}");
                        assert!(moves[high + 1..down].iter().any(|l| l.starts_with("G1 X")), "XY follows the lift");
                    } else {
                        assert!(moves[high].starts_with("G1 X"), "sloped lift: {moves:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn toolchanges_guard_the_sequence_and_restore_only_parked_heads() {
        for tower in [false, true] {
            for overrides in [false, true] {
                let (mut cfg, tp) = change_job(2, &[0, 1, 0, 1], tower);
                cfg.set_str("fab_toolchange_speed_guard", "1");
                cfg.set_str("fab_toolchange_prime_parked_only", "1");
                cfg.set_str("change_filament_gcode", "; change\n; held {new_extruder_retracted_length}\nT[next_extruder]\n");
                if overrides {
                    cfg.set_vec("filament_retract_length_toolchange", vec!["7".into(), "8".into()]);
                    cfg.set_str("deretraction_speed", "0");
                    cfg.set_str("retract_when_changing_layer", "1");
                }
                let out = write(&tp, &Job::new(cfg), "test", &|| false);
                assert!(out.warnings.is_empty(), "{:?}", out.warnings);
                let body = out.text.split("; EXECUTABLE_BLOCK_START\n").nth(1).unwrap();
                assert_eq!(body.matches("M220 B\nM220 S100\n").count(), 3, "one guard per change");
                assert_eq!(body.matches("M220 R\n").count(), 3, "one restore per change");
                let feed = if overrides { 1800 } else { 1200 };
                for (i, layer) in body.split("; CHANGE_LAYER\n").skip(2).enumerate() {
                    let guard = layer.find("M220 B\nM220 S100\n").unwrap();
                    let change = layer.find("; change\n").unwrap();
                    assert!(guard < change, "guard precedes the change template");
                    assert!(!layer[..guard].contains("G1 E-"), "guard precedes every outgoing retraction");
                    let before = &layer[guard..change];
                    assert!(before.contains("G1 E-"), "outgoing park retraction is guarded");
                    assert!(!layer[..guard].contains("; WIPE_TOWER_START"), "guard precedes the move off the object");
                    let after = &layer[change..];
                    let prime_end = if tower { after.find("G4 S0\n").unwrap() } else { after.find("M220 R\n").unwrap() };
                    let prime = &after[..prime_end];
                    let pushes: Vec<_> = prime.lines().filter(|l| l.starts_with("G1 E") && !l.starts_with("G1 E-")).collect();
                    if i == 0 {
                        assert!(pushes.is_empty(), "a head's first use has no push-back: {prime}");
                        assert!(prime.contains("; held 0\n"), "the start sequence does not count as a park");
                    } else {
                        let amount = if overrides { if i == 1 { "7.5" } else { "8.5" } } else { "10.5" };
                        assert_eq!(pushes, [format!("G1 E{amount} F{feed}")], "restore that head's park, with restart extra");
                        let held = if overrides { if i == 1 { "7" } else { "8" } } else { "10" };
                        assert!(prime.contains(&format!("; held {held}\n")), "template retraction length excludes restart extra");
                    }
                    let restore = after.find("M220 R\n").unwrap();
                    if tower {
                        let purge = after.find("; CP_TOOLCHANGE_WIPE").unwrap();
                        assert!(purge < restore, "restore follows the purge");
                        let last = after[..restore].lines().rev().find(|l| !l.starts_with("M73 ")).unwrap();
                        assert!(last.starts_with("G1 X") && last.contains(" E"), "restore immediately follows the last purge row: {last}");
                        assert!(after[restore..].starts_with("M220 R\n; WIPE_TOWER_END\n"));
                    } else {
                        assert!(after[..restore].contains(&format!("G1 Z{}\n", fmt::xyz((i + 2) as f64 * 0.2))), "Z restores before speed");
                        assert!(!after[..restore].lines().any(|l| l.starts_with("G1 X") && l.contains(" E")), "speed restores before the first extrusion");
                    }
                }
            }
        }
    }

    #[test]
    fn overhang_wall_acceleration_is_gated_and_keeps_first_layer_and_other_walls() {
        for enabled in [false, true] {
            for object_override in [false, true] {
                let (mut cfg, mut tp) = change_job(1, &[0, 0], false);
                if enabled { cfg.set_str("fab_overhang_wall_bridge_acceleration", "1"); }
                cfg.set_str("enable_overhang_bridge_fan", "1");
                cfg.set_str("overhang_fan_threshold", "0%");
                cfg.set_str("overhang_fan_speed", "100");
                if object_override { tp.objects[0].overrides.push(("outer_wall_acceleration".into(), "5000".into())); }
                for layer in &mut tp.layers {
                    let walls = &mut std::sync::Arc::make_mut(&mut layer.objects[0].islands[0]).walls;
                    let base = walls[0].clone();
                    for (i, role) in [Role::OverhangWall, Role::InnerWall, Role::Bridge, Role::InternalBridge].into_iter().enumerate() {
                        let mut path = base.clone();
                        path.role = role;
                        path.overhang = 5.0;
                        path.pts = vec![Pt::from_mm(50.0, 51.0 + i as f64), Pt::from_mm(60.0, 51.0 + i as f64)];
                        walls.push(path);
                    }
                }
                let out = write(&tp, &Job::new(cfg), "test", &|| false);
                assert!(out.warnings.is_empty(), "{:?}", out.warnings);
                for layer in 0..2 {
                    for role in [Role::OuterWall, Role::OverhangWall, Role::InnerWall, Role::Bridge, Role::InternalBridge] {
                        let moves: Vec<_> = out.timeline.moves().filter(|m| m.layer == layer && m.role == role && m.kind == crate::record::MoveKind::Extrude).collect();
                        assert!(!moves.is_empty(), "{role:?} prints on layer {layer}");
                        let expected = if layer == 0 { 1000.0 } else { match role {
                            Role::Bridge | Role::InternalBridge => 2500.0,
                            Role::OverhangWall if enabled => 2500.0,
                            Role::InnerWall => 4000.0,
                            _ => 5000.0,
                        } };
                        assert!(moves.iter().all(|m| m.accel == expected), "{role:?}, layer {layer}, enabled {enabled}, object override {object_override}: {expected}");
                    }
                }
            }
        }
    }

    #[test]
    fn by_object_changes_guard_the_outgoing_retraction() {
        let (mut cfg, mut tp) = change_job(2, &[0, 0], false);
        cfg.set_str("print_sequence", "by object");
        cfg.set_str("fab_toolchange_speed_guard", "1");
        let mut other = tp.objects[0].clone();
        other.name = "other".into();
        other.filament = 1;
        other.min = [130.0, 130.0, 0.0];
        other.max = [140.0, 130.0, 0.4];
        tp.objects.push(other);
        for layer in &mut tp.layers {
            let mut ol = layer.objects[0].clone();
            ol.object = 1;
            ol.filament = 1;
            for wall in &mut std::sync::Arc::make_mut(&mut ol.islands[0]).walls { wall.pts = vec![Pt::from_mm(130.0, 130.0), Pt::from_mm(140.0, 130.0)]; }
            layer.objects.push(ol);
        }
        let out = write(&tp, &Job::new(cfg), "test", &|| false);
        let body = out.text.split("; EXECUTABLE_BLOCK_START\n").nth(1).unwrap();
        let guard = body.find("M220 B\nM220 S100\n").expect("the change between the objects is guarded");
        let before = &body[..guard];
        let lines: Vec<_> = before.lines().collect();
        let last = lines.iter().rposition(|l| l.starts_with("G1 X") && l.contains(" E") && !l.contains(" E-")).expect("the first object's last extrusion");
        let tail = &lines[last + 1..];
        assert!(!tail.iter().any(|l| l.contains(" E-") || l.starts_with("G1 E-")), "the guard precedes the outgoing retraction: {tail:?}");
        assert!(body[guard..].contains("M220 R\n"), "and is restored");
    }

    #[test]
    fn second_family_routines_in_sequences_leave_the_position_unknown() {
        for (line, moves) in [
            ("", false), ("; note", false), ("G92 E0", false), ("g4 p100", false), ("M106 S0 ; fan", false), ("G90", false),
            ("PHOTO_FRAME", true), ("set_layer_info CURRENT=2", true), ("G28", true), ("G1 X1", true), ("G92 X0", true), ("M620 S1", true), ("T1", true),
            ("M600", true), ("M220 B", false), ("M104 S0 T1", false), ("m83", false),
            ("SET_PRESSURE_ADVANCE ADVANCE=0.02", false), ("set_velocity_limit accel=1000", false), ("EXCLUDE_OBJECT_END NAME=a", false),
        ] {
            assert_eq!(named_line_moves(line), moves, "{line:?}");
        }
        // The layer change's retraction and wipe come before a routine that may move the head,
        // and the travel then rises before it moves. The default family keeps its order.
        for named in [false, true] {
            let (mut cfg, tp) = change_job(1, &[0, 0], false);
            cfg.set_str("before_layer_change_gcode", "; before\nG92 E0\nPHOTO_FRAME\n");
            cfg.set_str("retract_when_changing_layer", "1");
            cfg.set_str("wipe", "1");
            cfg.set_str("wipe_distance", "2");
            cfg.set_str("z_hop", "0.4");
            cfg.set_str("z_hop_types", "Normal Lift");
            if named {
                cfg.set_str("printer_model", family::FAMILIES[1].models[0]);
                cfg.set_str("gcode_flavor", "klipper");
            }
            let out = write(&tp, &Job::new(cfg), "test", &|| false);
            let layer = out.text.split("; CHANGE_LAYER\n").nth(2).unwrap();
            let first = layer.lines().position(|l| l.starts_with("G1 X") && l.contains(" E") && !l.contains(" E-")).unwrap();
            let lines: Vec<_> = layer.lines().collect();
            let photo_line = lines.iter().position(|l| *l == "PHOTO_FRAME").unwrap();
            let retract = lines.iter().position(|l| l.starts_with("G1 E-")).expect("the layer change retracts");
            let wipe = lines.iter().position(|l| l.starts_with("G1 X") && l.contains(" E-")).expect("and wipes");
            if named {
                assert!(retract < photo_line && wipe < photo_line, "retract and wipe first: {layer}");
                let after = &lines[photo_line + 1..first];
                let rise = after.iter().position(|l| l.starts_with("G1 Z")).expect("the travel rises");
                let xy = after.iter().position(|l| l.starts_with("G1 X")).expect("then moves");
                assert!(rise < xy, "Z before XY after the routine: {after:?}");
                assert!(!after.iter().any(|l| l.contains(" E-")), "no wipe from an unknown position: {after:?}");
                assert!(!after.iter().any(|l| l.starts_with("G3") || l.starts_with("G2")), "no arc from an unknown position");
            } else {
                assert!(retract > photo_line && wipe > photo_line, "the default family's order: {layer}");
            }
        }
        // A sequence that leaves relative moves is followed by the writer's absolute mode, and the
        // default family's output gains nothing.
        for named in [false, true] {
            let (mut cfg, tp) = change_job(1, &[0, 0], false);
            cfg.set_str("before_layer_change_gcode", "PHOTO_FRAME\nG91\nM82\n");
            for (k, v) in [("retract_when_changing_layer", "1"), ("z_hop", "0.4"), ("z_hop_types", "Normal Lift")] { cfg.set_str(k, v); }
            if named {
                cfg.set_str("printer_model", family::FAMILIES[1].models[0]);
                cfg.set_str("gcode_flavor", "klipper");
            }
            let out = write(&tp, &Job::new(cfg), "test", &|| false);
            let layer = out.text.split("; CHANGE_LAYER\n").nth(2).unwrap();
            let after = layer.split("M82\n").nth(1).unwrap();
            if named {
                assert!(after.starts_with("G90\nM83\n"), "modes restored at once: {after}");
            } else {
                assert!(!after.starts_with("G90"), "the default family adds nothing: {after}");
            }
        }
        // After a routine in the after-layer sequence or a photo, nothing retraces the last path
        // or arcs before the head is placed again, a tool change included.
        for key in ["layer_change_gcode", "time_lapse_gcode"] {
            let (mut cfg, tp) = change_job(2, &[0, 1], false);
            cfg.set_str(key, "PHOTO_FRAME\n");
            for (k, v) in [("retract_when_changing_layer", "0"), ("wipe", "1"), ("wipe_distance", "2"), ("z_hop", "0.4"), ("z_hop_types", "Normal Lift"), ("gcode_flavor", "klipper")] {
                cfg.set_str(k, v);
            }
            cfg.set_str("printer_model", family::FAMILIES[1].models[0]);
            let out = write(&tp, &Job::new(cfg), "test", &|| false);
            let body = out.text.split("; EXECUTABLE_BLOCK_START\n").nth(1).unwrap();
            assert!(body.contains("PHOTO_FRAME\n"), "{key} is written");
            for chunk in body.split("PHOTO_FRAME\n").skip(1) {
                let lines: Vec<_> = chunk.lines().take_while(|l| !(l.starts_with("G1 X") && l.contains(" E") && !l.contains(" E-"))).collect();
                let xy = lines.iter().position(|l| (l.starts_with("G1 X") || l.starts_with("G0 X")) && !l.contains(" E")).unwrap_or(lines.len());
                assert!(!lines[..xy].iter().any(|l| l.contains(" E-") && l.contains(" X") || l.starts_with("G2") || l.starts_with("G3")), "{key}: no wipe or arc before the head is placed again: {lines:?}");
            }
        }
    }

    #[test]
    fn an_embedded_item_pauses_the_print_parked_and_resumes_hot_and_primed() {
        let nut = ObjectEmbed { name: "M3 nut".into(), min: [52.0, 49.0, 0.0], max: [57.0, 51.0, 0.35], cover: 0.5, layer: Some(2) };
        for (named, own) in [(false, None), (true, None), (true, Some("M601\n"))] {
            let (mut cfg, mut tp) = change_job(1, &[0, 0, 0, 0], false);
            for (k, v) in [("printable_height", "250"), ("textured_plate_temp", "60"), ("textured_plate_temp_initial_layer", "65")] {
                cfg.set_str(k, v);
            }
            if named {
                cfg.set_str("printer_model", family::FAMILIES[1].models[0]);
                cfg.set_str("gcode_flavor", "klipper");
            }
            if let Some(own) = own {
                cfg.set_str("machine_pause_gcode", own);
            }
            tp.objects[0].embeds.push(nut.clone());
            let out = write(&tp, &Job::new(cfg), "test", &|| false);
            let case = format!("named {named}, own pause {own:?}");
            assert!(out.warnings.is_empty(), "{case}: {:?}", out.warnings);
            // The progress lines aside, which fall between any two.
            let lines: Vec<&str> = out.text.lines().filter(|l| !l.starts_with("M73 ")).collect();
            let at = |pred: &dyn Fn(&str) -> bool, from: usize| (from..lines.len()).find(|&i| pred(lines[i])).unwrap_or_else(|| panic!("{case}: no such line after {from}: {:?}", &lines[from..(from + 30).min(lines.len())]));
            let mark = at(&|l| l == "; FAB_PAUSE: layer 3 z 0.6: place M3 nut", 0);
            assert_eq!(out.text.matches("; FAB_PAUSE:").count(), 1, "{case}: one pause");
            assert_eq!(lines[..mark].iter().filter(|l| **l == "; CHANGE_LAYER").count(), 2, "{case}: after the item's last cut layer, before the layer over it");
            // Drawn back, lifted over everything printed and parked at the reach's far corner.
            assert!(lines[mark - 3].starts_with("G1 E-1 "), "{case}: retracted first: {:?}", &lines[mark - 3..mark]);
            assert!(lines[mark - 2].starts_with("G1 Z2.4 "), "{case}: 2 mm over the print: {:?}", &lines[mark - 3..mark]);
            assert!(lines[mark - 1].starts_with("G1 X200 Y200 "), "{case}: the corner farthest from the item: {:?}", &lines[mark - 3..mark]);
            // The printer's own pause, then the heaters back before anything moves.
            let pause = if own.is_some() { "M601" } else if named { "PAUSE" } else { "M400 U1" };
            let waits = at(&|l| l == pause, mark);
            assert_eq!(waits, mark + 1, "{case}: the printer's pause right after the stop's tag");
            assert_eq!(lines[waits + 1], "M190 S60", "{case}: the bed back first");
            assert_eq!(lines[waits + 2], if named { "M109 S205 T0" } else { "M109 S205" }, "{case}: then the nozzle, waiting");
            // Back over the print, down onto the layer, primed, then the bead.
            let back = at(&|l| l.starts_with("G1 X50 Y50"), waits);
            let down = at(&|l| l.split_whitespace().nth(1) == Some("Z.6"), back);
            let primed = at(&|l| l.starts_with("G1 E1 "), down);
            let bead = at(&|l| l.starts_with("G1 X60 Y50") && l.contains(" E"), waits);
            assert!(lines[waits..back].iter().any(|l| l.starts_with("G1 Z") && l.split_whitespace().nth(1) != Some("Z.6")), "{case}: the travel back starts over the print: {:?}", &lines[waits..back]);
            assert!(back < down && down < primed && primed < bead, "{case}: return, descend, prime, print: {:?}", &lines[waits..=bead]);
            assert!(!lines[mark..down].iter().any(|l| l.starts_with("G1") && l.contains(" X") && l.contains(" E")), "{case}: nothing extrudes until the head is down");
            // The time model marks the pause where the head waits.
            let pauses: Vec<_> = out.timeline.moves().filter(|m| m.kind == crate::record::MoveKind::Pause).collect();
            assert_eq!(pauses.len(), 1, "{case}");
            assert_eq!((pauses[0].p0, pauses[0].t_end - pauses[0].t_start), ([200.0, 200.0, 2.4], 0.0), "{case}: at the park, of no time the model can know");
            assert_eq!(out.timeline.pauses, vec![crate::process::Pause { layer: 2, z: 0.6, items: vec!["M3 nut".into()] }], "{case}: the time model reads the stop back");
        }
        // An item nothing prints over makes no pause.
        let (cfg, mut tp) = change_job(1, &[0, 0], false);
        tp.objects[0].embeds.push(ObjectEmbed { layer: None, ..nut });
        assert!(!write(&tp, &Job::new(cfg), "test", &|| false).text.contains("FAB_PAUSE"));
    }

    #[test]
    fn second_layer_sets_the_used_plate_temperature_once_without_heating_parked_heads() {
        for enabled in [false, true] {
            for (plate, key) in [
                ("Textured PEI Plate", "textured_plate_temp"), ("Smooth PEI Plate", "hot_plate_temp"),
                ("High Temp Plate", "hot_plate_temp"), ("Cool Plate", "cool_plate_temp"),
                ("Textured Cool Plate", "textured_cool_plate_temp"), ("Engineering Plate", "eng_plate_temp"),
                ("SuperTack Plate", "supertack_plate_temp"),
            ] {
                let (mut cfg, tp) = change_job(3, &[0, 0, 1, 1], false);
                if enabled {
                    cfg.set_str("fab_bed_other_layers_always", "1");
                    cfg.set_str("fab_layer_start_lift_above_new_layer", "1");
                }
                cfg.set_str("curr_bed_type", plate);
                cfg.set_vec(key, ["60", "65", "90"].map(String::from).to_vec());
                cfg.set_vec(&format!("{key}_initial_layer"), ["60", "65", "90"].map(String::from).to_vec());
                cfg.set_str("z_hop", "0.4");
                cfg.set_str("z_hop_types", "Normal Lift");
                let out = write(&tp, &Job::new(cfg), "test", &|| false);
                assert!(out.warnings.is_empty(), "{:?}", out.warnings);
                assert_eq!(out.text.lines().filter(|l| l.starts_with("M140 ")).count(), usize::from(enabled), "bed once with the key, no new default-family line: {plate}");
                if enabled {
                    let layer = out.text.split("; CHANGE_LAYER\n").nth(2).unwrap();
                    let bed = layer.find("M140 S65\n").unwrap();
                    assert!(layer.find("; after layer\n").unwrap() < bed, "after the layer template");
                    assert!(layer.find("M204 S7000\n").unwrap() < bed, "after the travel acceleration");
                    assert!(bed < layer.find("; OBJECT_ID:").unwrap() && bed < layer.find("G1 Z.8").unwrap(), "before the object and lift");
                }
            }
        }
        let (mut cfg, tp) = change_job(4, &[0, 0, 1, 2, 3], false);
        cfg.set_str("printer_model", family::FAMILIES[1].models[0]);
        cfg.set_str("fab_bed_other_layers_always", "1");
        cfg.set_str("enable_pre_heating", "1");
        cfg.set_str("hotend_cooling_rate", "2");
        cfg.set_str("hotend_heating_rate", "1000");
        cfg.set_vec("textured_plate_temp", ["0", "60", "65", "0"].map(String::from).to_vec());
        cfg.set_vec("textured_plate_temp_initial_layer", ["0", "60", "65", "0"].map(String::from).to_vec());
        let out = write(&tp, &Job::new(cfg), "test", &|| false);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.text.lines().filter(|l| *l == "M140 S65").count(), 1, "zero-temperature filaments do not replace the highest used temperature");
        let layer = out.text.split("; CHANGE_LAYER\n").nth(2).unwrap();
        let first = layer.lines().position(|l| l.starts_with("G1 X") && l.contains(" E")).unwrap();
        let temps: Vec<_> = layer.lines().take(first).filter(|l| l.starts_with("M104 ")).collect();
        assert!(!temps.is_empty(), "active head gets its later-layer temperature");
        assert!(temps.iter().all(|l| l.contains("S205") && l.split_whitespace().any(|w| w == "T0")), "only the active head heats at layer 2: {temps:?}");
        for head in 1..4 {
            assert!(out.text.lines().any(|l| l.starts_with("M104 S205 ") && l.split_whitespace().any(|w| w == format!("T{head}"))), "head {head} heats when next used");
        }
    }

    #[test]
    fn second_family_plate_names_objects_and_uses_filament_slots() {
        let mut cfg = Config::new();
        cfg.set_str("printer_model", &format!("{} (test)", family::FAMILIES[1].models[0]));
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 4]);
        cfg.set_vec("filament_map", vec!["4".into(), "3".into()]);
        cfg.set_vec("filament_diameter", vec!["1.75".into(); 2]);
        cfg.set_vec("filament_density", vec!["1.24".into(); 2]);
        cfg.set_vec("nozzle_temperature_initial_layer", vec!["210".into(); 2]);
        cfg.set_vec("nozzle_temperature", vec!["205".into(); 2]);
        cfg.set_str("machine_start_gcode", "G90\nM83\nT0\nM104 S210 T0\nM104 S210 T1\nG1 Z.2 F600\n");
        cfg.set_str("machine_end_gcode", "M104 S0 T0\nM104 S0 T1\n");
        cfg.set_str("gcode_flavor", "klipper");
        cfg.set_str("accel_to_decel_enable", "1");
        cfg.set_str("accel_to_decel_factor", "50%");
        cfg.set_str("bridge_acceleration", "50%");
        cfg.set_str("default_acceleration", "10000");
        cfg.set_str("initial_layer_acceleration", "1000");
        cfg.set_str("initial_layer_travel_acceleration", "0");
        cfg.set_str("travel_acceleration", "0");
        cfg.set_vec("outer_wall_acceleration", ["1000", "2000", "4000", "6000"].map(String::from).to_vec());
        for (key, value) in [("machine_max_acceleration_x", "8000"), ("machine_max_acceleration_extruding", "8000"), ("machine_max_speed_x", "500"), ("machine_max_jerk_x", "10")] {
            cfg.set_str(key, value);
        }
        cfg.set_str("initial_layer_flow_ratio", "1");
        cfg.set_str("travel_speed", "100");
        cfg.set_str("enable_pre_heating", "1");
        cfg.set_str("time_lapse_gcode", "; shared photo\n");
        cfg.set_vec("hotend_cooling_rate", vec!["2".into(); 4]);
        cfg.set_vec("hotend_heating_rate", vec!["4".into(); 4]);
        cfg.set_vec("z_hop", vec!["0.4".into(); 4]);
        cfg.set_vec("z_hop_types", vec!["Spiral Lift".into(); 4]);
        let job = Job::new(cfg);
        let mut tp = Toolpaths::default();
        for object in 0..2 {
            let x = 10.0 + object as f64 * 20.0;
            tp.objects.push(ObjectInfo { name: format!(" part ! {object} "), filament: object, min: [x, 10.0, 0.0], max: [x + 5.0, 15.0, 0.6], overrides: Vec::new(), embeds: Vec::new() });
        }
        tp.objects[1].overrides.push(("outer_wall_acceleration".into(), "50%".into()));
        for li in 0..3 {
            let mut layer = Layer { index: li, print_z: (li + 1) as f64 * 0.2, height: 0.2, ..Default::default() };
            for object in 0..2 {
                let x = tp.objects[object].min[0];
                let path = ExtrusionPath {
                    role: Role::OuterWall, pts: vec![Pt::from_mm(x, 10.0), Pt::from_mm(x + 5.0, 10.0), Pt::from_mm(x + 5.0, 15.0), Pt::from_mm(x, 15.0), Pt::from_mm(x, 10.0)],
                    width: 0.4, height: 0.2, area: 0.08, speed: 10.0, ..Default::default()
                };
                let fills = if li == 1 {
                    [Role::Bridge, Role::InternalBridge].into_iter().enumerate().map(|(i, role)| ExtrusionPath {
                        role, pts: vec![Pt::from_mm(x, 11.0 + i as f64), Pt::from_mm(x + 5.0, 11.0 + i as f64)],
                        width: 0.4, height: 0.2, area: 0.08, speed: 10.0, ..Default::default()
                    }).collect()
                } else { Vec::new() };
                layer.objects.push(ObjectLayer { object, filament: object, islands: vec![std::sync::Arc::new(Island { walls: vec![path], fills })], ..Default::default() });
            }
            tp.layers.push(layer);
        }
        let out = write(&tp, &job, "test", &|| false);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let text = &out.text;
        assert_eq!(text.lines().filter(|l| l.starts_with("EXCLUDE_OBJECT_DEFINE ")).count(), 2);
        let mut open = None;
        let mut starts = 0;
        let mut accelerations = 0;
        for line in text.lines() {
            assert!(!matches!(line.split_whitespace().next(), Some("M201" | "M203" | "M204" | "M205")), "unexpected machine command: {line}");
            if let Some(args) = line.strip_prefix("SET_VELOCITY_LIMIT ") {
                let args: Vec<_> = args.split_whitespace().collect();
                assert_eq!(args.len(), 2, "only acceleration and acceleration-to-deceleration: {line}");
                let a: f64 = args[0].strip_prefix("ACCEL=").unwrap().parse().unwrap();
                let d: f64 = args[1].strip_prefix("ACCEL_TO_DECEL=").unwrap().parse().unwrap();
                assert!(a > 0.0, "zero acceleration leaves the last value in force");
                assert_eq!(d, a / 2.0, "{line}");
                accelerations += 1;
            }
            if let Some(name) = line.strip_prefix("EXCLUDE_OBJECT_START NAME=") {
                assert!(open.replace(name).is_none(), "nested object run");
                starts += 1;
            } else if let Some(name) = line.strip_prefix("EXCLUDE_OBJECT_END NAME=") {
                assert_eq!(open.take(), Some(name), "object end matches its start");
            }
            if line == "; shared photo" { assert!(open.is_none(), "shared commands are outside object runs"); }
            if line.starts_with("M104 ") {
                let tool = line.split_whitespace().find(|w| w.starts_with('T'));
                assert!(matches!(tool, Some("T0" | "T1")), "filament slot: {line}");
            }
        }
        assert!(accelerations > 0, "the plate changes acceleration");
        for (object, outer) in [(0, 6000.0), (1, 5000.0)] {
            for role in [Role::OuterWall, Role::Bridge, Role::InternalBridge] {
                let moves: Vec<_> = out.timeline.moves().filter(|m| m.layer == 1 && m.object == object && m.role == role && m.kind == crate::record::MoveKind::Extrude).collect();
                assert!(!moves.is_empty(), "object {object}, {role:?} has extrusion on layer 2");
                let expected = if role == Role::OuterWall { outer } else { outer / 2.0 };
                assert!(moves.iter().all(|m| m.accel == expected), "object {object}, {role:?}: expected {expected}");
            }
        }
        assert!(starts >= 6, "each object on every layer is bracketed");
        assert!(open.is_none(), "final object run ends");
        for forbidden in ["M624", "M625", "M981", "M1003", "M73 E", ";VT", "G3 Z", "M632", "M633"] {
            assert!(!text.contains(forbidden), "unexpected {forbidden}");
        }
        assert!(text.find("; CONFIG_BLOCK_START").unwrap() > text.find("; EXECUTABLE_BLOCK_END").unwrap());
        let footprints = named_objects(&tp);
        assert_eq!(footprints[0].center, [12.5, 12.5]);
        assert_eq!(footprints[0].polygon.len(), 4);
        // With slices, the footprint is their hull over every layer, wider than the paths.
        let square = |a: f64, b: f64| makepad_fab_slicer::geom::ExPolygon { contour: makepad_fab_slicer::geom::Polygon { pts: vec![Pt::from_mm(a, a), Pt::from_mm(b, a), Pt::from_mm(b, b), Pt::from_mm(a, b)] }, holes: Vec::new() };
        tp.layers[0].objects[0].slices = vec![square(9.0, 16.0)].into();
        tp.layers[1].objects[0].slices = vec![square(8.0, 12.0)].into();
        let hull = &named_objects(&tp)[0].polygon;
        assert!(hull.contains(&[8.0, 8.0]) && hull.contains(&[16.0, 16.0]) && !hull.contains(&[9.0, 9.0]), "{hull:?}");
        tp.layers.clear();
        assert_eq!(named_objects(&tp)[0].polygon, vec![[10.0, 10.0], [15.0, 10.0], [15.0, 15.0], [10.0, 15.0]]);
    }

    #[test]
    fn bridge_acceleration_resolves_percent_absolute_fallback_and_zero_travel() {
        for (bridge, expected) in [(None, 10000.0), (Some("50%"), 3000.0), (Some("1700"), 1700.0), (Some("0"), 10000.0)] {
            let mut cfg = Config::new();
            cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 2]);
            cfg.set_vec("filament_map", vec!["2".into(), "1".into()]);
            cfg.set_str("default_acceleration", "10000");
            cfg.set_vec("outer_wall_acceleration", vec!["2000".into(), "60%".into()]);
            cfg.set_str("initial_layer_acceleration", "1000");
            cfg.set_str("travel_acceleration", "0");
            cfg.set_str("initial_layer_travel_acceleration", "0");
            if let Some(value) = bridge { cfg.set_str("bridge_acceleration", value); }
            let job = Job::new(cfg);
            let mut w = Writer::new(&job, family::of(&job.cfg).expect("claimed fixture"), Sink::Model(Box::new(Processor::new(&job))));
            let mut object = ObjectInfo { name: "part".into(), filament: 0, min: [0.0; 3], max: [1.0; 3], overrides: Vec::new(), embeds: Vec::new() };
            for own in [false, true] {
                if own {
                    object.overrides = vec![("outer_wall_acceleration".into(), "60%".into())];
                    w.object_accels = vec![ObjectAccels::of(&job, &object)];
                }
                for role in [Role::Bridge, Role::InternalBridge] {
                    assert_eq!(w.accel_for(role, false), expected, "bridge {bridge:?}, own {own}, {role:?}");
                    assert_eq!(w.accel_for(role, true), 1000.0, "first-layer bridge {bridge:?}, own {own}, {role:?}");
                }
            }
            object.overrides = vec![("bridge_acceleration".into(), "25%".into())];
            w.object_accels = vec![ObjectAccels::of(&job, &object)];
            assert_eq!(w.accel_for(Role::Bridge, false), 1500.0, "a bridge-only object override is applied");
            w.filament = 1;
            assert_eq!(w.accel_for(Role::Bridge, false), 500.0, "the object's other extruder takes its own outer wall's");
            w.filament = 0;
            w.set_accel(6000.0);
            w.set_accel(w.travel_accel(true));
            w.set_accel(w.travel_accel(false));
            w.set_accel(6000.0);
            assert_eq!(w.accel, 6000.0, "zero travel preserves the last acceleration");
            w.set_accel(6000.2);
            assert_eq!(w.accel, 6000.0, "a value that writes the same line is the one in force");
            let Sink::Model(model) = w.sink else { unreachable!() };
            assert_eq!(model.finish().line_end.len(), 1, "zero travel and unchanged acceleration emit nothing");
        }
    }

    /// The outgoing filament rams its ramming length on the default family's printers, and its
    /// multitool ramming volume on a toolchanger whose presets turn that ramming on.
    #[test]
    fn a_head_change_rams_what_its_family_sets() {
        let job = |keys: &[(&str, &str)]| {
            let mut cfg = Config::new();
            cfg.set_vec("filament_diameter", vec!["1.75".into(); 2]);
            for &(k, v) in keys {
                cfg.set_vec(k, vec![v.into(); 2]);
            }
            Job::new(cfg)
        };
        let area = std::f64::consts::PI * 1.75 * 1.75 / 4.0;
        assert!((ram_volume(&job(&[("filament_change_length", "10")]), 1) - 10.0 * area).abs() < 1e-9);
        let multitool = [("filament_change_length", "0"), ("filament_multitool_ramming", "1"), ("filament_multitool_ramming_volume", "5")];
        assert_eq!(ram_volume(&job(&multitool), 1), 5.0);
        assert_eq!(ram_volume(&job(&[multitool[0], ("filament_multitool_ramming", "0"), multitool[2]]), 1), 0.0, "ramming off rams nothing");
    }

    /// The prime rows after a change: a family that climbs steps them up to the layer's target, the
    /// first layer's speed on the first layer, else the lower of the tower's purge speed and the
    /// sparse infill's; the default family's rows hold the fifth row's set speed.
    #[test]
    fn the_prime_rows_speed_up_as_their_family_lays_them() {
        let job = |model: &str| {
            let mut cfg = Config::new();
            for (k, v) in [("printer_model", model), ("prime_tower_max_speed", "90"), ("initial_layer_speed", "50"), ("sparse_infill_speed", "270")] {
                cfg.set_str(k, v);
            }
            Job::new(cfg)
        };
        let feeds = |job: &Job, first: bool| (0..20).map(|k| (prime_row_speed(job, family::of(&job.cfg).expect("claimed fixture"), k, first, 0) * 60.0 * 1000.0).round() / 1000.0).collect::<Vec<_>>();
        let climb = job(&format!("{} (test)", family::FAMILIES[1].models[0]));
        let later = feeds(&climb, false);
        assert_eq!(later[..6], [1782.0, 2025.0, 2473.0, 4725.0, 4775.0, 4825.0]);
        assert_eq!((later[15], later[16], later[17], later[19]), (5325.0, 5375.0, 5400.0, 5400.0), "the rows climb to the purge speed and stay there");
        assert_eq!(feeds(&climb, true)[..6], [990.0, 1125.0, 1374.0, 2625.0, 2675.0, 2725.0], "the first layer climbs to its own speed");
        assert_eq!(feeds(&climb, true)[19], 3000.0);
        let mut slow = climb.cfg.clone();
        slow.set_str("sparse_infill_speed", "60");
        assert_eq!(feeds(&Job::new(slow), false)[19], 3600.0, "the sparse infill's speed caps the target");
        let hold = feeds(&job("Bambu Lab H2D"), false);
        assert_eq!(hold[3..6], [4725.0, 4774.998, 4774.998], "the default family holds the fifth row");
    }

    /// A tower whose spot is near the plate's back edge moves until its first layer, ribs and
    /// brim included, prints on the plate; its ribs reach half their extra length past a corner
    /// there, and none on its top layer.
    #[test]
    fn a_tower_s_first_layer_prints_on_the_plate() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(); 2]);
        cfg.set_vec("filament_map", vec!["1".into(), "2".into()]);
        for (k, v) in [("filament_diameter", "1.75"), ("filament_prime_volume", "45"), ("filament_change_length", "10")] {
            cfg.set_vec(k, vec![v.into(); 2]);
        }
        for (k, v) in [("printable_area", "0x0,270x0,270x270,0x270"), ("layer_height", "0.2"), ("prime_tower_rib_wall", "1"), ("prime_tower_rib_width", "8"), ("prime_tower_extra_rib_length", "8"), ("prime_tower_brim_width", "8"), ("wipe_tower_x", "150"), ("wipe_tower_y", "240")] {
            cfg.set_str(k, v);
        }
        let job = Job::new(cfg);
        let mut tp = Toolpaths::default();
        tp.objects.push(ObjectInfo { name: "part".into(), filament: 0, min: [100.0, 100.0, 0.0], max: [120.0, 120.0, 2.0], overrides: Vec::new(), embeds: Vec::new() });
        for li in 0..10 {
            tp.layers.push(Layer { index: li, print_z: (li + 1) as f64 * 0.2, height: 0.2, ..Default::default() });
        }
        let t = plan_tower(&job, &tp, &[0, 1], &vec![vec![(0, 1), (1, 0)]; 10], 9);
        let b = BBox::of(&tower_first_footprint(&job, &t));
        let (lo, hi) = ((b.min.x_mm(), b.min.y_mm()), (b.max.x_mm(), b.max.y_mm()));
        assert!(lo.0 >= 0.0 && lo.1 >= 0.0 && hi.0 <= 270.0 && hi.1 <= 270.0, "the first layer reaches {lo:?}..{hi:?}");
        // How far the outline reaches past the box's corner along its diagonal.
        let past = |z: f64| {
            let tip = t.outline(0.0, z).into_iter().flat_map(|e| e.contour.pts).map(|p| p.x_mm() + p.y_mm()).fold(f64::MIN, f64::max);
            (tip - (t.x + t.w) - (t.y + t.d)) / std::f64::consts::SQRT_2
        };
        let (base, top) = (past(t.z0), past(t.top));
        assert!(top.abs() < 0.05, "the ribs end at the corners on the top layer: {top}");
        assert!((base - 4.0 * 0.9).abs() < 0.05, "half the extra, less a tenth for the first layer's height, past each corner: {base}");
    }

    /// A layer prints each nozzle's filaments together, the nozzle printing now first, each
    /// nozzle from the filament it holds and ending on one the next layer prints: on two nozzles
    /// (filaments 0, 1 and 2 on the left, 3 on the right) every change it saves is a swap less.
    #[test]
    fn a_layer_prints_its_filaments_in_the_order_that_swaps_least() {
        let mut cfg = Config::new();
        cfg.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        cfg.set_vec("filament_map", ["1", "1", "1", "2"].map(String::from).to_vec());
        cfg.set_vec("flush_volumes_matrix", (0..2 * 4 * 4).map(|i| if i % 4 == (i / 4) % 4 { "0" } else { "100" }.to_string()).collect());
        let job = Job::new(cfg);
        let changes = makepad_fab_settings::change::ChangeModel::new(&job);
        let order = |present: &[usize], loaded: usize, held: [Option<usize>; 2], next: &[usize]| filament_order(present, loaded, &held, next, &changes);
        // the right nozzle prints now: it goes first, then the left from the filament it holds
        assert_eq!(order(&[1, 0, 3], 3, [Some(0), Some(3)], &[0, 1, 3]), vec![3, 0, 1]);
        assert_eq!(order(&[1, 0, 3], 3, [Some(1), Some(3)], &[0, 1, 3]), vec![3, 1, 0]);
        // the next layer prints only filament 1 on the left: this one ends on it
        assert_eq!(order(&[0, 1, 2], 0, [Some(0), None], &[1]), vec![0, 2, 1]);
        // in order of first use when nothing tells them apart
        assert_eq!(order(&[2, 1], 3, [None, Some(3)], &[]), vec![2, 1]);
    }

    #[test]
    fn nan_path_coordinate_keeps_the_nonfinite_warning_count() {
        let mut cfg = Config::new();
        cfg.set_str("machine_start_gcode", "G90\nM83\nG1 Z.2 F600\n");
        cfg.set_str("machine_end_gcode", "M104 S0\n");
        cfg.set_str("filament_diameter", "1.75");
        cfg.set_str("nozzle_diameter", "0.4");
        cfg.set_str("initial_layer_flow_ratio", "1");
        cfg.set_str("travel_speed", "100");
        let job = Job::new(cfg);
        let path = ExtrusionPath {
            role: Role::OuterWall,
            pts: vec![Pt::from_mm(10.0, 10.0), Pt::from_mm(11.0, 10.0), Pt::from_mm(12.0, 10.0)],
            z: vec![0.0, f64::NAN, 0.0],
            width: 0.4,
            height: 0.2,
            area: 0.08,
            speed: 10.0,
            ..Default::default()
        };
        let tp = Toolpaths {
            objects: vec![ObjectInfo { name: "part".into(), filament: 0, min: [10.0, 10.0, 0.0], max: [12.0, 10.0, 0.2], overrides: Vec::new(), embeds: Vec::new() }],
            layers: vec![Layer {
                print_z: 0.2,
                height: 0.2,
                objects: vec![ObjectLayer {
                    islands: vec![std::sync::Arc::new(Island { walls: vec![path], fills: Vec::new() })],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        // Each segment touching the NaN Z produces one non-finite extrusion.
        let out = write(&tp, &job, "test", &|| false);
        assert_eq!(out.text.matches("ENaN").count(), 2, "both affected segments are written");
        assert_eq!(out.warnings, ["the G-code has 2 non-finite numbers (a NaN or infinite coordinate, feed or extrusion)"], "the final warning includes the chunk formatters' counts");
        // Force each consumer path, without relying on which thread wins the queue.
        for ahead in [false, true] {
            let pipe = Pipe::new();
            let (w, _, _, _, _) = write_lines(&tp, &job, family::of(&job.cfg).expect("claimed fixture"), Sink::Text(&pipe), &|| false);
            if ahead { pipe.read_ahead(&|| false); }
            let (_, chunks, ends, nonfinite) = time_model(&job, &[], &pipe, &|| false);
            assert_eq!(nonfinite + w.nonfinite, 2, "read_ahead={ahead}: preserve every count");
            assert_eq!(ends.len(), w.lines, "read_ahead={ahead}: preserve line indices");
            assert_eq!(ends.last().copied(), Some(chunks.iter().map(String::len).sum()), "read_ahead={ahead}: preserve byte offsets");
        }
    }

    /// A stop ends the writing with its warning alone, wherever it is seen: by the writer between
    /// layers, or by the time model or a reader between chunks, each of which leaves the chunks
    /// after it unread; even a stop that turns false again, as what a part cut short cannot be put
    /// together. Never asked, the whole plate.
    #[test]
    fn a_stop_between_chunks_leaves_the_rest_unread() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (cfg, tp) = change_job(1, &[0; 600], false);
        let job = Job::new(cfg);
        let family = family::of(&job.cfg).expect("claimed fixture");
        // A stop from its `k`th ask on, or at that ask alone.
        let from = |k: usize| {
            let asked = AtomicUsize::new(0);
            move || asked.fetch_add(1, Ordering::Relaxed) >= k
        };
        let at = |k: usize| {
            let asked = AtomicUsize::new(0);
            move || asked.fetch_add(1, Ordering::Relaxed) == k
        };
        let sent = || {
            let pipe = Pipe::new();
            write_lines(&tp, &job, family, Sink::Text(&pipe), &|| false);
            pipe
        };
        let left = |pipe: &Pipe| pipe.q.lock().unwrap().raw.len();
        let n = left(&sent());
        assert!(n >= 2, "{n} chunks");
        for k in 0..n {
            let pipe = sent();
            let (_, chunks, ends, _) = time_model(&job, &[], &pipe, &from(k));
            assert!(chunks.is_empty() && ends.is_empty(), "the model keeps nothing of {k} chunks");
            assert_eq!(left(&pipe), n - k, "the model reads {k} chunks and leaves the rest");
            let pipe = sent();
            pipe.read_ahead(&from(k));
            assert_eq!(left(&pipe), n - k, "a reader reads {k} chunks and leaves the rest");
        }
        let whole = write(&tp, &job, "test", &|| false);
        assert!(whole.warnings.is_empty() && !whole.text.is_empty(), "{:?}", whole.warnings);
        for k in (0..tp.layers.len()).step_by(41) {
            for out in [write(&tp, &job, "test", &from(k)), write(&tp, &job, "test", &at(k))] {
                assert_eq!((out.text.as_str(), out.warnings.as_slice()), ("", [STOPPED.to_string()].as_slice()), "a stop at ask {k}");
            }
        }
    }

    /// Two walls far apart on one layer, written with the wipe settings given: the lines from the
    /// first retraction to the end of its wipe.
    fn first_wipe(keys: &[(&str, &str)], first_len: f64) -> Vec<String> {
        let mut cfg = Config::new();
        cfg.set_str("machine_start_gcode", "G90\nM83\nG1 Z.2 F600\n");
        cfg.set_str("machine_end_gcode", "M104 S0\n");
        for (k, v) in [("filament_diameter", "1.75"), ("nozzle_diameter", "0.4"), ("initial_layer_flow_ratio", "1"), ("travel_speed", "100"),
            ("retraction_length", "1.2"), ("retraction_speed", "30"), ("retraction_minimum_travel", "1"), ("wipe", "1"), ("wipe_distance", "2"),
            ("wipe_speed", "60"), ("retract_before_wipe", "0"), ("z_hop", "0")] {
            cfg.set_str(k, v);
        }
        for &(k, v) in keys { cfg.set_str(k, v); }
        let job = Job::new(cfg);
        let wall = |x: f64, len: f64| ExtrusionPath {
            role: Role::OuterWall, pts: vec![Pt::from_mm(x, 10.0), Pt::from_mm(x + len, 10.0)],
            width: 0.4, height: 0.2, area: 0.08, speed: 10.0, ..Default::default()
        };
        let tp = Toolpaths {
            objects: vec![ObjectInfo { name: "part".into(), filament: 0, min: [10.0, 10.0, 0.0], max: [60.0, 10.0, 0.2], overrides: Vec::new(), embeds: Vec::new() }],
            layers: vec![Layer {
                print_z: 0.2, height: 0.2,
                objects: vec![ObjectLayer { islands: vec![std::sync::Arc::new(Island { walls: vec![wall(10.0, first_len), wall(50.0, 10.0)], fills: Vec::new() })], ..Default::default() }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let text = write(&tp, &job, "test", &|| false).text;
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let first = lines.iter().position(|l| l.starts_with("G1 X") && l.contains(" E") && !l.contains(" E-")).expect("an extrusion");
        let from = first + lines[first..].iter().position(|l| l.contains(" E-") || l == "; WIPE_START").expect("a retraction");
        let to = from + lines[from..].iter().position(|l| l == "; WIPE_END" || l.starts_with("G1 Z") || (l.starts_with("G1 X") && !l.contains(" E"))).unwrap_or(0);
        lines[from..to.max(from + 1)].to_vec()
    }

    fn retracted(lines: &[String]) -> (f64, f64) {
        let e = |l: &String| l.split_whitespace().find_map(|w| w.strip_prefix("E-")).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let wipe = lines.iter().position(|l| l == "; WIPE_START").unwrap_or(lines.len());
        (lines[..wipe].iter().map(e).sum(), lines[wipe..].iter().map(e).sum())
    }

    /// The other slicer's wipe: during the wipe only what the retraction speed draws back over
    /// its length (30 mm/s over 2 mm at 60 mm/s is 1 mm), at most what retract_before_wipe leaves;
    /// the rest of the 1.2 mm in place before it, at the retraction speed.
    #[test]
    fn a_wipe_at_retraction_speed_retracts_the_rest_in_place_first() {
        let own = [("fab_wipe_retract_at_retraction_speed", "1")];
        let close = |(a, b): (f64, f64), (x, y): (f64, f64)| (a - x).abs() < 1e-4 && (b - y).abs() < 1e-4;
        let lines = first_wipe(&own, 10.0);
        assert!(close(retracted(&lines), (0.2, 1.0)), "{lines:?}");
        assert_eq!(lines[0], "G1 E-.2 F1800", "in place first, at the retraction speed");
        assert_eq!(lines[1], "; WIPE_START");
        assert!(lines.iter().any(|l| l == "G1 F3600"), "the wipe at its own speed: {lines:?}");
        let lines = first_wipe(&[own[0], ("retract_before_wipe", "50")], 10.0);
        assert!(close(retracted(&lines), (0.6, 0.6)), "half before the wipe: {lines:?}");
        let lines = first_wipe(&own, 1.0);
        assert!(close(retracted(&lines), (0.7, 0.5)), "a 1 mm path wipes 1 mm: {lines:?}");
        let lines = first_wipe(&[own[0], ("wipe_distance", "0")], 10.0);
        assert!(close(retracted(&lines), (1.2, 0.0)) && !lines.iter().any(|l| l == "; WIPE_START"), "no wipe: {lines:?}");
        // 30 mm/s over 2 mm at 10 mm/s (never slower) is 6 mm of a 10 mm retraction.
        let lines = first_wipe(&[own[0], ("wipe_speed", "5"), ("retraction_length", "10")], 10.0);
        assert!(close(retracted(&lines), (4.0, 6.0)), "the wipe runs at 10 mm/s at least: {lines:?}");
        // Without the key, the default family's wipe is unchanged: most of it during the wipe.
        let lines = first_wipe(&[], 10.0);
        assert!(lines[0] != "G1 E-.2 F1800" && retracted(&lines).1 > 1.1, "{lines:?}");
    }

    #[test]
    fn a_blended_fan_with_equal_ends_is_that_speed() {
        let mut cfg = Config::new();
        for (k, v) in [("fan_min_speed", "100"), ("fan_max_speed", "100"), ("slow_down_layer_time", "8"), ("fan_cooling_layer_time", "100"), ("close_fan_the_first_x_layers", "1")] {
            cfg.set_str(k, v);
        }
        let job = Job::new(cfg);
        let w = Writer::new(&job, family::of(&job.cfg).expect("claimed fixture"), Sink::Model(Box::new(Processor::new(&job))));
        for i in 0..9200 {
            let t = 8.0 + i as f64 * 0.01;
            assert_eq!(w.fan_for_layer(5, t).0, 255.0, "layer time {t}");
        }
    }

    /// The internal bridge's fan and the overhang fan's rule apply only to a job that prints
    /// internal bridges as their own role.
    #[test]
    fn internal_bridges_get_their_fan_only_when_the_job_has_them() {
        for has in [false, true] {
            let mut cfg = Config::new();
            for (k, v) in [("enable_overhang_bridge_fan", "1"), ("overhang_fan_speed", "80"), ("internal_bridge_fan_speed", "40")] { cfg.set_str(k, v); }
            if has { cfg.set_str("internal_bridge_speed", "150%"); }
            let job = Job::new(cfg);
            let mut w = Writer::new(&job, family::of(&job.cfg).expect("claimed fixture"), Sink::Model(Box::new(Processor::new(&job))));
            w.overhang_fan = Some(204.0);
            let path = ExtrusionPath { role: Role::InternalBridge, ..Default::default() };
            assert_eq!(w.wants_overhang_fan(&path), has);
            assert_eq!(w.internal_bridges(), has);
            assert_eq!(w.internal_bridge_fan(), Some(102.0), "40 % when set");
        }
    }
}
