//! The live overlay contract: how every view draws simulated values on the circuit itself
//! (ARCHITECTURE 5.20). Voltage is colour, current is light that moves, dissipation is glow. The
//! explorer, the board and the 3D view each own their instances; this module owns the mapping from
//! values to shared texture rows and the shader functions that read them, so current looks the
//! same on a wire, a track and a body in 3D.
//!
//! Every live value comes from four row textures, each a [`LiveRows`] with
//! `TextureFormat::VecRGBAf32`. Each texture row has [`LIVE_ROW_W`] = 256 texels; index `i` is at
//! texel `(i % 256, i / 256)`. Read with `Live.net_uv(i, size)` and `sample_nearest` in either
//! shader stage, never filtered. Changed texture rows are uploaded at most once a frame. The
//! shader uniforms are named `live_nets`, `live_segs`, `live_parts` and `live_pins`, each `texture_2d(float)`.
//!
//! A further row texture, `live_mini`, holds the rolling voltage strips described in [`mini`].
//! It is another [`LiveRows`], addressed by stable subscribed slots rather than net indices.
//!
//! Net indices are `ValueIndex::net(NetRef)`, as in `LiveValues::net_v`; part indices are
//! `ValueIndex::comp(CompRef)`, as in `LiveValues::part_p`; pin indices are `ValueIndex::pin(PinRef)`.
//! These indices hold for one `ValueIndex` (one lowering). Each segment has a row from the
//! host's segment-row allocator, keyed by
//! (tree owner, structural key). The owner is geometry (a sheet's wiring or a board's copper),
//! never a pane, so every pane of a unit reads the same rows. A row stays stable while its
//! segment exists: a changed tree gets rows for new segments, frees removed segments' rows and
//! moves nothing. A [`net::NetTree`] carries each segment's row. Instances store only static row
//! indices `live_net`, `live_seg`, `live_part` and `live_pin` as floats, plus `along0` for a flow stroke.
//!
//! Nets ([`net_row`]):
//!
//! | Lane | Meaning | From |
//! |---|---|---|
//! | x | voltage ramp position 0..1; -1: no value, the ink as it is | [`volt_x`] |
//! | y | volts, signed and raw | `LiveValues::net_v` |
//! | z | level of the current the net carries, 0..1 | [`current_level`] |
//! | w | emphasis 0..1, hovered or probed net: drawn in the view's focus accent (the board's net takes its colour, the sheet rims and tints it) | host |
//!
//! Segments ([`seg_row`]):
//!
//! | Lane | Meaning | From |
//! |---|---|---|
//! | x | flow phase in pattern cycles at the live clock's epoch, 0..1 | [`FlowPhase::attr`] |
//! | y | flow speed in pattern cycles a second, signed; magnitude 0 or 0.4..3.0 | [`FlowPhase::attr`] |
//! | z | level of the segment's current, 0..1 | [`current_level`] |
//! | w | reserved, 0 | host |
//!
//! Positive speed runs from the segment's first node to its second.
//!
//! Parts ([`part_row`]):
//!
//! | Lane | Meaning | From |
//! |---|---|---|
//! | x | glow 0..1 from dissipation relative to the part’s rating | [`glow_attr`] |
//! | y | LED light output 0..1, drawn in its own colour; 0 for anything else | [`emission_attr`] |
//! | z | dissipation in watts, signed and raw | `LiveValues::part_p` |
//! | w | emphasis 0..1, hovered or probed part | host |
//!
//! Pins ([`pin_row`]), in `live_pins` at `ValueIndex::pin(PinRef)`:
//!
//! | Lane | Meaning | From |
//! |---|---|---|
//! | x | mode: -1 none, 0 input, 1 input with pull-up, 2 output, 3 PWM, 4 peripheral | firmware via [`PinLiveMode`] |
//! | y | level: 0 low, 1 high, -1 unknown | firmware state at its instant |
//! | z | duty 0..1 for PWM, 0 otherwise | firmware timer |
//! | w | emphasis 0..1, hovered or probed pin | host's emphasis path, as for nets and parts |
//!
//! Only co-simulated pins are written; the rest hold [`PIN_OFF`]. A new lowering clears the
//! pin rows before rewriting the current state with its new indices. The firmware pass writes
//! x/y/z and preserves w. Pin marks sample these rows directly, with no relayout or instance
//! rewrite per publication. `Live.net_uv` also addresses pins: their row layout is identical.
//!
//! Flow uses `u = x + y * live_time` cycles, with `live_time` in seconds since the host's
//! [`LiveClock`] epoch. At each epoch move ([`LIVE_REBASE_S`]) the host writes every segment row
//! again. Dash coverage is `Live.flow(live_time, s, px, x, y)`, where `s = along0 + distance
//! along the stroke` in world units, in the segment's direction. Segments of a chain carry the
//! same current and so the same phase: oriented head to tail with a continuous `along0`, the
//! dashes run through corners; at a branch they may restart.
//!
//! The shader functions (`use mod.elec_live.*`, then `Live.name(..)`), each with its CPU twin here:
//! - `Live.flow(t, s, px, phase, speed) -> float`: the dash pattern's coverage at arc length `s`
//!   (world units, `along0` plus the distance along the stroke), `px` device pixels per world
//!   unit, `phase` and `speed` the segment row's x and y; the density follows from the speed. Dashes keep
//!   a legible spacing (16 to 34 device pixels) at every zoom: the pattern is anchored in world
//!   units at the nearest power-of-two period and cross-fades between octaves, so nothing slides or
//!   shimmers while zooming ([`flow_alpha`]).
//! - `Live.volt(x, light) -> vec3`, `Live.mag(x, light) -> vec3`, `Live.grade(x, light) -> vec3`:
//!   the voltage, magnitude and phosphor grading ramps at `x` in 0..1, display colour ([`ramp`]).
//! - `Live.lit(base, a, light) -> vec3`: a wire's ink lit by a dash of coverage `a` ([`lit`]).
//! - `Live.glow_peak(g, led) -> float`: a part underlay's opacity inside the part from its glow or
//!   an LED's light ([`glow_peak`]).
//! - `Live.glow_alpha(a, distance, margin) -> float`: that opacity at a signed distance from the
//!   part outline, with distance and margin in the same world units ([`glow_alpha`]).
//! - `Live.net_uv(i, size) -> vec2`: the coordinate of row `i`'s texel in any of the row
//!   textures of `size` texels; read it with `sample_nearest` in the vertex or the pixel stage.
//! - `Live.net_color(row, base, light, accent) -> vec3`: a net-indexed stroke's colour from its
//!   texel, mixed toward `accent` (the view's focus accent) by its emphasis ([`net_color`]).
//!
//! Who writes: the host, from `LiveValues`' interval means when a `LiveValues` arrives; while
//! scrubbing, from the trace store at the cursor's time. A freed segment row is written zero.
//! Views bind the textures and set `live_time` ([`LiveClock::time`]) and `live_light`
//! (1 on a light canvas, 0 on a dark one); nothing live goes into their draw lists.
//!
//! Ramps are designed in a perceptual space (OKLab) as five stops each, interpolated there and
//! converted to display colour; the CPU and the shaders share the stop tables below literally.

