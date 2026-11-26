//! `Draw2D{draw: fn(c, t, edit) {…}}`: an immediate-mode 2D layer. The draw
//! function runs every frame (a pure function of time) and records into `c`:
//! HDR capsule segments (a line batch: thousands a frame, one draw call per
//! run of the same blend), text, filled polygons and stroked polylines under
//! a Canvas2D-style transform stack. What a scene that computes its picture
//! imperatively each frame needs (per-segment colour and heat, dashes in
//! pixels, particles, typed-on strings, per-glyph transforms and clips)
//! without a layer per shape.
//!
//! The layer's box is `size` (the document's size when none); coordinates
//! are its units, y down. `t` is the layer's clock, `edit` the host time.
//!
//! ```text
//! c.seg(ax, ay, bx, by, width, rgb, alpha)   capsule in layer units, NOT transformed; rgb: [r g b] / vec3
//!                                            linear light (over 1 glows), or a colour (#hex, emit())
//! c.seg_rgb(ax, ay, bx, by, width, r, g, b, alpha)   the same, the linear colour as numbers
//! c.segs([ax ay bx by width r g b alpha …], {m alpha dash upto})   many seg_rgb in one call (nine numbers each);
//!                                            m: [a b c d e f] places the ends (widths stay in layer units:
//!                                            baked geometry under a moving camera), alpha multiplies each,
//!                                            dash: [on off offset] dashes them in drawn units, the pattern
//!                                            running on along the segments in order (one path);
//!                                            upto: r draws the first r of their length and returns
//!                                            the head as vec2 (the data's units)
//! c.blend(@add | @normal | @max)             segments recorded after it (line batch blending)
//! c.text(text, x, y, {px font color alpha align baseline ls stroke clip})   font: a Font{family weight italic axes}
//!                                            px: em size in layer units; align 0 left .. 0.5 .. 1 right
//!                                            (or @left @center @right); baseline @alphabetic @top @middle
//!                                            @bottom; ls: letter spacing; stroke: outline width (no fill);
//!                                            clip: [x y w h] in the same units as x, y; count: n shows
//!                                            the first n characters (a typewriter)
//! c.rect(x, y, w, h, color, alpha)           filled, transformed
//! c.poly(pts, color, alpha)                  filled polygon [[x y] …], or contours [[[x y] …] …]
//!                                            (even-odd across them: letters with counters), transformed
//! c.circle(x, y, r, color, alpha)            filled disc, transformed
//! c.stroke(pts, width, color, alpha, {closed dash gap offset cap: @round|@butt})
//!                                            polyline, transformed (width scales with it), drawn as segments
//! c.curve(b, width, color, alpha, {n upto})  the cubic Bézier [x0 y0 x1 y1 x2 y2 x3 y3] (the first eight
//!                                            numbers of b) as a stroke of n segments (32), drawn on from
//!                                            u = 0 to upto (1): a draw-on without a loop per point
//! c.gradient(pts, {radial: [cx cy r0 r1] | linear: [x0 y0 x1 y1] stops: [[offset colour] …]}, alpha)
//!                                            contours filled with a gradient, transformed
//! c.composite(@destination_out | @source_over)   fills and text erase what the layer drew
//! c.clip_begin(pts) … c.clip_end()           ops between show only inside the contours (nest)
//! c.save() c.restore() c.set_transform(a, b, c, d, e, f) c.reset() c.translate(x, y) c.rotate(rad)
//! c.scale(sx, sy) c.alpha(k)                 Canvas2D state: alpha multiplies text, fills and strokes
//! c.measure(text, {px font ls count})   advance width in layer units
//! hashn(a, b, c, d)                          (in the module) a stateless hash of 1-4 numbers to 0..1
//! vnoise(x, y, seed)                         (in the module) value noise in -1..1 over hashn's lattice
//!                                            (quintic fade)
//! fixed(x, digits)                           (in the module) x printed with digits decimals
//! ```
//!
//! Colours of text, fills and strokes are colours as elsewhere in Motion
//! (sRGB `#hex`, `rgba()`, `emit()`, or `[r g b (a)]` 0..1 sRGB).

use crate::value::doc_host;
use makepad_script_compute::rand::{hashn, vnoise};
use makepad_script::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// How a run of segments blends into the layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SegBlend {
    /// Premultiplied over.
    Normal,
    /// Summed (glow).
    Add,
    /// The brightest per channel.
    Max,
}

/// A capsule: ends in layer units, width in layer units, linear colour and alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub width: f32,
    pub color: [f32; 4],
}

/// Where a text's `y` sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Baseline {
    Alphabetic,
    Top,
    Middle,
    Bottom,
}

