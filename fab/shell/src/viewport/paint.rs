//! The plate's paint as the views draw it: each painted part's program (`fab/paint_field`)
//! baked to a sparse brick volume on a worker thread, every volume packed after the element
//! table in the element lookup's texture (`elements.rs`), which the mesh lanes
//! (`mod.draw.PaintLookup` in `libs/render`'s static shader, by element) and the field shade
//! (`draw_tiles_lit`, by object) sample once per pixel: no texture slot of its own.
//! Paint never touches a part's field or mesh: a stroke rebakes paint bricks only.
//!
//! Force's study of a part goes in the same pack, over the part's paint (`super::force`): the
//! lanes that read paint read its colours and marks, on the real field or the real mesh alike.
//!
//! A stroke being drawn shows at once: its points join a copy of the part's strokes log (the
//! file its script reads, `fab_paint_field::strokes`) and only the bricks the new capsules
//! reach are baked again, here, on the UI thread (a few bricks per move). When the stroke lands
//! in the document only the bricks the log's change reaches bake again, on a worker; the rest of
//! the volume is kept.

use crate::api::{AppState, PaintTarget, PaintView};
use fab_paint_field::bake::{AtlasPart, OverlayFrame, Surface};
use fab_paint_field::{Channel, Layer, Op, Program, Region, StrokeOp, StrokeSwatch, Strokes, Volume};
use makepad_widgets_core::makepad_platform::thread::{checkpoint, Class, SignalToUI, TaskHandle, Version};
use makepad_widgets_core::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

/// What is being painted now, for [`PaintLayer::set_live`]: a stroke being drawn (printer mm
/// points, radius mm), or what a fill click would paint (regions in the part's model frame).
pub enum LivePaint<'a> {
    Stroke { object: &'a str, points: &'a [[f64; 3]], radius: f64, rgb: [f32; 3], erase: bool },
    Fill { object: &'a str, regions: Vec<Region>, names: Vec<String>, rgb: [f32; 3] },
}

impl LivePaint<'_> {
    fn object(&self) -> &str {
        match self {
            LivePaint::Stroke { object, .. } | LivePaint::Fill { object, .. } => object,
        }
    }
    fn rgb(&self) -> [f32; 3] {
        match self {
            LivePaint::Stroke { rgb, .. } | LivePaint::Fill { rgb, .. } => *rgb,
        }
    }
}

#[derive(Clone, PartialEq)]
enum LiveKey {
    /// Model points, radius, colour slot, erase.
    Stroke(Vec<[f64; 3]>, f64, u8, bool),
    Fill(Vec<String>, u8),
}

/// The live paint's volume: its key, the palette with its colour, the volume and its frame,
/// and for a stroke the program it is drawn into and the strokes file that holds it.
struct Live {
    key: LiveKey,
    palette: Vec<[f32; 3]>,
    vol: Volume,
    stroke: Option<(Program, String)>,
}

/// A field part's shape as a bake reads its surface: its key, and its frame into the paint
/// program's.
type FieldAt = ([u8; 32], [[f64; 4]; 3]);

/// How far apart two frames or boxes may lie (mm, and shares of one for a turn) and still be
/// one: the same one reached by another placement's arithmetic.
const SAME_MM: f64 = 1e-6;

/// Whether box `outer` holds box `inner` (to [`SAME_MM`]).
fn holds(outer: ([f64; 3], [f64; 3]), inner: ([f64; 3], [f64; 3])) -> bool {
    (0..3).all(|k| inner.0[k] >= outer.0[k] - SAME_MM && inner.1[k] <= outer.1[k] + SAME_MM)
}

/// Whether two fields are one: the same shape in the same place in the paint's frame (to
/// [`SAME_MM`]), or none.
fn same_field(a: Option<FieldAt>, b: Option<FieldAt>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((ka, ma)), Some((kb, mb))) => ka == kb && (0..3).all(|r| (0..4).all(|c| (ma[r][c] - mb[r][c]).abs() <= SAME_MM)),
        _ => false,
    }
}

/// A finished bake: the volume, the program it is of, the field it was baked round and that
/// field's surface as the bake read it.
type Baked = (Volume, Arc<Program>, Option<FieldAt>, Option<Arc<Surface>>);

/// Wakes the view when a bake ends, however it ends (done, given way to a newer one, never run):
/// the view takes it then, and asks for nothing while it runs.
struct BakeEnd(Arc<AtomicBool>);

impl Drop for BakeEnd {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        SignalToUI::set_ui_signal();
    }
}

struct PaintPart {
    version: Version,
    object: String,
    view: PaintView,
    /// The requested box in the program's frame (the pending bake's extent).
    bounds: ([f64; 3], [f64; 3]),
    /// The field the requested bake bakes round.
    field: Option<FieldAt>,
    /// The completed volume's extent, which stays put while a new extent bakes.
    vol_bounds: ([f64; 3], [f64; 3]),
    /// The field the completed volume was baked round; None: every brick of its extent.
    vol_field: Option<FieldAt>,
    /// That field's surface with what the bake read of it: the next bake reads the field it
    /// bakes round again only where it differs.
    vol_surface: Option<Arc<Surface>>,
    /// The baked volume (in the program's own frame, wherever the part stands) and the program
    /// it is of.
    vol: Option<(Arc<Volume>, Arc<Program>)>,
    job: Option<mpsc::Receiver<Baked>>,
    /// The stroke being drawn on it (program frame) and the volume it shows.
    live: Option<Live>,
}

/// What the field shade binds: the texture holding the atlas, its `paint_ctl`, and per object
/// its field header texel.
pub struct PaintBinding {
    pub texture: Texture,
    pub ctl: [f32; 4],
    sdf: Vec<(String, f32)>,
}

impl PaintBinding {
    pub fn sdf_header(&self, object: &str) -> Option<f32> {
        self.sdf.iter().find(|(o, _)| o == object).map(|(_, h)| *h)
    }
}

/// The atlas packed for the element lookup: its texels (from `base`), each element's header
/// texel, and each field part's.
pub struct PaintPack {
    pub data: Vec<f32>,
    pub elements: Vec<(usize, f32)>,
    pub sdf: Vec<(String, f32)>,
    /// What was packed coarser to fit the device's largest texture, in words (empty: nothing).
    pub coarser: String,
}

#[derive(Default)]
pub struct PaintLayer {
    parts: Vec<PaintPart>,
    /// A test can stop the next job before it bakes, without timing the workers.
    #[cfg(test)]
    bake_step: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    binding: Option<PaintBinding>,
    /// Moves whenever what the atlas holds changes (the element lookup packs it again).
    revision: u64,
    /// A bake ended since the view last asked ([`PaintLayer::take_woke`]).
    woke: Arc<AtomicBool>,
    sdf: Vec<(String, f32)>,
    /// The light floor the atlas holds for every part (`shown_floor`).
    floor: f32,
}

/// The printer box of a part through `m` (its corners).
fn bounds_through(min: [f64; 3], max: [f64; 3], m: &[[f64; 4]; 3]) -> ([f64; 3], [f64; 3]) {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for c in 0..8 {
        let p = [if c & 1 == 0 { min[0] } else { max[0] }, if c & 2 == 0 { min[1] } else { max[1] }, if c & 4 == 0 { min[2] } else { max[2] }];
        for r in 0..3 {
            let v = m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3];
            lo[r] = lo[r].min(v);
            hi[r] = hi[r].max(v);
        }
    }
    (lo, hi)
}

/// The least light a support channel's tint shows at, as a share of the tint on the display.
const SUPPORT_TINT_FLOOR: f32 = 0.7;

/// The light floor of the paint shown (`AtlasPart::floor`, in the renderer's convention: a share
/// of the colour at the renderer's exposure): a support channel's tint must read where it is
/// painted most, under an overhang that no light reaches, through the composite's own bias on
/// that exposure; colour paint is lit like the part.
fn shown_floor(state: &AppState) -> f32 {
    if shown_target(state) == PaintTarget::Color { 0.0 } else { SUPPORT_TINT_FLOOR / super::display_tone(&state.view, state.view.lighting_for(&state.ui)).0 }
}