use fab_elec_design::{CursorSource, TimeCursor};
use makepad_widgets_core::*;

pub mod controls_strip;
pub mod edit_diff;
pub mod flags;
pub mod mini;
pub mod fw_steps;
pub mod listen;
pub mod net;
pub mod overview;
pub mod peek;
pub mod peek_scope;
pub mod probe;
pub mod segs;
pub mod sim;
pub mod strip;
pub mod tag;
pub mod transport;
pub mod views;

/// A colour ramp. Each role has a dark-canvas and a light-canvas variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RampId {
    /// Voltage on a dark canvas: blue at the low end through violet, red and orange to yellow.
    VoltDark = 0,
    /// Voltage on a light canvas: the same hues, darker, ending at amber.
    VoltLight = 1,
    /// Magnitude (current, heat) on a dark canvas: one warm hue, dim to near white.
    MagDark = 2,
    /// Magnitude on a light canvas: pale amber to deep red-orange.
    MagLight = 3,
    /// Phosphor intensity grading on a dark screen: deep blue, cyan, green, yellow, white-hot.
    PhosphorDark = 4,
    /// Phosphor intensity grading on a light screen: pale blue to deep red.
    PhosphorLight = 5,
}

/// The ramps' stops as (lightness, chroma, hue in degrees) in OKLab's polar form, at x = 0, 0.25,
/// 0.5, 0.75, 1. Literally the same numbers as in the shader functions of [`script_mod`].
const STOPS: [[[f32; 3]; 5]; 6] = [
    [[0.52, 0.14, 262.0], [0.58, 0.18, 300.0], [0.66, 0.21, 355.0], [0.79, 0.17, 52.0], [0.93, 0.15, 100.0]],
    [[0.56, 0.14, 258.0], [0.50, 0.17, 300.0], [0.52, 0.20, 352.0], [0.58, 0.18, 35.0], [0.66, 0.16, 68.0]],
    [[0.38, 0.07, 55.0], [0.55, 0.13, 52.0], [0.70, 0.16, 62.0], [0.84, 0.13, 80.0], [0.97, 0.05, 95.0]],
    [[0.88, 0.05, 75.0], [0.78, 0.11, 68.0], [0.66, 0.15, 55.0], [0.55, 0.17, 40.0], [0.45, 0.16, 30.0]],
    [[0.32, 0.11, 262.0], [0.55, 0.12, 215.0], [0.74, 0.18, 145.0], [0.88, 0.17, 100.0], [0.98, 0.04, 95.0]],
    [[0.86, 0.06, 250.0], [0.72, 0.11, 225.0], [0.62, 0.14, 165.0], [0.56, 0.16, 60.0], [0.42, 0.16, 28.0]],
];

/// How a view maps values to looks: which ramps, the voltage span, and the reference current
/// at which flow is full. Each part carries its own power rating.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveLook {
    pub volt_ramp: RampId,
    pub mag_ramp: RampId,
    /// Volts at the low and high ends of the voltage ramp.
    pub v_range: (f32, f32),
    /// Amperes of full flow (fastest, densest dashes).
    pub i_ref: f32,
}