/// One text run: drawn at (`x`, `y`) of its own units, mapped by `m`
/// (a, b, c, d, e, f: x' = a x + c y + e, y' = b x + d y + f) into the layer.
#[derive(Clone, Debug, PartialEq)]
pub struct TextOp {
    pub text: String,
    pub font: String,
    pub weight: f64,
    pub italic: bool,
    pub axes: Vec<(String, f64)>,
    pub px: f64,
    pub x: f64,
    pub y: f64,
    pub m: [f64; 6],
    /// Straight sRGB and alpha (the layer's alpha folded in).
    pub color: [f64; 4],
    pub align: f64,
    pub baseline: Baseline,
    pub ls: f64,
    /// Outline width in the text's units; 0 fills.
    pub stroke: f64,
    /// `[x0, y0, x1, y1]` in the text's units.
    pub clip: Option<[f64; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// `segs[start..end]`, one blend.
    Segs { blend: SegBlend, start: usize, end: usize },
    Text(TextOp),
    /// Filled contours in layer units (even-odd across them), straight
    /// sRGB and alpha.
    Fill { contours: Vec<Vec<[f64; 2]>>, color: [f64; 4] },
    /// Contours filled with a gradient (layer units).
    Gradient { contours: Vec<Vec<[f64; 2]>>, paint: Gradient, alpha: f64 },
    /// Canvas2D `destination-out`: what the layer drew so far is removed
    /// under these contours, by `alpha`.
    Erase { contours: Vec<Vec<[f64; 2]>>, alpha: f64 },
    /// `destination-out` text: what the layer drew so far is removed under
    /// the letters (a knockout), by the op's alpha.
    EraseText(TextOp),
    /// The ops until the matching `ClipEnd` show only inside these contours.
    ClipBegin { contours: Vec<Vec<[f64; 2]>> },
    ClipEnd,
}

/// A gradient in layer units; stops are (offset 0..1, straight sRGB and alpha).
#[derive(Clone, Debug, PartialEq)]
pub enum Gradient {
    /// From the circle (`center`, `r0`) out to (`center`, `r1`), `r1` in x and
    /// `ry` in y (a stretched transform).
    Radial { center: [f64; 2], r0: f64, r1: f64, ry: f64, stops: Vec<(f64, [f64; 4])> },
    Linear { from: [f64; 2], to: [f64; 2], stops: Vec<(f64, [f64; 4])> },
}

thread_local! {
    /// Draw2D calls record where they ran from ([`Draw2dList::marks`]) on
    /// this thread while it evaluates a traced frame
    /// ([`crate::MotionDoc::eval_traced`]).
    pub(crate) static TRACED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What a Draw2D layer recorded for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Draw2dList {
    pub segs: Vec<Seg>,
    pub ops: Vec<Op>,
    /// Where the draw function was when it drew (in a traced frame,
    /// [`crate::MotionDoc::eval_traced`]): from `ops[op]` and
    /// `segs[seg]` on, until the next mark. Not part of the picture (the
    /// hash leaves it out).
    pub marks: Vec<Mark>,
}

/// The code a Draw2D call ran from: the call's site, then the sites of the
/// calls it was inside, innermost first (resolved by
/// [`crate::MotionDoc::code_at`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub op: u32,
    pub seg: u32,
    pub sites: Vec<ScriptIp>,
    /// The loops it ran inside and their values then, outermost first
    /// (`li` 0, `gi` 1): which iteration drew it.
    pub loops: Vec<(makepad_script::LiveId, f64)>,
}

impl Draw2dList {
    fn hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for s in &self.segs {
            for v in [s.a[0], s.a[1], s.b[0], s.b[1], s.width, s.color[0], s.color[1], s.color[2], s.color[3]] {
                v.to_bits().hash(&mut h);
            }
        }
        for op in &self.ops {
            match op {
                Op::Segs { blend, start, end } => (0u8, blend, start, end).hash(&mut h),
                Op::Text(t) => {
                    (1u8, &t.text, &t.font, t.italic, t.baseline).hash(&mut h);
                    for v in [t.weight, t.px, t.x, t.y, t.align, t.ls, t.stroke].iter().chain(&t.m).chain(&t.color).chain(t.clip.iter().flatten()) {
                        v.to_bits().hash(&mut h);
                    }
                    for (k, v) in &t.axes {
                        (k, v.to_bits()).hash(&mut h);
                    }
                }
                Op::Fill { contours, color } => {
                    2u8.hash(&mut h);
                    for v in contours.iter().flatten().flatten().chain(color) {
                        v.to_bits().hash(&mut h);
                    }
                }
                Op::Gradient { contours, paint, alpha } => {
                    3u8.hash(&mut h);
                    let (geo, stops): (Vec<f64>, &Vec<(f64, [f64; 4])>) = match paint {
                        Gradient::Radial { center, r0, r1, ry, stops } => (vec![center[0], center[1], *r0, *r1, *ry], stops),
                        Gradient::Linear { from, to, stops } => (vec![from[0], from[1], to[0], to[1], -1.0], stops),
                    };
                    for v in contours.iter().flatten().flatten().chain(&geo).chain(stops.iter().flat_map(|(o, c)| std::iter::once(o).chain(c))).chain([alpha]) {
                        v.to_bits().hash(&mut h);
                    }
                }
                Op::EraseText(t) => {
                    7u8.hash(&mut h);
                    (&t.text, &t.font, t.italic, t.baseline).hash(&mut h);
                    for v in [t.weight, t.px, t.x, t.y, t.align, t.ls, t.stroke].iter().chain(&t.m).chain(&t.color).chain(t.clip.iter().flatten()) {
                        v.to_bits().hash(&mut h);
                    }
                }
                Op::Erase { contours, alpha } => {
                    4u8.hash(&mut h);
                    for v in contours.iter().flatten().flatten().chain([alpha]) {
                        v.to_bits().hash(&mut h);
                    }
                }
                Op::ClipBegin { contours } => {
                    5u8.hash(&mut h);
                    for v in contours.iter().flatten().flatten() {
                        v.to_bits().hash(&mut h);
                    }
                }
                Op::ClipEnd => 6u8.hash(&mut h),
            }
        }
        h.finish()
    }
}

#[derive(Clone, Copy)]
struct State {
    m: [f64; 6],
    alpha: f64,
    /// Canvas2D `destination-out`: fills erase instead of paint.
    erase: bool,
}

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(m: &[f64; 6], n: &[f64; 6]) -> [f64; 6] {
    [m[0] * n[0] + m[2] * n[1], m[1] * n[0] + m[3] * n[1], m[0] * n[2] + m[2] * n[3], m[1] * n[2] + m[3] * n[3], m[0] * n[4] + m[2] * n[5] + m[4], m[1] * n[4] + m[3] * n[5] + m[5]]
}

fn apply(m: &[f64; 6], p: [f64; 2]) -> [f64; 2] {
    [m[0] * p[0] + m[2] * p[1] + m[4], m[1] * p[0] + m[3] * p[1] + m[5]]
}

/// The recorder the draw function's `c` writes into.
struct Recorder {
    list: Draw2dList,
    blend: SegBlend,
    state: State,
    stack: Vec<State>,
    /// Open `clip_begin`s.
    clips: usize,
    warnings: Vec<String>,
    /// The layer's time this frame (a kernel's output drawn by `c.segs` runs at it).
    time: f64,
}

impl Recorder {
    /// The code drawing from here on (a new mark when it moved).
    fn mark(&mut self, sites: &[ScriptIp], loops: &[(makepad_script::LiveId, f64)]) {
        if self.list.marks.last().is_some_and(|m| m.sites == sites && m.loops == loops) {
            return;
        }
        let (op, seg) = (self.list.ops.len() as u32, self.list.segs.len() as u32);
        // A mark nothing was drawn under gives way.
        if let Some(last) = self.list.marks.last_mut().filter(|m| m.op == op && m.seg == seg) {
            last.sites = sites.to_vec();
            last.loops = loops.to_vec();
            return;
        }
        self.list.marks.push(Mark { op, seg, sites: sites.to_vec(), loops: loops.to_vec() });
    }
}

impl Recorder {
    fn new(time: f64) -> Self {
        Self { list: Draw2dList::default(), blend: SegBlend::Add, state: State { m: IDENTITY, alpha: 1.0, erase: false }, stack: Vec::new(), clips: 0, warnings: Vec::new(), time }
    }

    fn warn(&mut self, w: &str) {
        if self.warnings.len() < 8 && !self.warnings.iter().any(|x| x == w) {
            self.warnings.push(w.to_string());
        }
    }

    fn push_seg(&mut self, s: Seg) {
        let n = self.list.segs.len();
        match self.list.ops.last_mut() {
            Some(Op::Segs { blend, end, .. }) if *blend == self.blend && *end == n => *end = n + 1,
            _ => self.list.ops.push(Op::Segs { blend: self.blend, start: n, end: n + 1 }),
        }
        self.list.segs.push(s);
    }

    fn fill(&mut self, pts: &[[f64; 2]], color: [f64; 4]) {
        self.fill_contours(&[pts.to_vec()], color);
    }

    fn fill_contours(&mut self, contours: &[Vec<[f64; 2]>], color: [f64; 4]) {
        if color[3] * self.state.alpha <= 0.0 {
            return;
        }
        let contours = self.placed(contours);
        if contours.is_empty() {
            return;
        }
        let a = color[3] * self.state.alpha;
        if self.state.erase {
            self.list.ops.push(Op::Erase { contours, alpha: a.min(1.0) });
        } else {
            self.list.ops.push(Op::Fill { contours, color: [color[0], color[1], color[2], a] });
        }
    }