/// What the views show of paint: while the paint tool paints a support channel, that channel;
/// else the colour.
fn shown_target(state: &AppState) -> PaintTarget {
    if crate::tools::paint::active(state) { state.ui.paint_target } else { PaintTarget::Color }
}

/// A bake job's bricks on the pool's workers: a newer bake of the part ends this one.
fn job_fan(pool: &TaskPool, len: usize, f: &(dyn Fn(usize) + Sync)) {
    pool.fan_out(len, |i| {
        if checkpoint().is_ok() {
            f(i)
        }
    })
}

/// `program`'s colour over `bounds` at its own pitch, only the bricks round the field's surface
/// where it has one (`field`: its shape, and its frame into the program's). None: the job ended
/// first.
fn bake_volume(pool: &TaskPool, program: &Program, bounds: ([f64; 3], [f64; 3]), pitch: f64, field: Option<(&fab_sdf_engine::makepad_csg_sdf::SdfShape, &[[f64; 4]; 3])>) -> Option<Volume> {
    bake_volume_after(pool, program, bounds, pitch, field, None, None).map(|(v, ..)| v)
}

/// [`bake_volume`] after an edit, from the volume baked before it (`was`, of the program `old`):
/// only the bricks the edit can change bake ([`Volume::bake_again`]), all of them where the
/// program changed otherwise; and the field's surface read again only where it differs from
/// `read_before`'s ([`Surface::after`]). Also how many bricks it baked again (None: a whole
/// bake), and the surface as it read it.
fn bake_volume_after(pool: &TaskPool, program: &Program, bounds: ([f64; 3], [f64; 3]), pitch: f64, field: Option<(&fab_sdf_engine::makepad_csg_sdf::SdfShape, &[[f64; 4]; 3])>, was: Option<(&Volume, &Program)>, read_before: Option<&Surface>) -> Option<(Volume, Option<usize>, Option<Surface>)> {
    let fan = |len: usize, f: &(dyn Fn(usize) + Sync)| job_fan(pool, len, f);
    let surface = field.and_then(|(shape, to)| Surface::new(shape, to)).map(|s| match read_before {
        Some(w) => s.after(w),
        None => s,
    });
    if let Some((was, changed)) = was.and_then(|(was, old)| Some((was, program.changed_from(old)?))) {
        if let Ok((v, n)) = Volume::bake_again(program, Channel::Color, bounds, pitch, surface.as_ref(), was, &changed, &fan)? {
            return Some((v, Some(n.len()), surface));
        }
    }
    Some((Volume::bake(program, Channel::Color, bounds, pitch, surface.as_ref(), &fan)?, None, surface))
}

/// One field part's paint in a still picture (a review's, a variant's), baked for the picture
/// alone: the program of what the views show of it, each slot's colour in the picture's own
/// colours, and the box it bakes over and the field it bakes round, in the program's frame.
pub struct StillPaint {
    pub program: Arc<Program>,
    pub palette: Vec<[f32; 3]>,
    bounds: ([f64; 3], [f64; 3]),
    field: FieldAt,
}

impl StillPaint {
    /// `program` (`from_printer` takes printer mm into its frame) on a part whose printer box is
    /// `min_mm`..`max_mm`, its field the shape `key` placed by `to_printer` (shape to printer mm):
    /// over the box the plate view bakes the part's paint over.
    pub fn new(program: Arc<Program>, palette: Vec<[f32; 3]>, from_printer: &[[f64; 4]; 3], min_mm: [f64; 3], max_mm: [f64; 3], key: [u8; 32], to_printer: &[[f64; 4]; 3]) -> StillPaint {
        StillPaint { program, palette, bounds: bounds_through(min_mm, max_mm, from_printer), field: (key, mul(from_printer, to_printer)) }
    }
}

/// A still picture's paint of one part: taken from the plate view's very bake of it, else baking
/// on the pool's background workers. Dropping it ends its bake.
pub enum StillBake {
    Baked(Arc<Volume>),
    Baking(TaskHandle<Option<Volume>>),
}

impl StillBake {
    /// The paint of `still` on the field `shape` (the one its key names): the plate view's bake
    /// of the same program over the same box round the same field (`layer`), else a bake of its own.
    pub fn start(pool: &TaskPool, layer: Option<&PaintLayer>, still: &StillPaint, shape: Arc<fab_sdf_engine::makepad_csg_sdf::SdfShape>) -> Result<StillBake, String> {
        if let Some(vol) = layer.and_then(|l| l.baked_like(still)) {
            return Ok(StillBake::Baked(vol));
        }
        let (program, bounds, to) = (still.program.clone(), still.bounds, still.field.1);
        let workers = pool.clone();
        pool.submit_named(Class::Background, "fab still paint bake", move || bake_volume(&workers, &program, bounds, Volume::pitch(&program), Some((&shape, &to))))
            .map(StillBake::Baking)
            .map_err(|e| format!("the paint could not be baked: {e}"))
    }

    /// Its volume once baked; Err: its bake ended unfinished.
    pub fn volume(&mut self) -> Option<Result<&Arc<Volume>, String>> {
        if let StillBake::Baking(job) = self {
            match job.try_take()? {
                Ok(Some(vol)) => *self = StillBake::Baked(Arc::new(vol)),
                Ok(None) => return Some(Err("its paint's bake ended unfinished".into())),
                Err(e) => return Some(Err(format!("its paint's bake failed: {e}"))),
            }
        }
        match self {
            StillBake::Baked(vol) => Some(Ok(vol)),
            StillBake::Baking(_) => None,
        }
    }
}

impl Drop for StillBake {
    fn drop(&mut self) {
        if let StillBake::Baking(job) = self {
            job.cancel();
        }
    }
}

/// Still pictures' field paint packed alone from texel 0 of a texture `width` wide: per part its
/// object, its baked volume and its paint. The pack and each part's header texel.
pub fn pack_stills(parts: &[(&str, &Volume, &StillPaint)], width: usize) -> PaintPack {
    // colour paint, lit like the part (no floor: [`shown_floor`])
    let atlas: Vec<AtlasPart> = parts.iter().map(|(_, volume, still)| AtlasPart { volume, from: rows(&still.field.1), palette: &still.palette, floor: 0.0 }).collect();
    let atlas = fab_paint_field::Atlas::pack(&atlas, 0, width);
    PaintPack { sdf: parts.iter().map(|(object, ..)| object.to_string()).zip(atlas.headers.iter().copied()).collect(), data: atlas.data, elements: Vec::new(), coarser: String::new() }
}

/// A live edit's bricks, baked where its frame waits for them: the UI thread may not wait on the
/// pool, so these few run on threads of their own.
fn live_fan(len: usize, f: &(dyn Fn(usize) + Sync)) {
    let next = AtomicUsize::new(0);
    let work = || loop {
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= len {
            break;
        }
        f(i);
    };
    std::thread::scope(|s| {
        for _ in 1..LIVE_THREADS {
            s.spawn(work);
        }
        work();
    });
}

const LIVE_THREADS: usize = 8;

fn rows(m: &[[f64; 4]; 3]) -> [[f32; 4]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| m[r][c] as f32))
}

/// What the paint atlas packs ([`atlas_entries`]).
struct AtlasEntries<'a> {
    /// each volume once (two entries of a part share it), the part whose palette and name it packs with
    vols: Vec<(&'a Volume, usize)>,
    /// per atlas entry: its volume, its frame's rows, its part
    entries: Vec<(usize, [[f32; 4]; 3], usize)>,
    /// per atlas entry: (element, object of a field entry)
    owners: Vec<(Option<usize>, Option<String>)>,
}