impl LiveLook {
    /// The look on a dark canvas: 0 to 5 V, full flow at 50 mA.
    pub fn dark() -> LiveLook {
        LiveLook { volt_ramp: RampId::VoltDark, mag_ramp: RampId::MagDark, v_range: (0.0, 5.0), i_ref: 0.05 }
    }
    /// The same spans on a light canvas.
    pub fn light() -> LiveLook {
        LiveLook { volt_ramp: RampId::VoltLight, mag_ramp: RampId::MagLight, ..LiveLook::dark() }
    }
    /// The look for a canvas: `light` true on a light theme.
    pub fn for_theme(light: bool) -> LiveLook {
        if light {
            LiveLook::light()
        } else {
            LiveLook::dark()
        }
    }
}

/// Decades below the reference that flow spans (the reference / 1000 is the threshold).
const DECADES: f32 = 3.0;

/// A magnitude against its reference on the shared log scale: 0 at a thousandth of the reference
/// (and below, or not a number), 1 at the reference and above.
pub fn log_level(m: f32, reference: f32) -> f32 {
    if !(m > 0.0) || !(reference > 0.0) {
        return 0.0;
    }
    (1.0 + (m / reference).log10() / DECADES).clamp(0.0, 1.0)
}

/// Whether a magnitude is at or above the drawing threshold (a thousandth of the reference).
fn above_threshold(m: f32, reference: f32) -> bool {
    m.is_finite() && reference > 0.0 && m >= reference * 10f32.powf(-DECADES)
}

/// Flow for a current `i` (amperes, signed along the wire's arc length): `[speed, density]`, speed
/// in pattern cycles a second (its sign the direction), density 0..1 ([`flow_density`] of the
/// speed). Zero below the threshold.
pub fn flow_attr(i: f32, look: &LiveLook) -> [f32; 2] {
    let m = i.abs();
    if !above_threshold(m, look.i_ref) {
        return [0.0, 0.0];
    }
    let speed = i.signum() * (0.4 + 2.6 * log_level(m, look.i_ref));
    [speed, flow_density(speed)]
}

/// The dash density 0..1 a flow speed implies (0 for no flow): denser as the current grows.
pub fn flow_density(speed: f32) -> f32 {
    if speed == 0.0 || !speed.is_finite() {
        return 0.0;
    }
    ((speed.abs() - 0.4) / 2.6).clamp(0.05, 1.0)
}

/// Dissipation as a fraction of the part's rated watts, on a 1.3-decade log scale.
/// Off below 5% of the rating, full at and above the rating.
pub fn glow_attr(p: f32, p_rated: f32) -> f32 {
    if !p.is_finite() || !p_rated.is_finite() || p_rated <= 0.0 || p <= 0.05 * p_rated {
        return 0.0;
    }
    (1.0 + (p / p_rated).log10() / 1.3).clamp(0.0, 1.0)
}

/// A part's dissipation colour on the theme's ramp, with relative intensity as alpha.
pub fn glow_color(p: f32, p_rated: f32, look: &LiveLook) -> Vec4f {
    let g = glow_attr(p, p_rated);
    let c = ramp(look.mag_ramp, g);
    vec4(c.x, c.y, c.z, g)
}

/// The current level, 0..1 on the flow scale (0 below the threshold): a net or segment row's z.
pub fn current_level(i: f32, look: &LiveLook) -> f32 {
    let m = i.abs();
    if !above_threshold(m, look.i_ref) {
        return 0.0;
    }
    log_level(m, look.i_ref).max(0.05)
}

/// The least LED light output (a part row's y) that shows: below it an LED is dark. The brightest
/// lens draws a few thousand display whites at full output, and a display's darkest step is some
/// 3e-4 white, so this much light stays under a sixth of that step on any lens. A solver's leakage
/// through an LED that is off (1e-14 to 1e-12 of its rating, new in each step) is below it.
pub const LIGHT_SHOWN: f32 = 1e-8;

/// An LED's light output 0..1 for its forward current `i_f` against its rated current: a part row's
/// y, drawn in the LED's own colour (not the magnitude ramp). Linear in light, as an
/// LED's output is close to linear in its current; 0 under [`LIGHT_SHOWN`].
pub fn emission_attr(i_f: f32, i_rated: f32) -> f32 {
    if !(i_f > 0.0) || !(i_rated > 0.0) {
        return 0.0;
    }
    let y = (i_f / i_rated).min(1.0);
    if y < LIGHT_SHOWN { 0.0 } else { y }
}

/// Whether an LED's light going from `shown` to `y` (a part row's y) can show: by at least
/// [`LIGHT_SHOWN`], and by more than a thousandth of the light (a display's steps are about a
/// hundredth of the level they are at). A view that drew `shown` need not draw `y` otherwise.
pub fn light_moved(shown: f32, y: f32) -> bool {
    let change = (y - shown).abs();
    change >= LIGHT_SHOWN && change > 1e-3 * y.max(shown)
}