    /// Contours of 3 points or more, through the current transform.
    fn placed(&self, contours: &[Vec<[f64; 2]>]) -> Vec<Vec<[f64; 2]>> {
        let m = self.state.m;
        contours.iter().filter(|c| c.len() >= 3).map(|c| c.iter().map(|p| apply(&m, *p)).collect()).collect()
    }

    /// A polyline through the current transform as segments (width scales
    /// with it), dashed when `dash` and `gap` are both over 0.
    #[allow(clippy::too_many_arguments)]
    fn stroke(&mut self, p: &[[f64; 2]], width: f64, color: [f64; 4], alpha: f64, dash: f64, gap: f64, offset: f64) {
        let m = self.state.m;
        let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
        let w = width * scale;
        let a = color[3] * alpha * self.state.alpha;
        if a <= 0.0 || p.len() < 2 {
            return;
        }
        let lin = makepad_tween::color_srgb_to_linear([color[0], color[1], color[2], 1.0]);
        let col = [lin[0] as f32, lin[1] as f32, lin[2] as f32, a.min(1.0) as f32];
        let pts: Vec<[f64; 2]> = p.iter().map(|q| apply(&m, *q)).collect();
        let saved = self.blend;
        self.blend = SegBlend::Normal;
        // Dashes run in drawn units along the whole polyline (Canvas2D's setLineDash).
        let (on, off) = (dash * scale, gap * scale);
        let mut acc = offset * scale;
        for w2 in pts.windows(2) {
            let (p0, p1) = (w2[0], w2[1]);
            let len = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt();
            if on <= 0.0 || off <= 0.0 {
                self.push_seg(Seg { a: [p0[0] as f32, p0[1] as f32], b: [p1[0] as f32, p1[1] as f32], width: w as f32, color: col });
                continue;
            }
            let period = on + off;
            let mut u = 0.0;
            while u < len {
                let ph = (acc + u).rem_euclid(period);
                let run = if ph < on { on - ph } else { period - ph };
                let run = run.min(len - u).max(1e-6);
                if ph < on {
                    let (f0, f1) = (u / len, (u + run) / len);
                    let lerp = |f: f64| [(p0[0] + (p1[0] - p0[0]) * f) as f32, (p0[1] + (p1[1] - p0[1]) * f) as f32];
                    self.push_seg(Seg { a: lerp(f0), b: lerp(f1), width: w as f32, color: col });
                }
                u += run;
            }
            acc += len;
        }
        self.blend = saved;
    }
}

/// The n + 1 points of the cubic Bézier `b` (x0 y0 x1 y1 x2 y2 x3 y3) at
/// u = i * (upto / n), in the Bernstein form, each product and sum in the
/// order a Splash loop writing `v * v * v * x0 + 3 * v * v * u * x1 + …`
/// makes them (so the points are the loop's, bit for bit).
fn cubic_points(b: &[f64], n: f64, upto: f64) -> Vec<[f64; 2]> {
    let du = upto / n;
    (0..=n as usize)
        .map(|i| {
            let u = i as f64 * du;
            let v = 1.0 - u;
            let (a0, a1, a2, a3) = (v * v * v, 3.0 * v * v * u, 3.0 * v * u * u, u * u * u);
            [a0 * b[0] + a1 * b[2] + a2 * b[4] + a3 * b[6], a0 * b[1] + a1 * b[3] + a2 * b[5] + a3 * b[7]]
        })
        .collect()
}

/// A kernel's output as segments (see `crate::kernel2d`); kernels need the
/// `scene3d` feature's runtime.
/// The segments `c.segs` draws: the flat nine numbers a document writes
/// (ax ay bx by width r g b alpha), or a kernel's `Segment` records as it
/// gives them (a.xyz b.xyz r g b alpha width, eleven floats each).
enum Segs {
    Flat(Vec<f64>),
    Records(std::sync::Arc<Vec<f32>>),
}

impl Segs {
    fn count(&self) -> usize {
        match self {
            Segs::Flat(v) => v.len() / 9,
            Segs::Records(r) => r.len() / 11,
        }
    }

    /// Segment `k`'s `j`th number in the flat order.
    fn get(&self, k: usize, j: usize) -> f64 {
        const RECORD: [usize; 9] = [0, 1, 3, 4, 10, 6, 7, 8, 9];
        match self {
            Segs::Flat(v) => v[k * 9 + j],
            Segs::Records(r) => r[k * 11 + RECORD[j]] as f64,
        }
    }
}

fn kernel_segments(vm: &mut ScriptVm, data: ScriptValue, time: f64, with: Option<ScriptObject>) -> Option<Result<std::sync::Arc<Vec<f32>>, String>> {
    #[cfg(feature = "scene3d")]
    return crate::kernel2d::frame_segments(vm, data, time, with);
    #[cfg(not(feature = "scene3d"))]
    {
        let _ = (vm, data, time, with);
        None
    }
}

/// A document VM's Draw2D state ([`crate::value::DocHost`]): the `c` object
/// and the recorder its methods fill while a draw function runs.
#[derive(Default)]
pub(crate) struct Draw2dHost {
    rec: Option<Recorder>,
    c: ScriptValue,
    /// `c` lives across frames: held for the document VM's collector.
    _c_root: Option<ScriptObjectRef>,
    sites: Vec<ScriptIp>,
}

/// A Draw2D layer's picture at one moment, as a frame carries it: the
/// recorded list and its hash (frames compare by it).
#[derive(Clone, Debug)]
pub struct Draw2dFrame {
    pub list: Arc<Draw2dList>,
    pub hash: u64,
}

impl PartialEq for Draw2dFrame {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash
    }
}

/// Draw2D's natives in `mod.motion` and its `c` object, in a new document
/// VM.
pub(crate) fn install(vm: &mut ScriptVm, module: ScriptObject) {
    // A stateless hash of up to four numbers to 0..1, the same in every
    // host (per-element randomness a draw function keys on indices).
    vm.add_method(module, id_lut!(hashn), script_args!(a = NIL, b = NIL, c = NIL, d = NIL), |vm, args| {
        let xs: Vec<f64> = [id!(a), id!(b), id!(c), id!(d)].into_iter().filter_map(|k| get(vm, args, k).as_number()).collect();
        hashn(&xs).into()
    });
    // Value noise of the plane in -1..1: hashn at the integer lattice,
    // blended by the quintic fade (what a jitter or tremor per point
    // calls thousands of times a frame, one call instead of a Splash
    // function of four hashes).
    vm.add_method(module, id_lut!(vnoise), script_args!(x = 0.0, y = 0.0, seed = 0.0), |vm, args| {
        vnoise(num(vm, args, id!(x), 0.0), num(vm, args, id!(y), 0.0), num(vm, args, id!(seed), 0.0)).into()
    });
    // A number with `digits` decimals, as JavaScript's toFixed prints it.
    vm.add_method(module, id_lut!(fixed), script_args!(x = 0.0, digits = 0.0), |vm, args| {
        let x = num(vm, args, id!(x), 0.0);
        let d = num(vm, args, id!(digits), 0.0).clamp(0.0, 20.0) as usize;
        let s = to_fixed(x, d);
        vm.new_string_with(|_, out| out.push_str(&s))
    });
    let c = recorder_object(vm);
    let _c_root = c.as_object().map(|o| vm.bx.heap_mut().new_object_ref(o));
    doc_host(vm).draw2d = Draw2dHost { rec: None, c, _c_root, sites: Vec::new() };
}