/// The paint atlas's entries: a part's mesh, and its field when it has one.
fn atlas_entries<'a>(parts: &'a [PaintPart], state: &AppState) -> AtlasEntries<'a> {
    let mut vols: Vec<(&Volume, usize)> = Vec::new();
    let mut entries: Vec<(usize, [[f32; 4]; 3], usize)> = Vec::new();
    let mut owners: Vec<(Option<usize>, Option<String>)> = Vec::new();
    for (k, v) in parts.iter().enumerate() {
        let vol = match (&v.live, &v.vol) {
            (Some(l), _) => &l.vol,
            (None, Some((vol, _))) => &**vol,
            (None, None) => continue,
        };
        let from = v.view.from_printer;
        let Some(p) = state.plate.parts.iter().find(|p| p.object == v.object) else { continue };
        let at = vols.iter().position(|(w, _)| std::ptr::eq(*w, vol)).unwrap_or_else(|| {
            vols.push((vol, k));
            vols.len() - 1
        });
        entries.push((at, rows(&mul(&from, &PRINTER_FROM_RENDER)), k));
        owners.push((p.element.map(|e| e.index()), None));
        if let Some(sdf) = &p.sdf {
            entries.push((at, rows(&mul(&from, &sdf.to_printer)), k));
            owners.push((None, Some(v.object.clone())));
        }
    }
    AtlasEntries { vols, entries, owners }
}

fn mul(a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    std::array::from_fn(|r| {
        let mut o = [0.0; 4];
        for c in 0..4 {
            o[c] = (0..3).map(|k| a[r][k] * b[k][c]).sum::<f64>() + if c == 3 { a[r][3] } else { 0.0 };
        }
        o
    })
}

/// The static lanes' geometry frame (the Y-up turn of Fab's metres, `pack::to_render`) to
/// printer millimetres.
const PRINTER_FROM_RENDER: [[f64; 4]; 3] = [[1000.0, 0.0, 0.0, 0.0], [0.0, 0.0, -1000.0, 0.0], [0.0, 1000.0, 0.0, 0.0]];

impl PaintLayer {
    pub fn binding(&self) -> Option<&PaintBinding> {
        self.binding.as_ref()
    }