/// The voltage's place on the ramp, 0..1 (clamped; 0 for not a number).
pub fn volt_x(v: f32, look: &LiveLook) -> f32 {
    let (lo, hi) = look.v_range;
    let x = (v - lo) / (hi - lo);
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A voltage's display colour on the look's ramp, alpha 1, as the shaders draw it from a net row's x.
pub fn volt_color(v: f32, look: &LiveLook) -> Vec4f {
    ramp(look.volt_ramp, volt_x(v, look))
}

/// Texels per texture row: index `i` is texel `(i % LIVE_ROW_W, i / LIVE_ROW_W)` in every row texture.
pub const LIVE_ROW_W: usize = 256;

/// A net's texel while it has no value.
pub const NET_OFF: [f32; 4] = [-1.0, 0.0, 0.0, 0.0];

/// A segment's texel while it has no value.
pub const SEG_OFF: [f32; 4] = [0.0; 4];

/// A part's texel while it has no value.
pub const PART_OFF: [f32; 4] = [0.0; 4];

/// A pin's texel while it has no firmware state.
pub const PIN_OFF: [f32; 4] = [-1.0, -1.0, 0.0, 0.0];

/// The shared drawing modes; the writer maps the firmware's own pin modes onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinLiveMode {
    /// An input without a pull-up.
    Input,
    /// An input with its pull-up enabled.
    PullUp,
    /// A digital output.
    Output,
    /// A timer-driven output with a PWM duty.
    Pwm,
    /// A pin owned by a peripheral.
    Peripheral,
}

/// A pin's texel: x mode (-1 none, 0 input, 1 pull-up, 2 output, 3 PWM, 4 peripheral),
/// y level (0 low, 1 high, -1 unknown), z PWM duty and w emphasis, both clamped to 0..1.
/// Non-finite duty/emphasis become zero; non-PWM duty is zero. No mode gives [`PIN_OFF`]
/// with the clamped emphasis retained.
pub fn pin_row(mode: Option<PinLiveMode>, level: Option<bool>, duty: f32, emphasis: f32) -> [f32; 4] {
    let e = if emphasis.is_finite() { emphasis.clamp(0.0, 1.0) } else { 0.0 };
    let Some(mode) = mode else { return [PIN_OFF[0], PIN_OFF[1], PIN_OFF[2], e] };
    let x = match mode {
        PinLiveMode::Input => 0.0,
        PinLiveMode::PullUp => 1.0,
        PinLiveMode::Output => 2.0,
        PinLiveMode::Pwm => 3.0,
        PinLiveMode::Peripheral => 4.0,
    };
    let duty = if mode == PinLiveMode::Pwm && duty.is_finite() { duty.clamp(0.0, 1.0) } else { 0.0 };
    [x, level.map_or(-1.0, |high| if high { 1.0 } else { 0.0 }), duty, e]
}

/// A net's texel: its voltage's ramp position and the voltage (None: no value, x -1), the level of
/// the current it carries ([`current_level`] of its through current), and the host's emphasis 0..1
/// (the hovered or probed net).
pub fn net_row(v: Option<f32>, through_i: f32, emphasis: f32, look: &LiveLook) -> [f32; 4] {
    let e = if emphasis.is_finite() { emphasis.clamp(0.0, 1.0) } else { 0.0 };
    match v {
        Some(v) if v.is_finite() => [volt_x(v, look), v, current_level(through_i, look), e],
        _ => [-1.0, 0.0, 0.0, e],
    }
}

/// A segment's texel: phase at the clock's epoch, signed speed, current level and reserved zero.
pub fn seg_row(flow: &FlowPhase, clock: &LiveClock, i: f32, look: &LiveLook) -> [f32; 4] {
    let [phase, speed] = flow.attr(clock);
    [phase, speed, current_level(i, look), 0.0]
}

/// A part's texel: glow, LED light output, raw watts and emphasis. Invalid watts or emphasis are zero.
pub fn part_row(p: f32, p_rated: f32, led: Option<(f32, f32)>, emphasis: f32) -> [f32; 4] {
    let e = if emphasis.is_finite() { emphasis.clamp(0.0, 1.0) } else { 0.0 };
    [glow_attr(p, p_rated), led.map_or(0.0, |(i_f, i_rated)| emission_attr(i_f, i_rated)), if p.is_finite() { p } else { 0.0 }, e]
}

/// A net-indexed stroke's colour from its texel: the voltage's colour (`base`, the ink, without a
/// value), mixed toward `accent` by the emphasis, so a fully emphasised net (a hovered pin's) is
/// drawn in the view's focus accent, as the board draws what has its focus. A view that shows focus
/// around a stroke instead (the sheet's rim and tint) passes the row without its emphasis, so the
/// stroke keeps the voltage's colour. The CPU twin of `Live.net_color`.
pub fn net_color(row: [f32; 4], base: [f32; 3], light: bool, accent: [f32; 3]) -> [f32; 3] {
    let c = if row[0] < 0.0 {
        base
    } else {
        let v = ramp(if light { RampId::VoltLight } else { RampId::VoltDark }, row[0]);
        [v.x, v.y, v.z]
    };
    let w = row[3].clamp(0.0, 1.0);
    [0, 1, 2].map(|k| c[k] * (1.0 - w) + accent[k] * w)
}