/// Run a Draw2D layer's `draw` function at layer time `local` (host time
/// `edit`): what it drew and its messages.
pub(crate) fn eval(vm: &mut ScriptVm, draw: ScriptValue, local: f64, edit: f64) -> (Draw2dFrame, Vec<String>) {
    doc_host(vm).draw2d.rec = Some(Recorder::new(local));
    vm.capture_errors();
    let c = doc_host(vm).draw2d.c;
    vm.call(draw, &[c, local.into(), edit.into()]);
    let mut errors: Vec<String> = vm.take_errors().into_iter().map(|e| e.to_string()).collect();
    let mut rec = doc_host(vm).draw2d.rec.take().expect("the Draw2D recorder is filled for the call");
    if rec.clips > 0 {
        rec.warn("clip_begin without clip_end (closed at the end)");
        for _ in 0..rec.clips {
            rec.list.ops.push(Op::ClipEnd);
        }
    }
    errors.extend(rec.warnings.iter().map(|w| format!("Draw2D: {w}")));
    let hash = rec.list.hash();
    (Draw2dFrame { list: Arc::new(rec.list), hash }, errors)
}

/// `x` with `d` decimals as JavaScript's toFixed prints it: the exact
/// decimal value of the double rounded half up on its magnitude, a minus for
/// any negative value (Rust's own formatting rounds an exact half to even).
pub fn to_fixed(x: f64, d: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() { "NaN".into() } else if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    // The double's exact decimal expansion is finite (at most 1074 digits);
    // 40 more digits than asked decide the rounding.
    let exact = format!("{:.*}", d + 40, x.abs());
    let (int, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().take(d)).map(|b| b - b'0').collect();
    let round_up = frac.as_bytes().get(d).is_some_and(|b| *b >= b'5');
    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let n_int = digits.len() - d;
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    out.extend(digits[..n_int].iter().map(|d| (b'0' + d) as char));
    if d > 0 {
        out.push('.');
        out.extend(digits[n_int..].iter().map(|d| (b'0' + d) as char));
    }
    out
}

// ---- reading arguments ------------------------------------------------------

fn get(vm: &ScriptVm, obj: ScriptObject, key: LiveId) -> ScriptValue {
    let v = vm.bx.heap_ref().value(obj, key.into(), NoTrap);
    if v.is_err() {
        NIL
    } else {
        v
    }
}

fn num(vm: &ScriptVm, obj: ScriptObject, key: LiveId, default: f64) -> f64 {
    get(vm, obj, key).as_number().filter(|n| n.is_finite()).unwrap_or(default)
}

fn opt_obj(v: ScriptValue) -> Option<ScriptObject> {
    if v.is_nil() {
        None
    } else {
        v.as_object()
    }
}

fn string_of(vm: &ScriptVm, v: ScriptValue) -> Option<String> {
    if let Some(s) = vm.bx.heap_ref().string_with(v, |_, s| s.to_string()) {
        return Some(s);
    }
    if let Some(id) = v.as_id() {
        return id.as_string(|s| s.map(str::to_string));
    }
    v.as_number().map(crate::value::fmt_num)
}