    /// A part's baked paint: the program of the volume the views show, its palette and its
    /// frame (printer mm to the program's).
    pub fn baked(&self, object: &str) -> Option<(&Arc<Program>, &[[f32; 3]], [[f64; 4]; 3])> {
        let v = self.parts.iter().find(|v| v.object == object)?;
        let (_, program) = v.vol.as_ref()?;
        Some((program, &v.view.colors, v.view.from_printer))
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The texture the element lookup made with the pack in it (`ctl` = width, height).
    pub fn bind(&mut self, texture: Option<Texture>, width: f32, height: f32) {
        self.binding = texture.filter(|_| !self.sdf.is_empty()).map(|texture| PaintBinding { texture, ctl: [width, height, 0.0, 1.0], sdf: self.sdf.clone() });
    }

    /// The plate view's finished bake of `still`'s very program, over its box round its field:
    /// the volume a bake of its own would make.
    fn baked_like(&self, still: &StillPaint) -> Option<Arc<Volume>> {
        self.parts.iter().find_map(|v| {
            let (vol, program) = v.vol.as_ref()?;
            (Arc::ptr_eq(program, &still.program) && v.vol_bounds == still.bounds && vol.cell == Volume::pitch(program) && v.vol_field == Some(still.field)).then(|| vol.clone())
        })
    }

    /// The plate's painted parts in step with `state`: bakes started, finished ones taken, the
    /// atlas packed again when anything changed. True while a bake is running. A drag's preview
    /// plate ([`crate::api::PlateInfo::preview`]) starts no bake and drops no volume: the paint
    /// baked last stays on show until the committed plate's bake replaces it.
    pub fn sync(&mut self, cx: &Cx, state: &AppState) -> bool {
        let plate = &state.plate.parts;
        let target = shown_target(state);
        let floor = shown_floor(state);
        if floor != self.floor {
            self.floor = floor;
            self.revision += 1;
        }
        // finished bakes first: a part whose paint moved on meanwhile bakes its newest now
        self.take_baked();
        if state.plate.preview {
            return self.parts.iter().any(|v| v.job.is_some());
        }
        let before = self.parts.len();
        self.parts.retain(|v| plate.iter().any(|p| p.object == v.object && (p.paint_view.as_ref().is_some_and(|view| view.shows(target)) || v.live.is_some())));
        if self.parts.len() != before {
            self.revision += 1;
        }
        for p in plate {
            let Some(full) = p.paint_view.as_ref().filter(|view| view.shows(target)) else { continue };
            let key = full.shown_key(target);
            let i = match self.parts.iter().position(|v| v.object == p.object) {
                Some(i) => i,
                None => {
                    let Some(view) = full.shown(target) else { continue };
                    self.parts.push(PaintPart { version: Version::new(), object: p.object.clone(), view, bounds: ([0.0; 3], [0.0; 3]), field: None, vol_bounds: ([0.0; 3], [0.0; 3]), vol_field: None, vol_surface: None, vol: None, job: None, live: None });
                    self.parts.len() - 1
                }
            };
            let v = &mut self.parts[i];
            let bounds = bounds_through(p.min_mm, p.max_mm, &full.from_printer);
            // the part's field into the paint's frame: only the bricks round its surface bake
            let field = p.sdf.as_ref().map(|sdf| (sdf.shape.clone(), (sdf.key, mul(&full.from_printer, &sdf.to_printer))));
            let field_at = field.as_ref().map(|(_, at)| *at);
            // The last bake was asked for what the volume is made of now (the paint's key, the
            // field) over a box that holds the part's: done, or on its way. Its colours and where
            // the part stands are drawn, not baked. Anything else starts a new bake, which ends
            // the one running (a size edit keeps the key, the box tells it apart).
            if (v.vol.is_some() || v.job.is_some()) && v.view.key == key && holds(v.bounds, bounds) && same_field(v.field, field_at) {
                let colors = target.tint().is_none().then_some(&full.colors).filter(|c| **c != v.view.colors);
                if v.view.from_printer != full.from_printer || colors.is_some() {
                    v.view.from_printer = full.from_printer;
                    if let Some(c) = colors {
                        v.view.colors = c.clone();
                    }
                    self.revision += 1;
                }
                continue;
            }
            // A growing pattern's newer state waits for the bake of the one before: each bake
            // shows, however fast the states come.
            if v.job.is_some() && holds(v.bounds, bounds) && full.program.has_patterns() && v.view.program.same_but_patterns(&full.program) {
                continue;
            }
            let Some(view) = full.shown(target) else { continue };
            let view = &view;
            // Only strokes moved (a stroke landed, an undo): the bricks their change reaches bake
            // again over the volume there is, if it has the requested extent. The pending
            // bake's bounds do not describe that volume until its result is installed.
            let program = view.program.clone();
            // a pattern on its way bakes on a lattice no finer than its own, so each state shows
            // soon; the one that prints bakes at the paint's own
            let pitch = match program.growing_pitch() {
                Some(grown) => std::iter::successors(Some(Volume::pitch(&program)), |c| Some(c * 2.0)).take_while(|c| *c <= grown).last().unwrap_or(Volume::pitch(&program)),
                None => Volume::pitch(&program),
            };
            let patch = v.vol.as_ref().filter(|(vol, _)| holds(v.vol_bounds, bounds) && vol.cell == pitch && same_field(v.vol_field, field_at)).and_then(|(vol, of)| of.strokes_change(&program).map(|boxes| (vol.clone(), boxes, v.vol_field, v.vol_surface.clone())));
            // Anything else (an edit of the shape, of a fill): the volume there is keeps the bricks
            // the edit cannot change, and the surface read before it what the edit cannot.
            let was = v.vol.as_ref().filter(|(vol, _)| patch.is_none() && vol.cell == pitch).cloned();
            let read_before = v.vol_surface.clone().filter(|_| patch.is_none());
            v.view = view.clone();
            // a patch is of the volume there is: its box, its field
            (v.bounds, v.field) = match patch {
                Some(_) => (v.vol_bounds, v.vol_field),
                None => (bounds, field_at),
            };
            let (tx, rx) = mpsc::channel();
            let pool = cx.task_pool();
            #[cfg(test)]
            let bake_step = self.bake_step.take();
            // A closed pool (the app ends) drops the job. Below the shape's own work, which
            // takes the workers back as it needs them.
            let end = BakeEnd(self.woke.clone());
            let _ = cx.task_pool().submit_at(Class::Visible, "fab paint bake", v.version.next(), move || {
                let _end = end;
                #[cfg(test)]
                if let Some((started, resume)) = bake_step {
                    started.send(()).unwrap();
                    makepad_widgets_core::makepad_platform::thread::without_permit(|| resume.recv().unwrap());
                }
                let t = std::time::Instant::now();
                let fan = |len: usize, f: &(dyn Fn(usize) + Sync)| job_fan(&pool, len, f);
                // a patch keeps the field its base was baked round
                let (vol, at, surface) = match patch {
                    Some((base, boxes, at, surface)) => {
                        let mut vol = (*base).clone();
                        match vol.rebake_boxes(&program, Channel::Color, &boxes, &fan) {
                            Some(true) => {}
                            Some(false) => vol.compact(),
                            None => return,
                        }
                        log!("fab vp paint: rebaked {} stroke capsules' bricks: {} bricks, {:.1} ms", boxes.len(), vol.bricks(), t.elapsed().as_secs_f64() * 1000.0);
                        (vol, at, surface)
                    }
                    None => {
                        let was = was.as_ref().map(|(vol, of)| (&**vol, &**of));
                        let Some((vol, again, surface)) = bake_volume_after(&pool, &program, bounds, pitch, field.as_ref().map(|(shape, (_, to))| (&**shape, to)), was, read_before.as_deref()) else { return };
                        let how = again.map_or(String::new(), |n| format!(" ({n} bricks the edit can change baked, the rest kept)"));
                        let read = surface.as_ref().filter(|s| s.kept() > 0).map_or(String::new(), |s| format!(", {} bricks read as before", s.kept()));
                        log!("fab vp paint: baked {} layers: cell {} mm, {} bricks, {:.1} MB, {:.0} ms{how}{read}", program.layers.len(), vol.cell, vol.bricks(), vol.bytes() as f64 / 1e6, t.elapsed().as_secs_f64() * 1000.0);
                        (vol, field.map(|(_, at)| at), surface.map(Arc::new))
                    }
                };
                let _ = tx.send((vol, program, at, surface));
            });
            v.job = Some(rx);
        }
        self.parts.iter().any(|v| v.job.is_some())
    }

    /// A bake ended since the last ask: the view records again to take it ([`PaintLayer::sync`]).
    /// A signal comes with it.
    pub fn take_woke(&self) -> bool {
        self.woke.swap(false, Ordering::AcqRel)
    }

    /// The bakes that finished, installed.
    fn take_baked(&mut self) {
        for v in &mut self.parts {
            if let Some(rx) = &v.job {
                match rx.try_recv() {
                    Ok((vol, program, at, surface)) => {
                        v.vol = Some((Arc::new(vol), program));
                        v.vol_bounds = v.bounds;
                        v.vol_field = at;
                        v.vol_surface = surface;
                        v.job = None;
                        v.live = None;
                        self.revision += 1;
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => v.job = None,
                }
            }
        }
    }

    /// What is being painted now, shown at once by baking only the bricks it reaches: the stroke
    /// being drawn, or what a fill click would paint. `None` drops it (kept while the edit's own
    /// bake runs, so the old paint never flashes).
    pub fn set_live(&mut self, state: &AppState, live: Option<LivePaint>) {
        let mut changed = false;
        let object = live.as_ref().map(|l| l.object().to_string());
        // Painting an unpainted part: an empty volume in its model frame to draw on.
        if let Some(object) = &object {
            if !self.parts.iter().any(|v| v.object == *object) {
                if let Some(p) = state.plate.parts.iter().find(|p| p.object == *object) {
                    let empty = Arc::new(Program::default());
                    let bounds = bounds_through(p.min_mm, p.max_mm, &p.model_from_printer);
                    let vol = Volume::bake(&empty, Channel::Color, bounds, Volume::pitch(&empty), None, &live_fan).expect("the live fan skips nothing");
                    let view = PaintView { program: empty.clone(), colors: vec![[0.0; 3]], from_printer: p.model_from_printer, key: 0 };
                    self.parts.push(PaintPart { version: Version::new(), object: object.clone(), view, bounds, field: None, vol_bounds: bounds, vol_field: None, vol_surface: None, vol: Some((Arc::new(vol), empty)), job: None, live: None });
                }
            }
        }
        for k in 0..self.parts.len() {
            let Some(l) = live.as_ref().filter(|l| l.object() == self.parts[k].object) else {
                if self.parts[k].job.is_none() {
                    changed |= self.parts[k].live.take().is_some();
                }
                continue;
            };
            let (slot, palette) = self.slot_for(&self.parts[k].object, l.rgb());
            let v = &mut self.parts[k];
            let Some((base, of)) = v.vol.as_ref() else { continue };
            let (base, of, f) = (base.clone(), of.clone(), v.view.from_printer);
            match l {
                LivePaint::Stroke { points, radius, erase, rgb, .. } => {
                    let model: Vec<[f64; 3]> = points.iter().map(|p| std::array::from_fn(|r| f[r][0] * p[0] + f[r][1] * p[1] + f[r][2] * p[2] + f[r][3])).collect();
                    let scale = (0..3).map(|k| f[k][0] * f[k][0]).sum::<f64>().sqrt();
                    let r = radius * scale;
                    let key = LiveKey::Stroke(model.clone(), r, slot, *erase);
                    if v.live.as_ref().is_some_and(|x| x.key == key) {
                        continue;
                    }
                    let t = std::time::Instant::now();
                    // the points added since the last move join the stroke being drawn
                    let more = match &v.live {
                        Some(Live { key: LiveKey::Stroke(pts, lr, ls, le), stroke: Some(_), .. }) if *lr == r && *ls == slot && *le == *erase && model.starts_with(pts) => Some(pts.len()),
                        _ => None,
                    };
                    let (boxes, live) = match more {
                        Some(n) => {
                            let mut live = v.live.take().unwrap();
                            let (program, file) = live.stroke.as_mut().unwrap();
                            let log = program.strokes_mut(file).expect("the live strokes layer");
                            (Arc::make_mut(log).extend(&model[n..]), live)
                        }
                        None => {
                            // the part's strokes file (a part's first stroke: one on top)
                            let mut program = (*of).clone();
                            let found = program.strokes_files().next().map(|(file, _)| file.to_string());
                            let file = match found {
                                Some(file) => file,
                                None => {
                                    let file = "live.strokes".to_string();
                                    program = program.with_strokes_on_top(&file, Arc::new(Strokes::new()), Arc::new(Vec::new()));
                                    file
                                }
                            };
                            let log = Arc::make_mut(program.strokes_mut(&file).expect("the strokes layer"));
                            let op = if *erase { StrokeOp::Erase } else { StrokeOp::Paint { slot: log.slot_for(StrokeSwatch { rgb: *rgb, filament: None }) } };
                            let swatches = log.palette.len();
                            let boxes = log.push(op, r, 1.0, &model);
                            if let StrokeOp::Paint { slot: file_slot } = op {
                                let slots = Arc::make_mut(program.strokes_slots_mut(&file).expect("the strokes layer"));
                                slots.resize(swatches, 0);
                                slots[file_slot as usize] = slot;
                            }
                            (boxes, Live { key: key.clone(), palette, vol: (*base).clone(), stroke: Some((program, file)) })
                        }
                    };
                    let mut live = live;
                    let (program, _) = live.stroke.as_ref().unwrap();
                    if live.vol.rebake_boxes(program, Channel::Color, &boxes, &live_fan) == Some(false) {
                        live.vol.compact();
                    }
                    if std::env::var_os("FAB_PAINT_TIMES").is_some() {
                        log!("fab vp paint: live dab {} capsules, {:.2} ms", boxes.len(), t.elapsed().as_secs_f64() * 1000.0);
                    }
                    live.key = key;
                    v.live = Some(live);
                    changed = true;
                }
                LivePaint::Fill { regions, names, .. } => {
                    let key = LiveKey::Fill(names.clone(), slot);
                    if v.live.as_ref().is_some_and(|x| x.key == key) {
                        continue;
                    }
                    let op = Op::Paint { channel: Channel::Color, slot };
                    let span = regions.iter().try_fold(None::<([f64; 3], [f64; 3])>, |acc, r| {
                        let (lo, hi) = match r {
                            Region::Field { node, grow } => {
                                let (lo, hi) = fab_paint_field::bounds_of(node)?;
                                (lo.map(|x| x - grow), hi.map(|x| x + grow))
                            }
                            Region::Operand(of) => of.iter().try_fold(None::<([f64; 3], [f64; 3])>, |acc, o| {
                                let (lo, hi) = o.bounds?;
                                Some(Some(match acc { None => (lo, hi), Some((a, b)) => (std::array::from_fn(|k| a[k].min(lo[k])), std::array::from_fn(|k| b[k].max(hi[k]))) }))
                            })??,
                            _ => return None,
                        };
                        Some(Some(match acc { None => (lo, hi), Some((a, b)) => (std::array::from_fn(|k| a[k].min(lo[k])), std::array::from_fn(|k| b[k].max(hi[k]))) }))
                    }).flatten();
                    let program = of.with_layers_on_top(regions.iter().map(|r| Layer { region: r.clone(), op: op.clone() }));
                    let mut vol = (*base).clone();
                    let whole = (v.bounds.0, v.bounds.1);
                    if vol.rebake(&program, Channel::Color, span.unwrap_or(whole), &live_fan) == Some(false) {
                        vol = (*base).clone();
                        vol.rebake(&program, Channel::Color, whole, &live_fan);
                    }
                    v.live = Some(Live { key, palette, vol, stroke: None });
                    changed = true;
                }
            }
        }
        if changed {
            self.revision += 1;
        }
    }

    /// The colour slot a stroke in `rgb` paints `object` in: its program's, or the next one.
    pub fn slot_for(&self, object: &str, rgb: [f32; 3]) -> (u8, Vec<[f32; 3]>) {
        let Some(v) = self.parts.iter().find(|v| v.object == object) else { return (1, vec![[0.0; 3], rgb]) };
        let mut colors = v.view.colors.clone();
        match colors.iter().skip(1).position(|c| (0..3).all(|k| (c[k] - rgb[k]).abs() < 1e-4)) {
            Some(i) => (i as u8 + 1, colors),
            None => {
                colors.push(rgb);
                ((colors.len() - 1) as u8, colors)
            }
        }
    }

    /// Each part's palette as it packs: its live stroke's, else its view's.
    fn palettes(&self) -> Vec<Vec<[f32; 3]>> {
        self.parts.iter().map(|v| v.live.as_ref().map_or_else(|| v.view.colors.clone(), |l| l.palette.clone())).collect()
    }

    /// The texels [`PaintLayer::pack_into`] packs at `width` with no `room`, without packing
    /// (the Force overlay's values are not made): the element lookup sizes its texture by it, so
    /// it packs once.
    pub fn pack_texels(&self, state: &AppState, width: usize) -> usize {
        let palettes = self.palettes();
        let AtlasEntries { vols, entries, .. } = atlas_entries(&self.parts, state);
        let parts: Vec<AtlasPart> = entries.iter().map(|&(i, from, k)| AtlasPart { volume: vols[i].0, from, palette: &palettes[k], floor: self.floor }).collect();
        let mut texels = fab_paint_field::Atlas::pack_texels(&parts, width);
        if let Some((index, values, marks, palette)) = super::force::Overlaid::sizes(state) {
            if let Some(p) = state.plate.parts.get(index) {
                // the overlay's frames: the mesh lanes', and its field shade's
                texels += fab_paint_field::Atlas::overlay_texels(width, 1 + p.sdf.is_some() as usize, palette, values, marks);
            }
        }
        texels
    }

    /// Every volume packed from texel `base` of a texture `width` wide, for the element lookup.
    /// `room`: the texels the atlas may take there when the device's largest texture holds no
    /// more. Past it the largest of the paint volumes and the Force overlay is made coarser one
    /// halving at a time ([`Volume::coarser`], [`fab_paint_field::bake::OverlayLattice::coarser_nodes`]),
    /// as few as fit; [`PaintPack::coarser`] says which. Without `room` nothing is.
    pub fn pack_into(&mut self, state: &AppState, base: usize, width: usize, room: Option<usize>) -> PaintPack {
        self.pack_onto(state, Vec::new(), base, width, room)
    }

    /// [`PaintLayer::pack_into`] after what `data` holds, whole rows of `width` from texel `base`
    /// ([`fab_paint_field::Atlas::pack_onto`]): the element lookup packs onto its table's rows,
    /// so its texture's data is written once. The pack's data is `data` with the atlas after.
    pub fn pack_onto(&mut self, state: &AppState, data: Vec<f32>, base: usize, width: usize, room: Option<usize>) -> PaintPack {
        let palettes = self.palettes();
        let floor = self.floor;
        let AtlasEntries { vols, entries, owners } = atlas_entries(&self.parts, state);
        let study = super::force::Overlaid::of(state).filter(|s| state.plate.parts.get(s.index).is_some());
        // Coarser, while what is packed would not fit `room`: the largest that still shrinks
        // goes one halving coarser. `coarse[i]`: volume i's coarser lattices, `overlay` the
        // overlay's (values, marks), `done` those that shrink no more.
        let mut coarse: Vec<Vec<Volume>> = vec![Vec::new(); vols.len()];
        let mut overlay: Vec<(Option<fab_paint_field::bake::OverlayGrid>, Option<fab_paint_field::bake::OverlayGrid>)> = Vec::new();
        if let Some(room) = room {
            let vol_texels = |v: &Volume, k: usize| v.index.len().div_ceil(4) + palettes[k].len().max(1) + v.samples.len() / 4;
            let lattice_texels = |l: Option<&fab_paint_field::bake::OverlayLattice>| l.map_or(0, |l| l.data.len().div_ceil(4));
            // each part's header, and the pack's and the overlay's last rows a part full
            let fixed = 6 * entries.len() + 2 * width + study.as_ref().map_or(0, |s| 2 * 14 + s.overlay().palette.len());
            let mut done = vec![false; vols.len() + 1];
            loop {
                let sizes: Vec<usize> = (0..vols.len()).map(|i| vol_texels(coarse[i].last().unwrap_or(vols[i].0), vols[i].1)).collect();
                let shown = study.as_ref().map(|s| s.overlay());
                let over = match (overlay.last(), &shown) {
                    (Some((v, m)), _) => lattice_texels(v.as_ref().map(|g| g.lattice()).as_ref()) + lattice_texels(m.as_ref().map(|g| g.lattice()).as_ref()),
                    (None, Some(o)) => lattice_texels(o.values.as_ref()) + lattice_texels(o.marks.as_ref()),
                    (None, None) => 0,
                };
                if fixed + sizes.iter().sum::<usize>() + over <= room {
                    break;
                }
                let largest = sizes.iter().copied().chain([over]).enumerate().filter(|&(i, _)| !done[i]).max_by_key(|&(_, t)| t);
                let Some((i, was)) = largest else { break };
                if i < vols.len() {
                    let next = coarse[i].last().unwrap_or(vols[i].0).coarser();
                    done[i] = vol_texels(&next, vols[i].1) >= was;
                    if !done[i] {
                        coarse[i].push(next);
                    }
                } else if let Some(o) = &shown {
                    let (v, m) = match overlay.last() {
                        Some((v, m)) => (v.as_ref().map(|g| g.lattice().coarser_nodes()), m.as_ref().map(|g| g.lattice().coarser_cells())),
                        None => (o.values.as_ref().map(|l| l.coarser_nodes()), o.marks.as_ref().map(|l| l.coarser_cells())),
                    };
                    let next = lattice_texels(v.as_ref().map(|g| g.lattice()).as_ref()) + lattice_texels(m.as_ref().map(|g| g.lattice()).as_ref());
                    done[i] = next >= was;
                    if !done[i] {
                        overlay.push((v, m));
                    }
                } else {
                    done[i] = true;
                }
            }
        }
        let coarser = {
            let mut said: Vec<String> = Vec::new();
            if !overlay.is_empty() {
                said.push(format!("the Force overlay at {}x its cells", 1 << overlay.len()));
            }
            for (i, c) in coarse.iter().enumerate().filter(|(_, c)| !c.is_empty()) {
                said.push(format!("{}'s paint at {}x its cells", self.parts[vols[i].1].object, 1 << c.len()));
            }
            said.join(", ")
        };
        let parts: Vec<AtlasPart> = entries.iter().map(|&(i, from, k)| AtlasPart { volume: coarse[i].last().unwrap_or(vols[i].0), from, palette: &palettes[k], floor }).collect();
        let mut atlas = fab_paint_field::Atlas::pack_onto(data, &parts, base, width);
        let mut elements = Vec::new();
        let mut sdf = Vec::new();
        for ((e, o), h) in owners.into_iter().zip(&atlas.headers) {
            if let Some(e) = e {
                elements.push((e, *h));
            }
            if let Some(o) = o {
                sdf.push((o, *h));
            }
        }
        if let Some(study) = &study {
            if let Some(p) = state.plate.parts.get(study.index) {
                const SAME: [[f32; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
                let paint = self.parts.iter().find(|v| v.object == p.object && (v.live.is_some() || v.vol.is_some())).map(|v| v.view.from_printer);
                let element = p.element.map(|e| e.index());
                // the mesh lanes hand it their geometry's frame through its rows: printer mm
                let mut frames = vec![OverlayFrame {
                    under: elements.iter().find(|(e, _)| Some(*e) == element).map(|(_, h)| *h),
                    lane: rows(&PRINTER_FROM_RENDER),
                    lattice: SAME,
                    paint: paint.map_or(SAME, |from| rows(&from)),
                }];
                // the field shade hands it its object frame, the frame its paint volume is in
                if let Some(field) = &p.sdf {
                    frames.push(OverlayFrame { under: sdf.iter().find(|(o, _)| *o == p.object).map(|(_, h)| *h), lane: SAME, lattice: rows(&field.to_printer), paint: SAME });
                }
                let shown = match overlay.last() {
                    Some((v, m)) => fab_paint_field::bake::Overlay { values: v.as_ref().map(|g| g.lattice()), marks: m.as_ref().map(|g| g.lattice()), palette: study.overlay().palette },
                    None => study.overlay(),
                };
                let headers = atlas.overlay(base, width, &shown, &frames);
                if let Some(e) = element {
                    elements.retain(|(x, _)| *x != e);
                    elements.push((e, headers[0]));
                }
                if p.sdf.is_some() {
                    sdf.retain(|(o, _)| *o != p.object);
                    sdf.push((p.object.clone(), headers[1]));
                }
            }
        }
        self.sdf = sdf.clone();
        PaintPack { data: atlas.data, elements, sdf, coarser }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_paint_field::bake::texel_at;

    /// Wait at the result channel, then let sync install the result by its normal path.
    fn finish(layer: &mut PaintLayer, cx: &Cx, state: &AppState) {
        if let Some(rx) = layer.parts[0].job.take() {
            let result = rx.recv().expect("the bake completes");
            let (tx, rx) = mpsc::channel();
            tx.send(result).unwrap();
            layer.parts[0].job = Some(rx);
        }
        assert!(!layer.sync(cx, state), "the completed bake is shown");
    }

    /// A volume's every lattice sample, a whole brick's each its slot at the ramp's end: two
    /// volumes alike read alike, however their sample bricks are ordered.
    fn read_through(v: &Volume) -> (f64, [i32; 3], [i32; 3], Vec<f32>) {
        let all = v.index.iter().flat_map(|&e| match e < 0.0 {
            true => vec![fab_paint_field::bake::sample_value((-1.0 - e) as u8, 1.0); 512],
            false => v.samples[e as usize * 512..e as usize * 512 + 512].to_vec(),
        });
        (v.cell, v.min, v.dim, all.collect())
    }

    /// A drag's preview plates start no bake and leave the paint on show as it was (the same
    /// atlas, no frame without it); the committed plate bakes, and its volume reads as a bake
    /// of the committed paint from scratch.
    #[test]
    fn a_drag_preview_keeps_the_paint_on_show_and_the_commit_bakes_it_afresh() {
        use crate::api::{ElementId, PlatePart};
        const SAME: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let cx = Cx::new(Box::new(|_, _| {}));
        let mut strokes = Strokes::new();
        let slot = strokes.slot_for(StrokeSwatch { rgb: [1.0, 0.0, 0.0], filament: None });
        strokes.push(StrokeOp::Paint { slot }, 1.5, 1.0, &[[-6.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        let painted = |strokes: &Strokes| Arc::new(Program::new(vec![Layer { region: Region::Strokes { file: "part.strokes".into(), strokes: Arc::new(strokes.clone()), slots: Arc::new(vec![1]) }, op: Op::Paint { channel: Channel::Color, slot: 1 } }]));
        let mut state = AppState::default();
        state.plate.parts.push(PlatePart {
            object: "part".into(), min_mm: [-8.0; 3], max_mm: [8.0; 3], model_from_printer: SAME,
            element: Some(ElementId(0)),
            paint_view: Some(PaintView { program: painted(&strokes), colors: vec![[0.0; 3], [1.0, 0.0, 0.0]], from_printer: SAME, key: 1 }),
            ..Default::default()
        });
        let mut layer = PaintLayer::default();
        assert!(layer.sync(&cx, &state), "the first plate bakes");
        finish(&mut layer, &cx, &state);
        let shown = layer.pack_into(&state, 0, 256, None).data;
        let revision = layer.revision();
        // the drag: a stroke drawn out step by step, each step a preview plate
        state.plate.preview = true;
        for k in 0..3 {
            let mut dragged = strokes.clone();
            dragged.push(StrokeOp::Paint { slot }, 1.5, 1.0, &[[0.0, 3.0, 0.0], [3.0 + k as f64, 3.0, 0.0]]);
            let view = state.plate.parts[0].paint_view.as_mut().unwrap();
            (view.program, view.key) = (painted(&dragged), 2 + k);
            assert!(!layer.sync(&cx, &state) && layer.parts[0].job.is_none(), "preview {k} started a bake");
            assert_eq!(layer.revision(), revision, "preview {k} packed the atlas again");
            assert!(layer.pack_into(&state, 0, 256, None).data == shown, "preview {k}: the paint shown before the drag is not on show");
        }
        // the commit: the last preview's paint
        state.plate.preview = false;
        assert!(layer.sync(&cx, &state), "the committed plate bakes");
        finish(&mut layer, &cx, &state);
        let program = state.plate.parts[0].paint_view.as_ref().unwrap().program.clone();
        let one_by_one = |len: usize, f: &(dyn Fn(usize) + Sync)| (0..len).for_each(f);
        let fresh = Volume::bake(&program, Channel::Color, bounds_through([-8.0; 3], [8.0; 3], &SAME), Volume::pitch(&program), None, &one_by_one).unwrap();
        let (baked, _) = layer.parts[0].vol.as_ref().expect("the commit's volume");
        assert!(read_through(baked) == read_through(&fresh), "the commit's volume is not the committed paint's bake");
        assert!(layer.pack_into(&state, 0, 256, None).data != shown, "the committed stroke shows");
    }

    /// Mechanics moves the parts every frame: a pose alone writes the element lookup's table again
    /// and leaves the paint atlas under it as it was packed (only the table's rows go up), the
    /// painted part still finding its volume; a new colour packs the atlas again.
    #[test]
    fn a_pose_alone_writes_the_lookups_table_and_leaves_the_paint_atlas_be() {
        use super::super::elements::{ElementLut, LutSync, LUT_WIDTH, STRIDE};
        use crate::api::{ElementId, PlatePart};
        const SAME: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut strokes = Strokes::new();
        let slot = strokes.slot_for(StrokeSwatch { rgb: [1.0, 0.0, 0.0], filament: None });
        strokes.push(StrokeOp::Paint { slot }, 1.5, 1.0, &[[-6.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        let program = Arc::new(Program::new(vec![Layer { region: Region::Strokes { file: "part.strokes".into(), strokes: Arc::new(strokes), slots: Arc::new(vec![1]) }, op: Op::Paint { channel: Channel::Color, slot: 1 } }]));
        let mut state = AppState::default();
        state.scene = Arc::new(crate::model::Scene::from_model(crate::model::fixture::parts(), &mut |_| {}));
        let part = ElementId::from_index(0);
        state.plate.parts.push(PlatePart {
            object: "part".into(), min_mm: [-8.0; 3], max_mm: [8.0; 3], model_from_printer: SAME,
            element: Some(part),
            paint_view: Some(PaintView { program, colors: vec![[0.0; 3], [1.0, 0.0, 0.0]], from_printer: SAME, key: 1 }),
            ..Default::default()
        });
        let mut layer = PaintLayer::default();
        assert!(layer.sync(&cx, &state), "the first plate bakes");
        finish(&mut layer, &cx, &state);
        let mut lut = ElementLut::default();
        let read = |lut: &ElementLut, cx: &mut Cx| {
            let t = lut.texture().expect("the lookup's texture");
            let data = t.take_vec_f32(cx);
            t.put_back_vec_f32(cx, data.clone(), None);
            data
        };
        assert_eq!(lut.sync(&mut cx, &state, crate::api::PaneId::PRIMARY, &[], 0, &mut layer), LutSync::All, "the first sync packs the atlas");
        let table = state.scene.elements.len() * STRIDE;
        let table = table.div_ceil(LUT_WIDTH) * LUT_WIDTH * 4;
        let packed = read(&lut, &mut cx);
        assert!(packed.len() > table, "the atlas follows the table");
        // the part's texel 1 .w: its volume's header, as the atlas stores texel numbers
        let header = (part.index() * STRIDE + 1) * 4 + 3;
        assert!(packed[header] > 0.0, "the painted part names its volume");
        // a pose: lifted 1 cm (Fab's Z up is render Y)
        let mut moves = std::collections::HashMap::new();
        moves.insert(part, [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.01f32]]);
        state.scene_state.set_moves(Some(Arc::new(moves)));
        assert_eq!(lut.sync(&mut cx, &state, crate::api::PaneId::PRIMARY, &[], 0, &mut layer), LutSync::Table, "a pose packed the atlas again");
        let posed = read(&lut, &mut cx);
        let at = part.index() * STRIDE * 4;
        assert_eq!(posed[at + 1..at + 4], [0.0, 0.01, 0.0], "the table holds the pose");
        assert_eq!(posed[header], packed[header], "the painted part lost its volume");
        assert!(posed[table..] == packed[table..], "the atlas under the table changed");
        assert_eq!(lut.sync(&mut cx, &state, crate::api::PaneId::PRIMARY, &[], 0, &mut layer), LutSync::Kept, "nothing changed");
        // slot 1 in another colour: the atlas is packed again, with it
        state.plate.parts[0].paint_view.as_mut().unwrap().colors[1] = [0.0, 0.2, 1.0];
        assert!(!layer.sync(&cx, &state), "a new palette baked");
        assert_eq!(lut.sync(&mut cx, &state, crate::api::PaneId::PRIMARY, &[], 0, &mut layer), LutSync::All, "a new colour kept the old atlas");
        let repacked = read(&lut, &mut cx);
        assert!(repacked[table..] != packed[table..], "the atlas holds the new colour");
        assert_eq!(repacked[at + 1..at + 4], [0.0, 0.01, 0.0], "the pose stays");
    }

    /// The paint's volume is baked of what it is made of alone: a new palette draws it in its
    /// new colours, and a move over the bed with a quarter turn draws it where the part now
    /// stands, neither baking: the volume stays the very one, byte for byte.
    #[test]
    fn a_new_palette_and_a_move_draw_the_paint_anew_and_bake_nothing() {
        use crate::api::{ElementId, PlatePart};
        const SAME: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let cx = Cx::new(Box::new(|_, _| {}));
        let mut strokes = Strokes::new();
        let slot = strokes.slot_for(StrokeSwatch { rgb: [1.0, 0.0, 0.0], filament: None });
        strokes.push(StrokeOp::Paint { slot }, 1.5, 1.0, &[[-6.0, 0.0, 0.0], [0.0, 0.0, 0.0]]);
        let program = Arc::new(Program::new(vec![Layer { region: Region::Strokes { file: "part.strokes".into(), strokes: Arc::new(strokes), slots: Arc::new(vec![1]) }, op: Op::Paint { channel: Channel::Color, slot: 1 } }]));
        let mut state = AppState::default();
        state.plate.parts.push(PlatePart {
            object: "part".into(), min_mm: [-8.0; 3], max_mm: [8.0; 3], model_from_printer: SAME,
            element: Some(ElementId(0)),
            paint_view: Some(PaintView { program, colors: vec![[0.0; 3], [1.0, 0.0, 0.0]], from_printer: SAME, key: 1 }),
            ..Default::default()
        });
        let mut layer = PaintLayer::default();
        assert!(layer.sync(&cx, &state), "the first plate bakes");
        finish(&mut layer, &cx, &state);
        let baked = layer.parts[0].vol.as_ref().unwrap().0.clone();
        let bytes = read_through(&baked);
        let header = |pack: &PaintPack| texel_at(pack.elements[0].1).unwrap() * 4;
        // slot 1 in another colour
        state.plate.parts[0].paint_view.as_mut().unwrap().colors[1] = [0.0, 0.2, 1.0];
        let revision = layer.revision();
        assert!(!layer.sync(&cx, &state) && layer.parts[0].job.is_none(), "a new palette baked");
        assert!(layer.revision() > revision, "a new palette packs the atlas again");
        let pack = layer.pack_into(&state, 0, 256, None);
        assert_eq!(layer.pack_texels(&state, 256), pack.data.len() / 4, "the size the element lookup sizes its texture by");
        let palette = texel_at(pack.data[header(&pack) + 10]).unwrap() * 4;
        assert_eq!(pack.data[palette + 4..palette + 7], [0.0, 0.2, 1.0], "slot 1 is drawn in its new colour");
        // 40 mm across and 25 back over the bed, a quarter turn about up: model x is printer y
        // less 25, model y is 40 less printer x
        let (t, from) = ([40.0, 25.0], [[0.0, 1.0, 0.0, -25.0], [-1.0, 0.0, 0.0, 40.0], [0.0, 0.0, 1.0, 0.0]]);
        let part = &mut state.plate.parts[0];
        (part.min_mm, part.max_mm, part.model_from_printer) = ([t[0] - 8.0, t[1] - 8.0, -8.0], [t[0] + 8.0, t[1] + 8.0, 8.0], from);
        part.paint_view.as_mut().unwrap().from_printer = from;
        let revision = layer.revision();
        assert!(!layer.sync(&cx, &state) && layer.parts[0].job.is_none(), "a move baked");
        assert!(layer.revision() > revision, "a move packs the atlas again");
        let (now, _) = layer.parts[0].vol.as_ref().unwrap();
        assert!(Arc::ptr_eq(now, &baked) && read_through(now) == bytes, "the volume is not the one baked before");
        let pack = layer.pack_into(&state, 0, 256, None);
        let h = header(&pack);
        let drawn = rows(&mul(&from, &PRINTER_FROM_RENDER)).concat();
        assert_eq!(pack.data[h + 12..h + 24], drawn[..], "the paint is drawn where the part now stands");
    }

    /// Hold the resize job before its bricks run, then land a stroke across the old extent.
    /// The replacement job must paint the grown part, including both support views.
    #[test]
    fn a_stroke_during_a_resize_bake_reaches_the_whole_shown_volume() {
        use crate::api::{ElementId, GizmoMode, PlatePart, Tool, Workspace};

        // Read a lattice sample from the atlas the renderer is given (outside reads slot 0).
        fn slot(pack: &PaintPack, p: [f64; 3]) -> u8 {
            let h = texel_at(pack.elements[0].1).unwrap() * 4;
            let d = &pack.data;
            let cell = d[h] as f64;
            let min = [d[h + 2] as i32, d[h + 3] as i32, d[h + 4] as i32];
            let dim = [d[h + 5] as i32, d[h + 6] as i32, d[h + 7] as i32];
            let g = p.map(|v| (v / cell).round() as i32);
            let b: [i32; 3] = std::array::from_fn(|k| g[k].div_euclid(8) - min[k]);
            if (0..3).any(|k| b[k] < 0 || b[k] >= dim[k]) {
                return 0;
            }
            let i = ((b[2] * dim[1] + b[1]) * dim[0] + b[0]) as usize;
            let brick = d[texel_at(d[h + 8]).unwrap() * 4 + i];
            if brick < 0.0 {
                return (-1.0 - brick) as u8;
            }
            let j = (g[0].rem_euclid(8) + 8 * g[1].rem_euclid(8) + 64 * g[2].rem_euclid(8)) as usize;
            fab_paint_field::bake::sample_parts(d[texel_at(d[h + 9]).unwrap() * 4 + brick as usize * 512 + j]).0
        }

        let cx = Cx::new(Box::new(|_, _| {}));
        const SAME: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        for target in [PaintTarget::Color, PaintTarget::SupportBlock, PaintTarget::SupportEnforce] {
            let mut state = AppState::default();
            (state.ui.gizmo, state.tool) = (GizmoMode::Paint, Tool::Select);
            state.ui.enter(Workspace::Design);
            state.ui.paint_target = target;
            let rgb = target.tint().unwrap_or([1.0, 0.0, 0.0]);
            let mut strokes = Strokes::new();
            let file_slot = strokes.slot_for(StrokeSwatch { rgb, filament: None });
            let program = Program::new(vec![Layer {
                region: Region::Strokes { file: "part.strokes".into(), strokes: Arc::new(strokes), slots: Arc::new(vec![1]) },
                op: Op::Paint { channel: target.channel(), slot: 1 },
            }]);
            state.plate.parts.push(PlatePart {
                object: "part".into(), min_mm: [-2.0; 3], max_mm: [2.0; 3], model_from_printer: SAME,
                element: Some(ElementId(0)),
                paint_view: Some(PaintView { program: Arc::new(program), colors: vec![[0.0; 3], rgb], from_printer: SAME, key: 1 }),
                ..Default::default()
            });
            let mut layer = PaintLayer::default();
            layer.sync(&cx, &state);
            finish(&mut layer, &cx, &state);

            let (started, began) = mpsc::channel();
            let (resume, held) = mpsc::channel();
            layer.bake_step = Some((started, held));
            state.plate.parts[0].max_mm[0] = 12.0;
            assert!(layer.sync(&cx, &state));
            began.recv().expect("the resize bake is held before its bricks run");

            let view = state.plate.parts[0].paint_view.as_mut().unwrap();
            let program = Arc::make_mut(&mut view.program);
            Arc::make_mut(program.strokes_mut("part.strokes").unwrap()).push(StrokeOp::Paint { slot: file_slot }, 2.0, 1.0, &[[0.0; 3], [9.6, 0.0, 0.0]]);
            view.key += 1;
            let old_job = layer.parts[0].job.take().unwrap();
            // Keep the pending job visible to sync while retaining its result channel to join it.
            let (_pending, rx) = mpsc::channel();
            layer.parts[0].job = Some(rx);
            layer.sync(&cx, &state);
            resume.send(()).unwrap();
            let _ = old_job.recv(); // superseded: its ticket stops the bake
            finish(&mut layer, &cx, &state);

            let pack = layer.pack_into(&state, 0, 256, None);
            for x in 0..=24 {
                for y in [-0.8, 0.0, 0.8] {
                    let p = [x as f64 * 0.4, y, 0.0];
                    assert_eq!(slot(&pack, p), 1, "{target:?}: the shown stroke is missing at {p:?}");
                }
            }
            assert_eq!(slot(&pack, [0.0, 4.0, 0.0]), 0, "{target:?}: outside the stroke stays bare");
        }
    }

    /// A part edited again while it bakes must not keep the workers on paint no one will see.
    #[test]
    fn a_newer_bake_ends_the_older() {
        let cx = Cx::new(Box::new(|_, _| {}));
        let pool = cx.task_pool();
        let version = Version::new();
        let ticket = version.next();
        let newer = version.clone();
        let worker_pool = pool.clone();
        let mut job = pool
            .submit_at(Class::Visible, "test bake", ticket, move || {
                newer.bump();
                let program = Program::default();
                let fan = |len: usize, f: &(dyn Fn(usize) + Sync)| job_fan(&worker_pool, len, f);
                Volume::bake(&program, Channel::Color, ([0.0; 3], [16.0; 3]), Volume::pitch(&program), None, &fan).is_some()
            })
            .expect("the pool is open");
        let baked = loop {
            match job.try_take() {
                Some(result) => break result.expect("the job ran"),
                None => std::thread::yield_now(),
            }
        };
        assert!(!baked, "a bake with a newer one behind it finished");
    }

    /// A support tint shown by the paint tool keeps its floor on the display through the
    /// composite's bias on the exposure (a studio's in Realtime), the atlas packing again when
    /// that bias changes; colour paint, and paint the tool is not showing, keep their lit look.
    #[test]
    fn a_support_tint_is_floored_on_the_display_and_colour_paint_is_not() {
        use crate::api::{GizmoMode, Lighting, Shading, Tool, Workspace};
        let cx = Cx::new(Box::new(|_, _| {}));
        let mut state = AppState::default();
        (state.ui.gizmo, state.tool) = (GizmoMode::Paint, Tool::Select);
        state.ui.enter(Workspace::Design);
        (state.view.lighting, state.view.shading) = (Lighting::Studio, Shading::Realtime);
        let shown = |state: &AppState| shown_floor(state) * super::super::display_tone(&state.view, state.view.lighting_for(&state.ui)).0;
        state.ui.paint_target = PaintTarget::Color;
        assert_eq!(shown_floor(&state), 0.0);
        let mut layer = PaintLayer::default();
        for target in [PaintTarget::SupportBlock, PaintTarget::SupportEnforce] {
            state.ui.paint_target = target;
            assert!((shown(&state) - SUPPORT_TINT_FLOOR).abs() < 1e-6, "{target:?} in a studio: {}", shown(&state));
        }
        let before = layer.revision();
        layer.sync(&cx, &state);
        assert!(layer.revision() > before, "the studio's floor packs the atlas again");
        let packed = layer.revision();
        layer.sync(&cx, &state);
        assert_eq!(layer.revision(), packed, "an unchanged floor leaves the atlas be");
        state.view.shading = Shading::Solid;
        assert!((shown(&state) - SUPPORT_TINT_FLOOR).abs() < 1e-6, "in Solid: {}", shown(&state));
        layer.sync(&cx, &state);
        assert!(layer.revision() > packed, "a new bias packs the atlas again");
        state.ui.gizmo = GizmoMode::Move;
        assert_eq!(shown_floor(&state), 0.0, "paint the tool is not showing is lit as it is");
    }
}