/// Live values on the GPU: one RGBA float texel per net, segment, part or pin
/// ([`net_row`], [`seg_row`], [`part_row`], [`pin_row`]), [`LIVE_ROW_W`] per texture row. The host uses
/// `LiveValues`' interval means and firmware's instantaneous pin state. [`LiveRows::texture`]
/// uploads the texture rows that changed, at
/// most once a frame. Unchanged rows need no copy; each texel is 16 bytes.
/// New, dropped and cleared texels hold the kind's off texel: [`NET_OFF`] for nets ([`LiveRows::new`]),
/// [`SEG_OFF`], [`PART_OFF`] or [`PIN_OFF`] through [`LiveRows::with_off`].
pub struct LiveRows {
    /// RGBA per texel, whole texture rows.
    data: Vec<f32>,
    nets: usize,
    /// The texel of a row without a value.
    off: [f32; 4],
    texture: Option<Texture>,
    /// The texture's height in rows when it was made.
    rows: usize,
    /// Live rows set since the last upload: [lo, hi).
    dirty: Option<(usize, usize)>,
}

impl Default for LiveRows {
    fn default() -> LiveRows {
        LiveRows::new()
    }
}

impl LiveRows {
    /// Net rows, none yet: one texture row of [`NET_OFF`].
    pub fn new() -> LiveRows {
        LiveRows::with_off(NET_OFF)
    }

    /// Rows whose texel without a value is `off` ([`SEG_OFF`], [`PART_OFF`] or [`PIN_OFF`]).
    pub fn with_off(off: [f32; 4]) -> LiveRows {
        LiveRows { data: off.repeat(LIVE_ROW_W), nets: 0, off, texture: None, rows: 0, dirty: None }
    }

    /// Size for `count` live rows: new and dropped texels are off (their storage stays).
    pub fn resize(&mut self, count: usize) {
        let texels = count.div_ceil(LIVE_ROW_W).max(1) * LIVE_ROW_W;
        let had = self.data.len() / 4;
        if texels > had {
            self.data.extend(self.off.repeat(texels - had));
        }
        for i in count..self.nets {
            self.set_texel(i, self.off);
        }
        self.nets = count;
    }

    /// The number of live rows.
    pub fn nets(&self) -> usize {
        self.nets
    }

    /// Set row `i`'s texel; an index beyond [`LiveRows::nets`] is ignored.
    pub fn set(&mut self, i: usize, row: [f32; 4]) {
        debug_assert!(i < self.nets, "row {i} beyond the {} rows", self.nets);
        if i < self.nets {
            self.set_texel(i, row);
        }
    }

    /// Row `i`'s texel, or the off texel when out of bounds.
    pub fn row(&self, i: usize) -> [f32; 4] {
        if i >= self.nets {
            return self.off;
        }
        let d = &self.data[i * 4..i * 4 + 4];
        [d[0], d[1], d[2], d[3]]
    }

    /// Every row off (live stopped).
    pub fn clear(&mut self) {
        for i in 0..self.nets {
            self.set_texel(i, self.off);
        }
    }

    /// Free the texture and its retained bindings without uploading pending rows.
    /// The next `texture` call uploads the rows again; a second release does nothing.
    pub(crate) fn release(&mut self, cx: &mut Cx) {
        if let Some(texture) = self.texture.take() {
            cx.draw_lists.release_bindings(&[texture.texture_id()], &[]);
            texture.release(cx);
        }
        self.rows = 0;
    }

    fn set_texel(&mut self, i: usize, row: [f32; 4]) {
        let d = &mut self.data[i * 4..i * 4 + 4];
        if d == row {
            return;
        }
        d.copy_from_slice(&row);
        self.dirty = Some(match self.dirty {
            Some((lo, hi)) => (lo.min(i), hi.max(i + 1)),
            None => (i, i + 1),
        });
    }

    /// The texture to bind, with what changed since the last call uploaded (call it once a frame,
    /// where the frame binds it). Growing past the texture's capacity makes a new texture.
    pub fn texture(&mut self, cx: &mut Cx) -> &Texture {
        let rows = self.data.len() / 4 / LIVE_ROW_W;
        if self.texture.is_none() || self.rows != rows {
            let format = TextureFormat::VecRGBAf32 { width: LIVE_ROW_W, height: rows, data: Some(self.data.clone()), updated: TextureUpdated::Full };
            self.texture = Some(Texture::new_with_format(cx, format));
            self.rows = rows;
            self.dirty = None;
        } else if let Some((lo, hi)) = self.dirty.take() {
            let (r0, r1) = (lo / LIVE_ROW_W, (hi - 1) / LIVE_ROW_W + 1);
            let span = r0 * LIVE_ROW_W * 4..r1 * LIVE_ROW_W * 4;
            let texture = self.texture.as_ref().unwrap();
            let mut data = texture.take_vec_f32(cx);
            data[span.clone()].copy_from_slice(&self.data[span]);
            let rect = RectUsize { origin: PointUsize { x: 0, y: r0 }, size: SizeUsize { width: LIVE_ROW_W, height: r1 - r0 } };
            texture.put_back_vec_f32(cx, data, Some(rect));
        }
        self.texture.as_ref().unwrap()
    }
}

/// A ramp's display colour at `x` (clamped to 0..1), alpha 1.
pub fn ramp(id: RampId, x: f32) -> Vec4f {
    let s = &STOPS[id as usize];
    let u = if x.is_finite() { x.clamp(0.0, 1.0) } else { 0.0 } * 4.0;
    let k = (u.floor() as usize).min(3);
    let f = u - k as f32;
    let (a, b) = (lch_lab(s[k]), lch_lab(s[k + 1]));
    let lab = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f];
    let c = oklab_display(lab);
    vec4(c[0], c[1], c[2], 1.0)
}