/// Numbers from a number, an array or a Splash vector.
fn nums_of(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<f64>> {
    if let Some(n) = v.as_number() {
        return Some(vec![n]);
    }
    if let Some(pod) = v.as_pod() {
        let (ty, data) = vm.bx.heap_ref().pod_data(pod);
        let lanes = match ty.ty().as_vec() {
            Some(makepad_script::pod::ScriptPodVec::Vec2f) => 2,
            Some(makepad_script::pod::ScriptPodVec::Vec3f) => 3,
            Some(makepad_script::pod::ScriptPodVec::Vec4f) => 4,
            None if ty.ty().kind() == makepad_script::pod::ScriptPodTyTag::F32 => 1,
            _ => return None,
        };
        return Some(data.iter().take(lanes).map(|b| f32::from_bits(*b) as f64).collect());
    }
    let array = v.as_array()?;
    let heap = vm.heap();
    (0..heap.array_len(array)).map(|i| heap.array_index(array, i, NoTrap).as_number()).collect()
}

/// A colour (sRGB straight): `#hex`, `rgba()`, `emit()` (components over 1
/// kept), `[r g b (a)]`.
fn color_of(vm: &ScriptVm, v: ScriptValue) -> Option<[f64; 4]> {
    if let Some(c) = v.as_color() {
        return Some(crate::load::unpack(c));
    }
    if let Some(obj) = opt_obj(v) {
        let n = nums_of(vm, get(vm, obj, id!(color)))?;
        return (n.len() >= 3).then(|| [n[0], n[1], n[2], n.get(3).copied().unwrap_or(1.0)]);
    }
    let n = nums_of(vm, v)?;
    (n.len() >= 3).then(|| [n[0], n[1], n[2], n.get(3).copied().unwrap_or(1.0)])
}

/// A segment colour: a list or vector is linear light as given; a colour is
/// converted from sRGB (an `emit()` keeps its multiple).
fn linear_of(vm: &ScriptVm, v: ScriptValue) -> Option<[f64; 3]> {
    if v.as_color().is_some() || opt_obj(v).is_some() {
        let c = color_of(vm, v)?;
        let lin = makepad_tween::color_srgb_to_linear(c);
        return Some([lin[0], lin[1], lin[2]]);
    }
    let n = nums_of(vm, v)?;
    (n.len() >= 3).then(|| [n[0], n[1], n[2]])
}

fn points_of(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<[f64; 2]>> {
    let array = v.as_array()?;
    let heap = vm.heap();
    let len = heap.array_len(array);
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let n = nums_of(vm, heap.array_index(array, i, NoTrap))?;
        if n.len() < 2 {
            return None;
        }
        out.push([n[0], n[1]]);
    }
    Some(out)
}

/// One contour `[[x, y], …]`, or several `[[[x, y], …], …]`.
fn contours_of(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<Vec<[f64; 2]>>> {
    points_of(vm, v).map(|p| vec![p]).or_else(|| {
        let array = v.as_array()?;
        (0..vm.bx.heap_ref().array_len(array)).map(|i| points_of(vm, vm.bx.heap_ref().array_index(array, i, NoTrap))).collect()
    })
}

/// Gradient stops `[[offset, colour], …]` (offsets clamped, in order).
fn stops_of(vm: &ScriptVm, v: ScriptValue) -> Option<Vec<(f64, [f64; 4])>> {
    let array = v.as_array()?;
    let heap = vm.heap();
    let mut out = Vec::new();
    for i in 0..heap.array_len(array) {
        let stop = heap.array_index(array, i, NoTrap).as_array()?;
        let offset = heap.array_index(stop, 0, NoTrap).as_number()?.clamp(0.0, 1.0);
        let color = color_of(vm, heap.array_index(stop, 1, NoTrap))?;
        out.push((offset, color));
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    (!out.is_empty()).then_some(out)
}

fn align_of(v: ScriptValue) -> Option<f64> {
    if let Some(n) = v.as_number() {
        return Some(n.clamp(0.0, 1.0));
    }
    let id = v.as_id()?;
    Some(if id == id!(left) || id == id!(start) {
        0.0
    } else if id == id!(center) || id == id!(centre) {
        0.5
    } else if id == id!(right) || id == id!(end) {
        1.0
    } else {
        return None;
    })
}

fn baseline_of(v: ScriptValue) -> Option<Baseline> {
    let id = v.as_id()?;
    Some(if id == id!(top) || id == id!(hanging) {
        Baseline::Top
    } else if id == id!(middle) {
        Baseline::Middle
    } else if id == id!(bottom) || id == id!(ideographic) {
        Baseline::Bottom
    } else {
        Baseline::Alphabetic
    })
}

/// The font a text names (a document font, or its alias), or why it cannot
/// draw (no such font, or an external family not installed): the frame's
/// Draw2D errors say each once and the text is not drawn.
fn font_of(vm: &ScriptVm, o: Option<ScriptObject>) -> Result<crate::font_spec::FontSpec, String> {
    let spec = match o.map(|o| get(vm, o, id!(font))).filter(|v| !v.is_nil()) {
        Some(v) => crate::font_spec::font_spec(vm, v)?,
        None => crate::font_spec::FontSpec { family: crate::font_spec::font_or_error("Inter")?, weight: 400.0, italic: false, axes: Vec::new() },
    };
    let font = spec.family;
    // `glyphs:` in the options: the coverage a build must keep (see
    // `makepad_doc_fonts::CoverageDecl`).
    if let Some(o) = o {
        let g = get(vm, o, id!(glyphs));
        let spec = g.as_id().map(|id| id.to_string()).or_else(|| string_of(vm, g));
        if let Some(spec) = spec.filter(|s| makepad_doc_fonts::check_coverage(s).is_ok()) {
            makepad_doc_fonts::declare_coverage(font, &spec);
        }
    }
    Ok(spec)
}

fn circle_pts(x: f64, y: f64, r: f64, scale: f64) -> Vec<[f64; 2]> {
    // Enough sides that the chord error stays under a tenth of a unit at the drawn size.
    let n = ((std::f64::consts::PI / (1.0 - 0.1 / (r * scale).max(0.2)).clamp(-1.0, 0.9999).acos()).ceil() as usize).clamp(8, 256);
    (0..n).map(|i| {
        let a = i as f64 / n as f64 * std::f64::consts::TAU;
        [x + r * a.cos(), y + r * a.sin()]
    }).collect()
}

macro_rules! with_rec {
    ($vm:expr, $r:ident, $body:block) => {{
        if TRACED.with(std::cell::Cell::get) {
            let mut sites = std::mem::take(&mut doc_host($vm).draw2d.sites);
            $vm.call_sites(&mut sites, 8);
            let mut loops = Vec::new();
            $vm.loop_values(&mut loops);
            let host = &mut doc_host($vm).draw2d;
            if let Some(r) = host.rec.as_mut() {
                r.mark(&sites, &loops);
            }
            host.sites = sites;
        }
        match doc_host($vm).draw2d.rec.as_mut() {
            Some($r) => $body,
            None => NIL,
        }
    }};
}

fn recorder_object(vm: &mut ScriptVm) -> ScriptValue {
    let obj = vm.bx.heap_mut().new_object();
    vm.add_method(obj, id_lut!(seg), script_args!(ax = 0.0, ay = 0.0, bx = 0.0, by = 0.0, width = 1.0, rgb = NIL, alpha = 1.0), move |vm, a| {
        let n = |k| num(vm, a, k, 0.0);
        let (ax, ay, bx, by) = (n(id!(ax)), n(id!(ay)), n(id!(bx)), n(id!(by)));
        let width = num(vm, a, id!(width), 1.0);
        let alpha = num(vm, a, id!(alpha), 1.0);
        let rgb = linear_of(vm, get(vm, a, id!(rgb)));
        with_rec!(vm, r, {
            match rgb {
                Some(c) if alpha > 0.0 => r.push_seg(Seg { a: [ax as f32, ay as f32], b: [bx as f32, by as f32], width: width as f32, color: [c[0] as f32, c[1] as f32, c[2] as f32, alpha.min(1.0) as f32] }),
                Some(_) => {}
                None => r.warn("seg(ax, ay, bx, by, width, rgb, alpha): rgb is [r, g, b] (linear) or a colour"),
            }
            NIL
        })
    });
    // The same with the colour as three numbers (no vector made per call:
    // what a loop drawing thousands of segments uses).
    vm.add_method(obj, id_lut!(seg_rgb), script_args!(ax = 0.0, ay = 0.0, bx = 0.0, by = 0.0, width = 1.0, r = 1.0, g = 1.0, b = 1.0, alpha = 1.0), move |vm, a| {
        let n = |k, d| num(vm, a, k, d);
        let alpha = n(id!(alpha), 1.0);
        let seg = Seg {
            a: [n(id!(ax), 0.0) as f32, n(id!(ay), 0.0) as f32],
            b: [n(id!(bx), 0.0) as f32, n(id!(by), 0.0) as f32],
            width: n(id!(width), 1.0) as f32,
            color: [n(id!(r), 1.0) as f32, n(id!(g), 1.0) as f32, n(id!(b), 1.0) as f32, alpha.min(1.0) as f32],
        };
        with_rec!(vm, r, {
            if alpha > 0.0 {
                r.push_seg(seg);
            }
            NIL
        })
    });
    // `c.segs(data, {m alpha})`: many segments in one call, a flat array of
    // nine numbers each (ax, ay, bx, by, width, r, g, b, alpha), as
    // `seg_rgb` takes them: a call per segment costs more than drawing it.
    // `m` places the ends by the affine [a b c d e f] (x' = a x + c y + e,
    // y' = b x + d y + f; widths stay in layer units) and `alpha` multiplies
    // every segment's alpha: an array made once (baked strokes) drawn each
    // frame under a moving camera and a fade, with no Splash loop per point.
    // `dash: [on, off, offset]` dashes the (placed) segments as `stroke`
    // dashes a polyline: the pattern runs on from one segment to the next.
    // `upto: r` (0..1) draws the first r of the segments' total length (in
    // the data's own units, in order: a traced outline drawing on) and
    // returns where it stops, as vec2 in the data's units (for a spark at
    // the head); without it nothing is returned.
    vm.add_method(obj, id_lut!(segs), script_args!(data = NIL, o = NIL), move |vm, a| {
        // The segments: a flat array, or a kernel's `Segment` output (`k.out`),
        // run now at the layer's time.
        let data = vm.bx.heap_ref().value(a, id!(data).into(), NoTrap);
        let with = opt_obj(get(vm, a, id!(o))).and_then(|o| opt_obj(get(vm, o, id!(with))));
        let time = doc_host(vm).draw2d.rec.as_ref().map_or(0.0, |r| r.time);
        // (an array a load-time kernel gave, drawn with the draw's own data,
        // is that kernel run again: see kernel2d)
        let rerun = if with.is_some() && data.as_array().is_some() { kernel_segments(vm, data, time, with) } else { None };
        let values: Segs = if let Some(Ok(records)) = rerun {
            Segs::Records(records)
        } else if let Some(arr) = data.as_array() {
            Segs::Flat((0..vm.bx.heap_ref().array_len(arr)).map(|i| vm.bx.heap_ref().array_index(arr, i, NoTrap).as_number().unwrap_or(0.0)).collect())
        } else {
            match kernel_segments(vm, data, time, with) {
                Some(Ok(records)) => Segs::Records(records),
                Some(Err(e)) => return with_rec!(vm, r, {
                    r.warn(&format!("segs: {e}"));
                    NIL
                }),
                None => return with_rec!(vm, r, {
                    r.warn("segs([ax, ay, bx, by, width, r, g, b, alpha, ...], {m: [a, b, c, d, e, f] alpha dash: [on, off, offset]}), or a kernel's Segment output: segs(k.out)");
                    NIL
                }),
            }
        };
        let o = opt_obj(get(vm, a, id!(o)));
        let m = o.and_then(|o| nums_of(vm, get(vm, o, id!(m)))).filter(|m| m.len() >= 6).map(|m| [m[0], m[1], m[2], m[3], m[4], m[5]]);
        let fade = o.map_or(1.0, |o| num(vm, o, id!(alpha), 1.0));
        let dash = o.and_then(|o| nums_of(vm, get(vm, o, id!(dash)))).filter(|d| d.len() >= 2 && d[0] > 0.0 && d[1] > 0.0);
        let upto = o.map(|o| get(vm, o, id!(upto))).filter(|u| !u.is_nil()).map(|u| u.as_number().unwrap_or(1.0));
        let n = values.count() * 9;
        let v = |i: usize| values.get(i / 9, i % 9);
        // `upto`: how much length is left to draw, and where the drawing stops.
        let mut left = upto.map(|r| {
            // Lengths as a Splash loop measures them (`sqrt(pow(dx, 2.0) + pow(dy, 2.0))`).
            let total: f64 = (0..n / 9).map(|k| ((v(k * 9 + 2) - v(k * 9)).powf(2.0) + (v(k * 9 + 3) - v(k * 9 + 1)).powf(2.0)).sqrt()).sum();
            r * total
        });
        let mut head: Option<[f64; 2]> = None;
        let at = |x: f64, y: f64| match &m {
            Some(m) => apply(m, [x, y]),
            None => [x, y],
        };
        let mut segs: Vec<Seg> = Vec::with_capacity(n / 9);
        // Dashes run in drawn units along the segments in order, as one path.
        let mut acc = dash.as_ref().map_or(0.0, |d| d.get(2).copied().unwrap_or(0.0));
        for k in 0..n / 9 {
            let o = k * 9;
            let alpha = v(o + 8) * fade;
            let (mut ex, mut ey) = (v(o + 2), v(o + 3));
            if let Some(l) = &mut left {
                // Drawn on by length: this segment up to what is left.
                let (ax, ay) = (v(o), v(o + 1));
                let len = ((ex - ax).powf(2.0) + (ey - ay).powf(2.0)).sqrt();
                let f = if len > 0.0 { (*l / len).min(1.0) } else { 1.0 };
                (ex, ey) = (ax + (ex - ax) * f, ay + (ey - ay) * f);
                *l -= len;
                head = Some([ex, ey]);
            }
            let (p0, p1) = (at(v(o), v(o + 1)), at(ex, ey));
            let width = v(o + 4) as f32;
            let color = [v(o + 5) as f32, v(o + 6) as f32, v(o + 7) as f32, alpha.min(1.0) as f32];
            let Some(d) = &dash else {
                if alpha > 0.0 {
                    segs.push(Seg { a: [p0[0] as f32, p0[1] as f32], b: [p1[0] as f32, p1[1] as f32], width, color });
                }
                if left.is_some_and(|l| l <= 0.0) {
                    break;
                }
                continue;
            };
            let (on, period) = (d[0], d[0] + d[1]);
            let len = ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt();
            let mut u = 0.0;
            while u < len {
                let ph = (acc + u).rem_euclid(period);
                let run = if ph < on { on - ph } else { period - ph }.min(len - u).max(1e-6);
                if ph < on && alpha > 0.0 {
                    let (f0, f1) = (u / len, (u + run) / len);
                    let lerp = |f: f64| [(p0[0] + (p1[0] - p0[0]) * f) as f32, (p0[1] + (p1[1] - p0[1]) * f) as f32];
                    segs.push(Seg { a: lerp(f0), b: lerp(f1), width, color });
                }
                u += run;
            }
            acc += len;
            if left.is_some_and(|l| l <= 0.0) {
                break;
            }
        }
        let head = match head {
            Some([x, y]) => {
                let ty = vm.pod_builtins().pod_vec2f;
                let pod = vm.bx.heap_mut().new_pod(ty);
                let data = vm.bx.heap_mut().pod_data_mut(pod);
                data[0] = (x as f32).to_bits();
                data[1] = (y as f32).to_bits();
                pod.into()
            }
            None => NIL,
        };
        with_rec!(vm, r, {
            for seg in segs {
                r.push_seg(seg);
            }
            head
        })
    });
    vm.add_method(obj, id_lut!(blend), script_args!(mode = NIL), move |vm, a| {
        let mode = vm.bx.heap_ref().value(a, id!(mode).into(), NoTrap).as_id();
        with_rec!(vm, r, {
            r.blend = match mode {
                Some(m) if m == id!(normal) || m == id!(over) => SegBlend::Normal,
                Some(m) if m == id!(max) => SegBlend::Max,
                Some(m) if m == id!(add) || m == id!(additive) || m == id!(lighter) => SegBlend::Add,
                _ => {
                    r.warn("blend(@add | @normal | @max)");
                    r.blend
                }
            };
            NIL
        })
    });
    vm.add_method(obj, id_lut!(text), script_args!(text = NIL, x = 0.0, y = 0.0, o = NIL), move |vm, a| {
        let text = string_of(vm, get(vm, a, id!(text))).unwrap_or_default();
        let (x, y) = (num(vm, a, id!(x), 0.0), num(vm, a, id!(y), 0.0));
        let o = opt_obj(get(vm, a, id!(o)));
        let px = o.map_or(16.0, |o| num(vm, o, id!(px), 16.0));
        let font = match font_of(vm, o) {
            Ok(font) => font,
            Err(error) => return with_rec!(vm, r, {
                r.warn(&error);
                NIL
            }),
        };
        let color = o.and_then(|o| color_of(vm, get(vm, o, id!(color)))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = o.map_or(1.0, |o| num(vm, o, id!(alpha), 1.0));
        let (weight, italic, axes) = (font.weight, font.italic, font.axes.clone());
        let font = font.family.to_string();
        let align = o.and_then(|o| align_of(get(vm, o, id!(align)))).unwrap_or(0.0);
        let baseline = o.and_then(|o| baseline_of(get(vm, o, id!(baseline)))).unwrap_or(Baseline::Alphabetic);
        let ls = o.map_or(0.0, |o| num(vm, o, id!(ls), 0.0));
        let stroke = o.map_or(0.0, |o| num(vm, o, id!(stroke), 0.0)).max(0.0);
        let clip = o.and_then(|o| nums_of(vm, get(vm, o, id!(clip)))).filter(|c| c.len() >= 4).map(|c| [c[0], c[1], c[0] + c[2], c[1] + c[3]]);
        let count = o.and_then(|o| get(vm, o, id!(count)).as_number());
        // `count: n`: the first n characters (a typewriter), placed as the whole text would be.
        let text = match count {
            Some(n) => text.chars().take(n.max(0.0) as usize).collect(),
            None => text,
        };
        with_rec!(vm, r, {
            let a = color[3] * alpha * r.state.alpha;
            if !text.is_empty() && a > 0.0 && px > 0.0 {
                let op = TextOp { text, font, weight, italic, axes, px, x, y, m: r.state.m, color: [color[0], color[1], color[2], a.min(1.0)], align, baseline, ls, stroke, clip };
                r.list.ops.push(if r.state.erase { Op::EraseText(op) } else { Op::Text(op) });
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(measure), script_args!(text = NIL, o = NIL), move |vm, a| {
        let text = string_of(vm, get(vm, a, id!(text))).unwrap_or_default();
        let o = opt_obj(get(vm, a, id!(o)));
        let px = o.map_or(16.0, |o| num(vm, o, id!(px), 16.0));
        let font = match font_of(vm, o) {
            Ok(font) => font,
            Err(error) => return with_rec!(vm, r, {
                r.warn(&error);
                0.0.into()
            }),
        };
        let ls = o.map_or(0.0, |o| num(vm, o, id!(ls), 0.0));
        let count = o.and_then(|o| get(vm, o, id!(count)).as_number());
        let text: String = match count {
            Some(n) => text.chars().take(n.max(0.0) as usize).collect(),
            None => text,
        };
        match crate::type_fns::text_width(&text, font.family, px, font.weight, font.italic, &font.axes) {
            Ok(w) => (w + ls * text.chars().count() as f64).into(),
            Err(e) => with_rec!(vm, r, {
                r.warn(&e);
                0.0.into()
            }),
        }
    });
    vm.add_method(obj, id_lut!(rect), script_args!(x = 0.0, y = 0.0, w = 0.0, h = 0.0, color = NIL, alpha = 1.0), move |vm, a| {
        let n = |k| num(vm, a, k, 0.0);
        let (x, y, w, h) = (n(id!(x)), n(id!(y)), n(id!(w)), n(id!(h)));
        let color = color_of(vm, get(vm, a, id!(color))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = num(vm, a, id!(alpha), 1.0);
        with_rec!(vm, r, {
            r.fill(&[[x, y], [x + w, y], [x + w, y + h], [x, y + h]], [color[0], color[1], color[2], color[3] * alpha]);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(poly), script_args!(pts = NIL, color = NIL, alpha = 1.0), move |vm, a| {
        let pts = contours_of(vm, get(vm, a, id!(pts)));
        let color = color_of(vm, get(vm, a, id!(color))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = num(vm, a, id!(alpha), 1.0);
        with_rec!(vm, r, {
            match pts {
                Some(p) => r.fill_contours(&p, [color[0], color[1], color[2], color[3] * alpha]),
                None => r.warn("poly(pts, colour, alpha): pts is [[x, y], …] or a list of them"),
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(circle), script_args!(x = 0.0, y = 0.0, r = 1.0, color = NIL, alpha = 1.0), move |vm, a| {
        let (x, y, rad) = (num(vm, a, id!(x), 0.0), num(vm, a, id!(y), 0.0), num(vm, a, id!(r), 1.0));
        let color = color_of(vm, get(vm, a, id!(color))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = num(vm, a, id!(alpha), 1.0);
        with_rec!(vm, r, {
            if rad > 0.0 {
                let m = r.state.m;
                let scale = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                r.fill(&circle_pts(x, y, rad, scale), [color[0], color[1], color[2], color[3] * alpha]);
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(stroke), script_args!(pts = NIL, width = 1.0, color = NIL, alpha = 1.0, o = NIL), move |vm, a| {
        let pts = points_of(vm, get(vm, a, id!(pts)));
        let width = num(vm, a, id!(width), 1.0);
        let color = color_of(vm, get(vm, a, id!(color))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = num(vm, a, id!(alpha), 1.0);
        let o = opt_obj(get(vm, a, id!(o)));
        let closed = o.and_then(|o| get(vm, o, id!(closed)).as_bool()).unwrap_or(false);
        let dash = o.map_or(0.0, |o| num(vm, o, id!(dash), 0.0));
        let gap = o.map_or(dash, |o| num(vm, o, id!(gap), dash));
        let offset = o.map_or(0.0, |o| num(vm, o, id!(offset), 0.0));
        with_rec!(vm, r, {
            let Some(mut p) = pts else {
                r.warn("stroke(pts, width, colour, alpha, {…}): pts is [[x, y], …]");
                return NIL;
            };
            if closed && p.len() > 2 {
                p.push(p[0]);
            }
            r.stroke(&p, width, color, alpha, dash, gap, offset);
            NIL
        })
    });
    // `c.curve(b, width, colour, alpha, {n upto})`: the cubic Bézier of
    // the first eight numbers of `b` (x0 y0, the two controls, x3 y3)
    // stroked as `stroke` strokes the n + 1 points at u = i * (upto / n),
    // i = 0..n: a curve drawn on up to `upto` (0..1), its points computed
    // here instead of by a Splash loop per point.
    vm.add_method(obj, id_lut!(curve), script_args!(b = NIL, width = 1.0, color = NIL, alpha = 1.0, o = NIL), move |vm, a| {
        let b = nums_of(vm, get(vm, a, id!(b)));
        let width = num(vm, a, id!(width), 1.0);
        let color = color_of(vm, get(vm, a, id!(color))).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let alpha = num(vm, a, id!(alpha), 1.0);
        let o = opt_obj(get(vm, a, id!(o)));
        let n = o.map_or(32.0, |o| num(vm, o, id!(n), 32.0)).round().clamp(1.0, 4096.0);
        let upto = o.map_or(1.0, |o| num(vm, o, id!(upto), 1.0));
        with_rec!(vm, r, {
            let Some(b) = b.filter(|b| b.len() >= 8) else {
                r.warn("curve(b, width, colour, alpha, {n upto}): b is [x0, y0, x1, y1, x2, y2, x3, y3]");
                return NIL;
            };
            r.stroke(&cubic_points(&b, n, upto), width, color, alpha, 0.0, 0.0, 0.0);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(save), script_args!(), move |vm, _a| {
        with_rec!(vm, r, {
            r.stack.push(r.state);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(restore), script_args!(), move |vm, _a| {
        with_rec!(vm, r, {
            if let Some(st) = r.stack.pop() {
                r.state = st;
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(set_transform), script_args!(a = 1.0, b = 0.0, c = 0.0, d = 1.0, e = 0.0, f = 0.0), move |vm, a| {
        let m = [num(vm, a, id!(a), 1.0), num(vm, a, id!(b), 0.0), num(vm, a, id!(c), 0.0), num(vm, a, id!(d), 1.0), num(vm, a, id!(e), 0.0), num(vm, a, id!(f), 0.0)];
        with_rec!(vm, r, {
            r.state.m = m;
            NIL
        })
    });
    vm.add_method(obj, id_lut!(reset), script_args!(), move |vm, _a| {
        with_rec!(vm, r, {
            r.state.m = IDENTITY;
            NIL
        })
    });
    vm.add_method(obj, id_lut!(translate), script_args!(x = 0.0, y = 0.0), move |vm, a| {
        let t = [1.0, 0.0, 0.0, 1.0, num(vm, a, id!(x), 0.0), num(vm, a, id!(y), 0.0)];
        with_rec!(vm, r, {
            r.state.m = mul(&r.state.m, &t);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(rotate), script_args!(rad = 0.0), move |vm, a| {
        let (sn, cs) = num(vm, a, id!(rad), 0.0).sin_cos();
        with_rec!(vm, r, {
            r.state.m = mul(&r.state.m, &[cs, sn, -sn, cs, 0.0, 0.0]);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(scale), script_args!(sx = 1.0, sy = NIL), move |vm, a| {
        let sx = num(vm, a, id!(sx), 1.0);
        let sy = num(vm, a, id!(sy), sx);
        with_rec!(vm, r, {
            r.state.m = mul(&r.state.m, &[sx, 0.0, 0.0, sy, 0.0, 0.0]);
            NIL
        })
    });
    vm.add_method(obj, id_lut!(gradient), script_args!(pts = NIL, o = NIL, alpha = 1.0), move |vm, a| {
        let contours = contours_of(vm, get(vm, a, id!(pts)));
        let o = opt_obj(get(vm, a, id!(o)));
        let alpha = num(vm, a, id!(alpha), 1.0);
        let stops = o.and_then(|o| stops_of(vm, get(vm, o, id!(stops))));
        let radial = o.and_then(|o| nums_of(vm, get(vm, o, id!(radial))));
        let linear = o.and_then(|o| nums_of(vm, get(vm, o, id!(linear))));
        with_rec!(vm, r, {
            let (Some(contours), Some(stops)) = (contours, stops) else {
                r.warn("gradient(pts, {radial: [cx, cy, r0, r1] | linear: [x0, y0, x1, y1] stops: [[offset, colour], …]}, alpha)");
                return NIL;
            };
            let m = r.state.m;
            let paint = match (radial, linear) {
                (Some(g), _) if g.len() >= 3 => {
                    let (r0, r1) = if g.len() >= 4 { (g[2], g[3]) } else { (0.0, g[2]) };
                    // The radii along the transform's two axes.
                    let sx = (m[0] * m[0] + m[1] * m[1]).sqrt();
                    let sy = (m[2] * m[2] + m[3] * m[3]).sqrt();
                    Gradient::Radial { center: apply(&m, [g[0], g[1]]), r0: r0 * sx, r1: r1 * sx, ry: r1 * sy, stops }
                }
                (_, Some(g)) if g.len() >= 4 => Gradient::Linear { from: apply(&m, [g[0], g[1]]), to: apply(&m, [g[2], g[3]]), stops },
                _ => {
                    r.warn("gradient: give radial: [cx, cy, r0, r1] (or [cx, cy, r]) or linear: [x0, y0, x1, y1]");
                    return NIL;
                }
            };
            let contours = r.placed(&contours);
            let alpha = alpha * r.state.alpha;
            if !contours.is_empty() && alpha > 0.0 {
                r.list.ops.push(Op::Gradient { contours, paint, alpha: alpha.min(1.0) });
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(composite), script_args!(mode = NIL), move |vm, a| {
        let mode = get(vm, a, id!(mode)).as_id();
        with_rec!(vm, r, {
            r.state.erase = match mode {
                Some(m) if m == id!(destination_out) || m == id!(erase) => true,
                Some(m) if m == id!(source_over) || m == id!(normal) => false,
                _ => {
                    r.warn("composite(@source_over | @destination_out)");
                    r.state.erase
                }
            };
            NIL
        })
    });
    vm.add_method(obj, id_lut!(clip_begin), script_args!(pts = NIL), move |vm, a| {
        let contours = contours_of(vm, get(vm, a, id!(pts)));
        with_rec!(vm, r, {
            match contours {
                Some(c) => {
                    let contours = r.placed(&c);
                    r.list.ops.push(Op::ClipBegin { contours });
                    r.clips += 1;
                }
                None => r.warn("clip_begin(pts): pts is [[x, y], …] or a list of them"),
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(clip_end), script_args!(), move |vm, _a| {
        with_rec!(vm, r, {
            if r.clips > 0 {
                r.clips -= 1;
                r.list.ops.push(Op::ClipEnd);
            } else {
                r.warn("clip_end() without a clip_begin");
            }
            NIL
        })
    });
    vm.add_method(obj, id_lut!(alpha), script_args!(k = 1.0), move |vm, a| {
        let k = num(vm, a, id!(k), 1.0).clamp(0.0, 1.0);
        with_rec!(vm, r, {
            r.state.alpha = k;
            NIL
        })
    });
    obj.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_compose_as_canvas2d() {
        // translate then rotate then scale, applied to a point, as Canvas2D does.
        let t = [1.0, 0.0, 0.0, 1.0, 10.0, 20.0];
        let (s, c) = std::f64::consts::FRAC_PI_2.sin_cos();
        let r = [c, s, -s, c, 0.0, 0.0];
        let k = [2.0, 0.0, 0.0, 2.0, 0.0, 0.0];
        let m = mul(&mul(&t, &r), &k);
        let p = apply(&m, [1.0, 0.0]);
        assert!((p[0] - 10.0).abs() < 1e-9 && (p[1] - 22.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn fixed_prints_like_to_fixed() {
        // The values JavaScript prints (node).
        assert_eq!(to_fixed(4.605, 2), "4.61");
        assert_eq!(to_fixed(0.0, 2), "0.00");
        assert_eq!(to_fixed(1.5, 0), "2");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(-2.5, 0), "-3");
        assert_eq!(to_fixed(9.995, 2), "9.99");
        assert_eq!(to_fixed(99.5, 0), "100");
        assert_eq!(to_fixed(-0.001, 2), "-0.00");
        assert_eq!(to_fixed(-0.0, 2), "0.00");
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(f64::NAN, 2), "NaN");
    }

    #[test]
    fn circles_have_enough_sides() {
        assert_eq!(circle_pts(0.0, 0.0, 1.0, 1.0).len(), 8);
        let big = circle_pts(0.0, 0.0, 500.0, 1.0);
        assert!(big.len() > 100, "{}", big.len());
    }
}