fn lch_lab(c: [f32; 3]) -> [f32; 3] {
    let h = c[2] * 0.017453292;
    [c[0], c[1] * h.cos(), c[1] * h.sin()]
}

/// OKLab to linear sRGB (clamped to the gamut box), encoded as display colour (gamma 2.2, as every
/// shader writes it).
fn oklab_display(lab: [f32; 3]) -> [f32; 3] {
    let l_ = lab[0] + 0.3963377774 * lab[1] + 0.2158037573 * lab[2];
    let m_ = lab[0] - 0.1055613458 * lab[1] - 0.0638541728 * lab[2];
    let s_ = lab[0] - 0.0894841775 * lab[1] - 1.2914855480 * lab[2];
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    [r, g, b].map(|c| c.clamp(0.0, 1.0).powf(1.0 / 2.2))
}

/// The dash pattern's base period in device pixels for a density: sparse 34 px, dense 16 px.
fn flow_period_px(density: f32) -> f32 {
    34.0 - 18.0 * density
}

/// One dash of the pattern: coverage at `u` cycles, `aa` one device pixel in cycles.
fn dash(u: f32, aa: f32) -> f32 {
    let d = ((u - u.floor()) - 0.5).abs();
    1.0 - smoothstep(0.18 - aa, 0.18 + aa, d)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// CPU twin of `Live.flow`: the dash coverage at clock time `t`, arc length `s` (world units),
/// `px` device pixels per world unit, for a segment row's `phase` (at the epoch) and `speed`.
pub fn flow_alpha(t: f32, s: f32, px: f32, phase: f32, speed: f32) -> f32 {
    let density = flow_density(speed);
    if density <= 0.0 {
        return 0.0;
    }
    let lv = (flow_period_px(density) / px.max(1e-9)).log2();
    let k = lv.floor();
    let f = lv - k;
    let p0 = 2f32.powf(k);
    let u = phase + speed * t;
    let a0 = dash(s / p0 - u, 1.0 / (p0 * px));
    let a1 = dash(s / (2.0 * p0) - u, 1.0 / (2.0 * p0 * px));
    (a0 + (a1 - a0) * f) * (0.55 + 0.45 * density)
}

/// CPU twin of `Live.lit`: ink `base` lit by a dash of coverage `a` (towards white on a dark
/// canvas, towards deeper ink on a light one).
pub fn lit(base: [f32; 3], a: f32, light: bool) -> [f32; 3] {
    if light {
        base.map(|c| c + (c * 0.45 - c) * a)
    } else {
        base.map(|c| c + (1.0 - c) * a * 0.65)
    }
}

/// CPU twin of `Live.glow_peak`: the underlay's opacity inside a part from its row's glow (`x`)
/// or, for an LED, its light output (`y`). Dissipation is a cue that tints at most 0.35; an LED's
/// light is what the person reads, so it shows as seen (brightness about the square root of the
/// output), up to 0.8: a few mA of a 20 mA LED read as clearly lit.
pub fn glow_peak(g: f32, led: bool) -> f32 {
    let x = if g.is_finite() { g.clamp(0.0, 1.0) } else { 0.0 };
    if led { 0.8 * x.sqrt() } else { 0.35 * x }
}

/// CPU twin of `Live.glow_alpha`: the opacity `a` inside the part, fading to zero over the margin
/// outside it. Distance and margin are both in world units.
pub fn glow_alpha(a: f32, distance: f32, margin: f32) -> f32 {
    a.clamp(0.0, 1.0) * (1.0 - smoothstep(0.0, margin, distance))
}

/// Seconds after which a [`LiveClock`] moves its epoch, so shader time stays small enough for f32.
pub const LIVE_REBASE_S: f64 = 512.0;

/// The wall clock live shaders animate by: seconds since an epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveClock {
    epoch: f64,
}

impl LiveClock {
    /// A clock whose epoch is `now` (the frame's time in seconds).
    pub fn new(now: f64) -> LiveClock {
        LiveClock { epoch: now }
    }
    pub fn epoch(&self) -> f64 {
        self.epoch
    }
    /// The `live_time` uniform for `now`, and whether the epoch moved (then every segment row is
    /// written again from its [`FlowPhase`]).
    pub fn time(&mut self, now: f64) -> (f32, bool) {
        let moved = now - self.epoch > LIVE_REBASE_S;
        if moved {
            self.epoch = now;
        }
        ((now - self.epoch) as f32, moved)
    }
}

/// One wire's or track's flow: keeps the dash phase continuous when its current changes, so a new
/// value changes how the dashes move, never where they are.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FlowPhase {
    /// Pattern cycles at `t0`.
    phase: f64,
    /// Wall time of the last change, seconds.
    t0: f64,
    speed: f32,
}

impl FlowPhase {
    /// A new current `i` at wall time `now`: the phase reached so far is kept.
    pub fn set(&mut self, now: f64, i: f32, look: &LiveLook) {
        let [speed, _] = flow_attr(i, look);
        self.phase = (self.phase + self.speed as f64 * (now - self.t0)).rem_euclid(1.0);
        self.t0 = now;
        self.speed = speed;
    }
    /// The phase in cycles at wall time `now` (for tests and CPU drawing).
    pub fn phase_at(&self, now: f64) -> f64 {
        self.phase + self.speed as f64 * (now - self.t0)
    }
    /// A segment row's flow lanes `[phase at the clock's epoch, speed]`.
    pub fn attr(&self, clock: &LiveClock) -> [f32; 2] {
        let since = clock.epoch() - self.t0;
        [(self.phase + self.speed as f64 * since).rem_euclid(1.0) as f32, self.speed]
    }
}

/// A view's side of the shared time cursor (`TimeCursor`, broadcast like `Focus`): the cursor it
/// last heard, its own scrub while one runs, and the instant it draws in each frame. Every view
/// advances a playing cursor from the same broadcast by the same frame time, so all panes show the
/// same instant in the same frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeCursorView {
    heard: TimeCursor,
    /// Frame time at which `heard` arrived, seconds.
    heard_at: f64,
    /// This view's scrub: the cursor it broadcast and draws until released.
    scrub: Option<TimeCursor>,
}

impl TimeCursorView {
    /// A paused cursor at 0 on the simulation.
    pub fn new(now: f64) -> TimeCursorView {
        TimeCursorView::heard(TimeCursor { t: 0.0, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None }, now)
    }
    /// A view that has heard `c` at frame time `now`.
    pub fn heard(c: TimeCursor, now: f64) -> TimeCursorView {
        TimeCursorView { heard: c, heard_at: now, scrub: None }
    }
    /// A broadcast cursor arrived. While this view scrubs, its own scrub stays what it draws.
    pub fn hear(&mut self, c: TimeCursor, now: f64) {
        self.heard = c;
        self.heard_at = now;
    }
    /// The cursor as of frame time `now`: a playing cursor advanced by its rate.
    pub fn cursor(&self, now: f64) -> TimeCursor {
        if let Some(s) = self.scrub {
            return s;
        }
        let mut c = self.heard;
        // Unbounded playback follows publications; it has no wall-time extrapolation.
        if c.playing && c.rate.is_finite() {
            c.t += c.rate * (now - self.heard_at);
        }
        c
    }
    /// The instant this view draws at frame time `now`.
    pub fn at(&self, now: f64) -> f64 {
        self.cursor(now).t
    }
    pub fn scrubbing(&self) -> bool {
        self.scrub.is_some()
    }
    /// The user scrubs this view to `t`: playback pauses there. Returns the cursor to broadcast.
    pub fn scrub_to(&mut self, t: f64) -> TimeCursor {
        let c = TimeCursor { t, playing: false, ..self.heard };
        self.scrub = Some(c);
        c
    }
    /// The scrub ends: the view goes back to what it hears, which is its own last scrub until a
    /// newer broadcast arrives.
    pub fn release(&mut self, now: f64) {
        if let Some(s) = self.scrub.take() {
            self.heard = s;
            self.heard_at = now;
        }
    }
}

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader.*

    mod.elec_live = {
    Live: {
        fn l_lch_lab(c: vec3) -> vec3 {
            let h = c.z * 0.017453292
            return vec3(c.x, c.y * cos(h), c.y * sin(h))
        }

        fn l_oklab_display(lab: vec3) -> vec3 {
            let lq = lab.x + 0.3963377774 * lab.y + 0.2158037573 * lab.z
            let mq = lab.x - 0.1055613458 * lab.y - 0.0638541728 * lab.z
            let sq = lab.x - 0.0894841775 * lab.y - 1.2914855480 * lab.z
            let l = lq * lq * lq
            let m = mq * mq * mq
            let s = sq * sq * sq
            let r = clamp(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, 0.0, 1.0)
            let g = clamp(0.0 - 1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, 0.0, 1.0)
            let b = clamp(0.0 - 0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s, 0.0, 1.0)
            let k = 1.0 / 2.2
            return vec3(pow(r, k), pow(g, k), pow(b, k))
        }

        fn l_ramp5(x: float, s0: vec3, s1: vec3, s2: vec3, s3: vec3, s4: vec3) -> vec3 {
            let u = clamp(x, 0.0, 1.0) * 4.0
            let k = min(floor(u), 3.0)
            let f = u - k
            var a = s0
            var b = s1
            if k > 0.5 {
                a = s1
                b = s2
            }
            if k > 1.5 {
                a = s2
                b = s3
            }
            if k > 2.5 {
                a = s3
                b = s4
            }
            return l_oklab_display(mix(l_lch_lab(a), l_lch_lab(b), f))
        }

        fn l_dash(u: float, aa: float) -> float {
            let d = abs(fract(u) - 0.5)
            return 1.0 - smoothstep(0.18 - aa, 0.18 + aa, d)
        }

        fn l_volt(x: float, light: float) -> vec3 {
            if light > 0.5 {
                return l_ramp5(x, vec3(0.56, 0.14, 258.0), vec3(0.50, 0.17, 300.0), vec3(0.52, 0.20, 352.0), vec3(0.58, 0.18, 35.0), vec3(0.66, 0.16, 68.0))
            }
            return l_ramp5(x, vec3(0.52, 0.14, 262.0), vec3(0.58, 0.18, 300.0), vec3(0.66, 0.21, 355.0), vec3(0.79, 0.17, 52.0), vec3(0.93, 0.15, 100.0))
        }

        fn l_lit(base: vec3, a: float, light: float) -> vec3 {
            if light > 0.5 {
                return mix(base, base * 0.45, a)
            }
            return mix(base, vec3(1.0, 1.0, 1.0), a * 0.65)
        }

        // Voltage ramp (RampId::VoltDark / VoltLight).
        volt: fn(x: float, light: float) -> vec3 {
            return l_volt(x, light)
        }

        // Magnitude ramp: current, heat (RampId::MagDark / MagLight).
        fn l_mag(x: float, light: float) -> vec3 {
            if light > 0.5 {
                return l_ramp5(x, vec3(0.88, 0.05, 75.0), vec3(0.78, 0.11, 68.0), vec3(0.66, 0.15, 55.0), vec3(0.55, 0.17, 40.0), vec3(0.45, 0.16, 30.0))
            }
            return l_ramp5(x, vec3(0.38, 0.07, 55.0), vec3(0.55, 0.13, 52.0), vec3(0.70, 0.16, 62.0), vec3(0.84, 0.13, 80.0), vec3(0.97, 0.05, 95.0))
        }

        mag: fn(x: float, light: float) -> vec3 {
            return l_mag(x, light)
        }

        // Phosphor intensity grading (RampId::PhosphorDark / PhosphorLight).
        grade: fn(x: float, light: float) -> vec3 {
            if light > 0.5 {
                return l_ramp5(x, vec3(0.86, 0.06, 250.0), vec3(0.72, 0.11, 225.0), vec3(0.62, 0.14, 165.0), vec3(0.56, 0.16, 60.0), vec3(0.42, 0.16, 28.0))
            }
            return l_ramp5(x, vec3(0.32, 0.11, 262.0), vec3(0.55, 0.12, 215.0), vec3(0.74, 0.18, 145.0), vec3(0.88, 0.17, 100.0), vec3(0.98, 0.04, 95.0))
        }

        // Dash coverage at clock time t, arc length s (world units), px device pixels per world
        // unit, the segment row's phase (at the epoch) and speed (cycles a second, signed).
        flow: fn(t: float, s: float, px: float, phase: float, speed: float) -> float {
            if speed == 0.0 {
                return 0.0
            }
            let density = clamp((abs(speed) - 0.4) / 2.6, 0.05, 1.0)
            let lv = log2((34.0 - 18.0 * density) / max(px, 0.000000001))
            let k = floor(lv)
            let f = lv - k
            let p0 = pow(2.0, k)
            let u = phase + speed * t
            let a0 = l_dash(s / p0 - u, 1.0 / (p0 * px))
            let a1 = l_dash(s / (2.0 * p0) - u, 1.0 / (2.0 * p0 * px))
            return mix(a0, a1, f) * (0.55 + 0.45 * density)
        }

        // Ink lit by a dash of coverage a.
        lit: fn(base: vec3, a: float, light: float) -> vec3 {
            return l_lit(base, a, light)
        }

        // The texel coordinate for any LiveRows texture, including pins (256 texels a row).
        net_uv: fn(net: float, size: vec2) -> vec2 {
            let i = floor(net + 0.5)
            let y = floor(i / 256.0)
            let x = i - y * 256.0
            return vec2((x + 0.5) / size.x, (y + 0.5) / size.y)
        }

        // A net-indexed stroke's colour from its texel: the voltage's colour (base without a value),
        // mixed toward the view's focus accent by the emphasis.
        net_color: fn(row: vec4, base: vec3, light: float, accent: vec3) -> vec3 {
            var c = base
            if row.x >= 0.0 {
                c = l_volt(row.x, light)
            }
            return mix(c, accent, clamp(row.w, 0.0, 1.0))
        }

        // The underlay's opacity inside the part ([`glow_peak`]): a dissipation cue tints at most
        // 0.35; an LED's light reads as seen (a square root of its output), up to 0.8.
        fn l_glow_peak(g: float, led: float) -> float {
            let x = clamp(g, 0.0, 1.0)
            if led > 0.5 {
                return 0.8*sqrt(x)
            }
            return 0.35*x
        }

        glow_peak: fn(g: float, led: float) -> float {
            return l_glow_peak(g, led)
        }

        // Straight display colour and intensity for a part underlay. Emitter colours are
        // packed display RGB. An emitting part uses its own light-output channel.
        part_glow: fn(part: vec2, packed: float, light: float) -> vec4 {
            if packed > 0.0 {
                let r = floor(packed/65536.0)
                let g = floor((packed-r*65536.0)/256.0)
                return vec4(vec3(r, g, packed-r*65536.0-g*256.0)/255.0, l_glow_peak(part.y, 1.0))
            }
            if part.x <= 0.0 { return vec4(0.0) }
            return vec4(l_mag(part.x, light), l_glow_peak(part.x, 0.0))
        }

        // The opacity `a` inside the outline, falling to zero over the margin outside it; the
        // signed distance and the margin are in world units.
        glow_alpha: fn(a: float, distance: float, margin: float) -> float {
            return clamp(a, 0.0, 1.0) * (1.0 - smoothstep(0.0, margin, distance))
        }
    }
    }
}
