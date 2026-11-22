//! A circuit look lab with voltage colour, moving current and glowing parts.
//! Wheel to zoom about the pointer; drag to pan. Set ELEC_LIGHT=1 for a light canvas.

use fab_elec_view::live::{net_row, part_row, seg_row, FlowPhase, LiveClock, LiveLook, LiveRows, LIVE_ROW_W, PART_OFF, SEG_OFF};
use makepad_widgets_core::*;
use std::time::Instant;

app_main!(App);

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    use mod.elec_live.*

    mod.draw.DrawLiveCanvas = set_type_default() do #(DrawLiveCanvas::script_shader(vm)) {
        ..mod.draw.DrawQuad
        origin: uniform(vec2(0.0))
        scale: uniform(1.0)
        live_light: uniform(0.0)
        ink: uniform(theme.color_text)
        canvas: uniform(theme.color_bg_app)

        pixel: fn() {
            let dpi = self.draw_pass.dpi_factor
            let p = (self.pos * self.rect_size - self.origin) * dpi
            let fine_spacing = 4.0 * self.scale * dpi
            let coarse_spacing = 16.0 * self.scale * dpi
            let fine_distance = length(mod(p + vec2(fine_spacing * 0.5), vec2(fine_spacing)) - vec2(fine_spacing * 0.5))
            let coarse_distance = length(mod(p + vec2(coarse_spacing * 0.5), vec2(coarse_spacing)) - vec2(coarse_spacing * 0.5))
            let fine = clamp(0.9 - fine_distance + 0.5, 0.0, 1.0)
            let coarse = clamp(0.9 - coarse_distance + 0.5, 0.0, 1.0)
            let fine_fade = smoothstep(4.0, 8.0, fine_spacing)
            let coarse_fade = smoothstep(4.0, 8.0, coarse_spacing)
            let dots = mix(coarse * coarse_fade, fine, fine_fade)
            let alpha = dots * mix(0.06, 0.08, self.live_light) * self.ink.a
            let background_alpha = self.canvas.a * (1.0 - alpha)
            return vec4(self.ink.rgb * alpha + self.canvas.rgb * background_alpha, alpha + background_alpha)
        }
    }

    mod.draw.DrawLiveWire = set_type_default() do #(DrawLiveWire::script_shader(vm)) {
        ..mod.draw.DrawQuad
        width: 1.5
        live_time: uniform(0.0)
        live_light: uniform(0.0)
        px: uniform(1.0)
        ink: uniform(theme.color_text)
        accent: uniform(theme.color_focus)
        live_nets: texture_2d(float)
        live_segs: texture_2d(float)

        pixel: fn() {
            let p = self.pos * self.rect_size
            let delta = self.seg.zw - self.seg.xy
            let length_sq = dot(delta, delta)
            var along = 0.0
            if length_sq > 0.0 {
                along = clamp(dot(p - self.seg.xy, delta) / length_sq, 0.0, 1.0)
            }
            let distance = length(p - self.seg.xy - delta * along)
            let dpi = self.draw_pass.dpi_factor
            let radius = max(self.width * 0.5, 0.5 / dpi)
            let coverage = clamp((radius - distance) * dpi + 0.5, 0.0, 1.0)
            let net = self.live_nets.sample_nearest(Live.net_uv(self.live_net, self.live_nets.size()))
            let flow = self.live_segs.sample_nearest(Live.net_uv(self.live_seg, self.live_segs.size()))
            let s = self.along0 + along * self.seg_len
            let c = Live.net_color(net, self.ink.rgb, self.live_light, self.accent.rgb)
            let a = Live.flow(self.live_time, s, self.px, flow.x, flow.y)
            let g = a * (0.35 + 0.65 * flow.z)
            if self.live_light > 0.5 {
                return vec4(Live.lit(c, g, self.live_light) * coverage, coverage)
            }
            let core = c * (0.55 + 0.45 * g)
            let outside = (distance - radius) * dpi
            // Away from the core the dashes blur into their mean, so the light spreads along the wire as well as
            // across it instead of standing out as bars.
            var mean = 0.0
            if flow.y != 0.0 {
                mean = 0.5 * (0.35 + 0.65 * flow.z)
            }
            let glow = mix(mean, g, exp(0.0 - outside / 2.5))
            let halo = glow * exp(0.0-max(outside, 0.0)/(3.0+9.0*glow)) * (1.0-smoothstep(10.0, 14.0, outside))
            // Zero halo alpha lets overlapping wires add their light.
            return vec4(core * coverage + c * halo * (1.0 - coverage), coverage)
        }
    }

    mod.draw.DrawLivePart = set_type_default() do #(DrawLivePart::script_shader(vm)) {
        ..mod.draw.DrawQuad
        radius: 1.0
        live_parts: texture_2d(float)
        live_light: uniform(0.0)
        ink: uniform(theme.color_text)
        canvas: uniform(theme.color_bg_app)

        body_distance: fn() -> float {
            let p = self.pos * self.rect_size
            let half_size = self.body.zw * 0.5
            let q = abs(p - (self.body.xy + half_size)) - half_size + vec2(self.radius)
            return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - self.radius
        }

        pixel: fn() {
            let d = self.body_distance() * self.draw_pass.dpi_factor
            let coverage = clamp(0.5 - d, 0.0, 1.0)
            let part = self.live_parts.sample_nearest(Live.net_uv(self.live_part, self.live_parts.size()))
            let edge = Live.lit(self.ink.rgb, part.w * 0.7, self.live_light)
            let fill = if self.led_color.a > 0.5 {self.led_color.rgb * 0.12}
                else {mix(self.canvas.rgb, self.ink.rgb, mix(0.07, 0.12, self.live_light))}
            let rim = clamp(d + 1.7, 0.0, 1.0) * mix(0.35, 0.85, self.live_light)
            let hover = part.w * (1.0 - smoothstep(1.0, 4.0, d)) * 0.22 * (1.0 - coverage)
            return vec4(mix(fill, edge, rim)*coverage + edge*hover, coverage+hover)
        }
    }

    mod.draw.DrawLiveGlow = set_type_default() do #(DrawLiveGlow::script_shader(vm)) {
        ..mod.draw.DrawLivePart
        pixel: fn() {
            let part = self.live_parts.sample_nearest(Live.net_uv(self.live_part, self.live_parts.size()))
            let led = self.led_color.a > 0.5
            let g = if led {part.y} else {part.x}
            let ink = if led {self.led_color.rgb} else {Live.mag(g, self.live_light)}
            // The quad's inset is the world margin times the camera scale, just as d is.
            let as_led = if led {1.0} else {0.0}
            let a = Live.glow_alpha(Live.glow_peak(g, as_led), self.body_distance(), self.body.x)
            return vec4(ink*a, a)
        }
    }

    mod.widgets.LiveLabBase = #(LiveLab::register_widget(vm))
    mod.widgets.LiveLab = set_type_default() do mod.widgets.LiveLabBase {
        width: Fill height: Fill
        background: theme.color_bg_app
        dark_canvas: #07090d
        ink: theme.color_text
        muted: theme.color_text_meta
        draw_bg: mod.draw.DrawLiveCanvas {}
        draw_wire: mod.draw.DrawLiveWire {}
        draw_part: mod.draw.DrawLivePart {}
        draw_glow: mod.draw.DrawLiveGlow {}
        draw_text +: {color: theme.color_text_meta text_style: theme.font_code {font_size: 10}}
        a11y: {role: "figure" label: "Live circuit: eight PWM LED channels"}
    }

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1320, 900)
                window.title: "Live circuit"
                body +: {
                    lab := mod.widgets.LiveLab {}
                }
            }
        }
    }
}

/// A canvas with world-aligned dots in one quad.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawLiveCanvas {
    #[deref]
    draw_super: DrawQuad,
}

/// A wire capsule with static row indices and world arc length.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawLiveWire {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    seg: Vec4f,
    #[live]
    width: f32,
    #[live]
    live_net: f32,
    #[live]
    live_seg: f32,
    #[live]
    along0: f32,
    #[live]
    seg_len: f32,
}

/// The rounded part outline above the live light and wires.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawLivePart {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    body: Vec4f,
    #[live]
    radius: f32,
    #[live]
    live_part: f32,
    #[live]
    led_color: Vec4f,
}

impl DrawLivePart {
    /// Draw the part bounds with this pass's padding in logical points.
    fn draw_parts(&mut self, cx: &mut Cx2d, parts: &[Part], camera: &Camera, pad: f64) {
        let scale = camera.scale();
        self.begin_many_instances(cx);
        for (index, part) in parts.iter().enumerate() {
            let pos = camera.screen(part.body.pos);
            let size = part.body.size * scale;
            self.body = vec4(pad as f32, pad as f32, size.x as f32, size.y as f32);
            self.radius = (part.radius * scale) as f32;
            self.live_part = index as f32;
            self.led_color = part.color;
            self.draw_abs(
                cx,
                Rect {
                    pos: pos - dvec2(pad, pad),
                    size: size + dvec2(pad * 2.0, pad * 2.0),
                },
            );
        }
        self.end_many_instances(cx);
    }
}

/// A separate batch below every wire, sharing the part outline's geometry.
#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawLiveGlow {
    #[deref]
    draw_super: DrawLivePart,
}

const CHANNELS: usize = 8;
const VBUS: usize = 0;
const V3V3: usize = 1;
const GND: usize = 2;
const ANODES: usize = 3;
const CATHODES: usize = ANODES + CHANNELS;
const NETS: usize = CATHODES + CHANNELS;
const WORLD_W: f64 = 148.0;
const WORLD_H: f64 = 124.0;
/// Half of the lab canvas’s four-unit fine grid.
const GLOW_MARGIN: f64 = 2.0;
const WIRE_HALO_PX: f64 = 14.0;

#[derive(Clone, Copy)]
enum Current {
    Channel(usize),
    Tail(usize),
    Zero,
}

impl Current {
    fn amperes(self, currents: &[f32; CHANNELS]) -> f32 {
        match self {
            Self::Channel(i) => currents[i],
            Self::Tail(i) => currents[i..].iter().sum(),
            Self::Zero => 0.0,
        }
    }
}

struct Wire {
    net: usize,
    row: usize,
    a: DVec2,
    b: DVec2,
    along0: f32,
    length: f32,
    current: Current,
}

#[derive(Clone, Copy)]
enum PartKind {
    Connector,
    Regulator,
    Controller,
    Resistor(usize),
    Led(usize),
}

struct Part {
    body: Rect,
    radius: f64,
    color: Vec4f,
    kind: PartKind,
}

struct Label {
    pos: DVec2,
    text: String,
}

/// Hand-routed geometry in millimetres, kept unchanged during playback.
#[derive(Default)]
struct Scene {
    wires: Vec<Wire>,
    parts: Vec<Part>,
    labels: Vec<Label>,
}

impl Scene {
    fn new() -> Self {
        let mut scene = Self::default();
        let no_led = vec4(0.0, 0.0, 0.0, 0.0);
        for (kind, x, y, w, h) in [
            (PartKind::Connector, 8.0, 14.0, 12.0, 16.0),
            (PartKind::Regulator, 34.0, 12.0, 18.0, 12.0),
            (PartKind::Controller, 116.0, 27.0, 20.0, 82.0),
        ] {
            scene.parts.push(Part {
                body: Rect {
                    pos: dvec2(x, y),
                    size: dvec2(w, h),
                },
                radius: 1.5,
                color: no_led,
                kind,
            });
        }
        scene.route(
            VBUS,
            Current::Tail(0),
            &[(20.0, 18.0), (26.0, 18.0), (26.0, 16.0), (31.0, 16.0), (31.0, 18.0), (34.0, 18.0)],
        );
        scene.route(V3V3, Current::Tail(0), &[(52.0, 18.0), (63.0, 18.0), (63.0, 34.0)]);
        // Return current runs from the sinking pins' controller back to the input.
        scene.route(
            GND,
            Current::Tail(0),
            &[(126.0, 109.0), (126.0, 115.0), (43.0, 115.0), (14.0, 115.0), (14.0, 30.0)],
        );
        scene.route(GND, Current::Zero, &[(43.0, 24.0), (43.0, 115.0)]);
        let colors = [
            vec4(1.0, 0.08, 0.06, 1.0),
            vec4(1.0, 0.28, 0.04, 1.0),
            vec4(1.0, 0.55, 0.03, 1.0),
            vec4(0.95, 0.88, 0.05, 1.0),
            vec4(0.12, 1.0, 0.28, 1.0),
            vec4(0.05, 0.87, 1.0, 1.0),
            vec4(0.12, 0.32, 1.0, 1.0),
            vec4(0.64, 0.15, 1.0, 1.0),
        ];
        for (i, color) in colors.into_iter().enumerate() {
            let y = 34.0 + i as f64 * 9.0;
            if i > 0 {
                // Each T takes its branch current; the next rail section carries the remainder.
                scene.route(V3V3, Current::Tail(i), &[(63.0, y - 9.0), (63.0, y)]);
            }
            scene.route(V3V3, Current::Channel(i), &[(63.0, y), (72.0, y)]);
            scene.route(ANODES + i, Current::Channel(i), &[(82.0, y), (93.4, y)]);
            scene.route(
                CATHODES + i,
                Current::Channel(i),
                &[(98.6, y), (104.0, y), (104.0, y + 2.0), (110.0, y + 2.0), (110.0, y), (116.0, y)],
            );
            scene.parts.push(Part {
                body: Rect {
                    pos: dvec2(72.0, y - 1.8),
                    size: dvec2(10.0, 3.6),
                },
                radius: 0.9,
                color: no_led,
                kind: PartKind::Resistor(i),
            });
            scene.parts.push(Part {
                body: Rect {
                    pos: dvec2(93.4, y - 2.6),
                    size: dvec2(5.2, 5.2),
                },
                radius: 2.6,
                color,
                kind: PartKind::Led(i),
            });
            scene.label(73.0, y - 4.7, format!("R{}", i + 1));
            scene.label(94.0, y - 5.5, format!("D{}", i + 1));
            scene.label(110.5, y - 2.8, format!("K{}", i + 1));
        }
        for (x, y, text) in [
            (8.0, 10.0, "J1"),
            (8.0, 33.0, "5 V IN"),
            (22.0, 12.0, "VBUS"),
            (34.0, 8.0, "U1 / 3.3 V"),
            (54.0, 14.0, "3V3"),
            (56.0, 42.0, "3V3"),
            (119.0, 22.0, "U2 / PWM"),
            (117.0, 118.0, "GND"),
            (72.0, 103.0, "100 ohm"),
            (93.0, 103.0, "20 mA"),
        ] {
            scene.label(x, y, text.into());
        }
        scene
    }

    fn label(&mut self, x: f64, y: f64, text: String) {
        self.labels.push(Label { pos: dvec2(x, y), text });
    }

    fn route(&mut self, net: usize, current: Current, points: &[(f64, f64)]) {
        let mut along0 = 0.0;
        for pair in points.windows(2) {
            let a = dvec2(pair[0].0, pair[0].1);
            let b = dvec2(pair[1].0, pair[1].1);
            let length = (b - a).length() as f32;
            self.wires.push(Wire {
                net,
                row: self.wires.len(),
                a,
                b,
                along0,
                length,
                current,
            });
            along0 += length;
        }
    }

    fn hover(&self, p: DVec2, scale: f64) -> (Option<usize>, Option<usize>) {
        for (i, part) in self.parts.iter().enumerate().rev() {
            let half = part.body.size * 0.5;
            let rel = p - part.body.pos - half;
            let q = dvec2(rel.x.abs() - half.x + part.radius, rel.y.abs() - half.y + part.radius);
            let distance = dvec2(q.x.max(0.0), q.y.max(0.0)).length() + q.x.max(q.y).min(0.0) - part.radius;
            if distance <= 0.0 {
                return (None, Some(i));
            }
        }
        let mut closest = 6.0 / scale;
        let mut net = None;
        for wire in &self.wires {
            let delta = wire.b - wire.a;
            let rel = p - wire.a;
            let along = ((rel.x * delta.x + rel.y * delta.y) / (delta.x * delta.x + delta.y * delta.y)).clamp(0.0, 1.0);
            let distance = (rel - delta * along).length();
            if distance <= closest {
                closest = distance;
                net = Some(wire.net);
            }
        }
        (net, None)
    }
}

/// Counts the same complete texture rows that LiveRows uploads.
struct Rows {
    live: LiveRows,
    dirty: Option<(usize, usize)>,
    uploaded: bool,
}

impl Rows {
    fn new(mut live: LiveRows, count: usize) -> Self {
        live.resize(count);
        Self {
            live,
            dirty: None,
            uploaded: false,
        }
    }

    fn set(&mut self, index: usize, value: [f32; 4]) {
        if self.live.row(index) != value {
            self.live.set(index, value);
            self.dirty = Some(match self.dirty {
                Some((lo, hi)) => (lo.min(index), hi.max(index + 1)),
                None => (index, index + 1),
            });
        }
    }

    fn texture(&mut self, cx: &mut Cx, bytes: &mut u64) -> &Texture {
        let rows = if !self.uploaded {
            self.live.nets().div_ceil(LIVE_ROW_W).max(1)
        } else {
            self.dirty.map_or(0, |(lo, hi)| (hi - 1) / LIVE_ROW_W + 1 - lo / LIVE_ROW_W)
        };
        *bytes += (rows * LIVE_ROW_W * 4 * std::mem::size_of::<f32>()) as u64;
        self.dirty = None;
        self.uploaded = true;
        self.live.texture(cx)
    }
}

/// Supplies interval means at 30 Hz and keeps phases continuous between them.
struct LabHost {
    scene: Scene,
    look: LiveLook,
    light: bool,
    started: Instant,
    tick: Option<u64>,
    clock: LiveClock,
    phases: Vec<FlowPhase>,
    currents: [f32; CHANNELS],
    nets: Rows,
    segs: Rows,
    parts: Rows,
    frames: u32,
    draw_seconds: f64,
    report_at: Instant,
    bytes: [u64; 3],
}

impl LabHost {
    fn new() -> Self {
        let scene = Scene::new();
        let light = light_theme();
        let mut look = LiveLook::for_theme(light);
        look.v_range = (0.0, 5.0);
        look.i_ref = 0.050;
        let started = Instant::now();
        Self {
            nets: Rows::new(LiveRows::new(), NETS),
            segs: Rows::new(LiveRows::with_off(SEG_OFF), scene.wires.len()),
            parts: Rows::new(LiveRows::with_off(PART_OFF), scene.parts.len()),
            phases: vec![FlowPhase::default(); scene.wires.len()],
            scene,
            look,
            light,
            started,
            tick: None,
            clock: LiveClock::new(0.0),
            currents: [0.0; CHANNELS],
            frames: 0,
            draw_seconds: 0.0,
            report_at: started,
            bytes: [0; 3],
        }
    }

    fn update(&mut self, hover: (Option<usize>, Option<usize>)) -> f32 {
        let now = self.started.elapsed().as_secs_f64();
        let (live_time, rebased) = self.clock.time(now);
        let tick = (now * 30.0).floor() as u64;
        if self.tick != Some(tick) {
            self.tick = Some(tick);
            let duties: [f32; CHANNELS] =
                std::array::from_fn(|i| (0.5 + 0.45 * (std::f64::consts::TAU * (0.25 * now - i as f64 / CHANNELS as f64)).sin()) as f32);
            self.currents = duties.map(|d| d * 0.015);
            let sum: f32 = self.currents.iter().sum();
            for (index, v) in [(VBUS, 5.0), (V3V3, 3.3), (GND, 0.0)] {
                self.nets.set(index, net_row(Some(v), sum, emphasis(hover.0, index), &self.look));
            }
            for (i, duty) in duties.into_iter().enumerate() {
                let current = self.currents[i];
                self.nets.set(
                    ANODES + i,
                    net_row(Some(3.3 - current * 100.0), current, emphasis(hover.0, ANODES + i), &self.look),
                );
                self.nets.set(
                    CATHODES + i,
                    net_row(Some((1.0 - duty) * 1.4 + duty * 0.15), current, emphasis(hover.0, CATHODES + i), &self.look),
                );
            }
            for wire in &self.scene.wires {
                self.phases[wire.row].set(now, wire.current.amperes(&self.currents), &self.look);
            }
            for (index, part) in self.scene.parts.iter().enumerate() {
                let (power, led) = match part.kind {
                    PartKind::Connector => (0.0, None),
                    PartKind::Regulator => ((5.0 - 3.3) * sum, None),
                    PartKind::Controller => (0.020 + 0.15 * sum, None),
                    PartKind::Resistor(i) => (self.currents[i] * self.currents[i] * 100.0, None),
                    PartKind::Led(i) => (self.currents[i] * 1.9, Some((self.currents[i], 0.020))),
                };
                let rating = if matches!(part.kind, PartKind::Resistor(_)) { 0.25 } else { 0.5 };
                self.parts.set(index, part_row(power, rating, led, emphasis(hover.1, index)));
            }
            self.write_segments();
        } else if rebased {
            self.write_segments();
        }
        live_time
    }

    fn write_segments(&mut self) {
        for wire in &self.scene.wires {
            self.segs.set(
                wire.row,
                seg_row(&self.phases[wire.row], &self.clock, wire.current.amperes(&self.currents), &self.look),
            );
        }
    }

    fn report_frame(&mut self, seconds: f64) {
        self.frames += 1;
        self.draw_seconds += seconds;
        if self.frames == 120 {
            let elapsed = self.report_at.elapsed().as_secs_f64().max(f64::EPSILON);
            log!(
                "elec_live_lab: 120 frames, CPU draw mean {:.3} ms; uploads nets {:.0}, segments {:.0}, parts {:.0}, total {:.0} B/s",
                self.draw_seconds * 1000.0 / f64::from(self.frames),
                self.bytes[0] as f64 / elapsed,
                self.bytes[1] as f64 / elapsed,
                self.bytes[2] as f64 / elapsed,
                self.bytes.iter().sum::<u64>() as f64 / elapsed
            );
            self.frames = 0;
            self.draw_seconds = 0.0;
            self.bytes = [0; 3];
            self.report_at = Instant::now();
        }
    }
}

fn emphasis(hover: Option<usize>, index: usize) -> f32 {
    if hover == Some(index) {
        1.0
    } else {
        0.0
    }
}

fn light_theme() -> bool {
    std::env::var("ELEC_LIGHT").is_ok_and(|v| v == "1")
}

struct Camera {
    view: Rect,
    zoom: f64,
    pan: DVec2,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            view: Rect::default(),
            zoom: 1.0,
            pan: dvec2(0.0, 0.0),
        }
    }
}

impl Camera {
    fn scale(&self) -> f64 {
        ((self.view.size.x - 112.0).max(1.0) / WORLD_W).min((self.view.size.y - 150.0).max(1.0) / WORLD_H) * self.zoom
    }

    fn center(&self) -> DVec2 {
        self.view.pos + self.view.size * 0.5 + dvec2(0.0, 12.0)
    }

    fn screen(&self, world: DVec2) -> DVec2 {
        self.center() + (world - dvec2(WORLD_W * 0.5, WORLD_H * 0.5) + self.pan) * self.scale()
    }

    fn world(&self, screen: DVec2) -> DVec2 {
        (screen - self.center()) / self.scale() + dvec2(WORLD_W * 0.5, WORLD_H * 0.5) - self.pan
    }
}

/// The canvas owns navigation; only the shared textures carry live values.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct LiveLab {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_bg: DrawLiveCanvas,
    #[live]
    draw_wire: DrawLiveWire,
    #[live]
    draw_part: DrawLivePart,
    #[live]
    draw_glow: DrawLiveGlow,
    #[live]
    draw_text: DrawText,
    #[live]
    background: Vec4f,
    #[live]
    dark_canvas: Vec4f,
    #[live]
    ink: Vec4f,
    #[live]
    muted: Vec4f,
    #[rust]
    area: Area,
    #[rust]
    camera: Camera,
    #[rust]
    pointer: Option<DVec2>,
    #[rust]
    drag: Option<(DVec2, DVec2)>,
    #[rust(LabHost::new())]
    host: LabHost,
}

impl WidgetNode for LiveLab {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _p: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for LiveLab {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let started = Instant::now();
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.camera.view = view;
        cx.push_clip_rect(view);
        let scale = self.camera.scale();
        let dpi = cx.current_dpi_factor();
        let light = if self.host.light { 1.0 } else { 0.0 };
        let canvas = if self.host.light { self.background } else { self.dark_canvas };
        let origin = self.camera.screen(dvec2(0.0, 0.0)) - view.pos;
        let bg_vars = &mut self.draw_bg.draw_vars;
        bg_vars.set_uniform(cx, id!(origin), &[origin.x as f32, origin.y as f32]);
        bg_vars.set_uniform(cx, id!(scale), &[scale as f32]);
        bg_vars.set_uniform(cx, id!(live_light), &[light]);
        bg_vars.set_uniform(cx, id!(ink), &[self.ink.x, self.ink.y, self.ink.z, self.ink.w]);
        bg_vars.set_uniform(cx, id!(canvas), &[canvas.x, canvas.y, canvas.z, canvas.w]);
        self.draw_bg.draw_abs(cx, view);

        let hover = self
            .pointer
            .filter(|p| view.contains(*p))
            .map_or((None, None), |p| self.host.scene.hover(self.camera.world(p), scale));
        let live_time = self.host.update(hover);
        self.draw_wire.draw_vars.set_texture(0, self.host.nets.texture(cx, &mut self.host.bytes[0]));
        self.draw_wire.draw_vars.set_texture(1, self.host.segs.texture(cx, &mut self.host.bytes[1]));
        let parts = self.host.parts.texture(cx, &mut self.host.bytes[2]);
        self.draw_part.draw_vars.set_texture(0, parts);
        self.draw_glow.draw_vars.set_texture(0, parts);
        self.draw_glow.draw_vars.set_uniform(cx, id!(live_light), &[light]);
        let wire_vars = &mut self.draw_wire.draw_vars;
        wire_vars.set_uniform(cx, id!(live_time), &[live_time]);
        wire_vars.set_uniform(cx, id!(live_light), &[light]);
        wire_vars.set_uniform(cx, id!(px), &[(scale * dpi) as f32]);
        wire_vars.set_uniform(cx, id!(ink), &[self.ink.x, self.ink.y, self.ink.z, self.ink.w]);
        let part_vars = &mut self.draw_part.draw_vars;
        part_vars.set_uniform(cx, id!(live_light), &[light]);
        part_vars.set_uniform(cx, id!(ink), &[self.ink.x, self.ink.y, self.ink.z, self.ink.w]);
        part_vars.set_uniform(cx, id!(canvas), &[canvas.x, canvas.y, canvas.z, canvas.w]);

        self.draw_glow.draw_parts(cx, &self.host.scene.parts, &self.camera, GLOW_MARGIN * scale);

        let width = if self.host.light {
            (0.32 * scale).max(1.0 / dpi)
        } else {
            (0.45 * scale).max(1.5 / dpi)
        };
        let pad = width * 0.5 + if self.host.light { 2.0 } else { WIRE_HALO_PX } / dpi;
        self.draw_wire.begin_many_instances(cx);
        for wire in &self.host.scene.wires {
            let a = self.camera.screen(wire.a);
            let b = self.camera.screen(wire.b);
            let rect = Rect {
                pos: dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad),
                size: dvec2((b.x - a.x).abs() + pad * 2.0, (b.y - a.y).abs() + pad * 2.0),
            };
            let local_a = a - rect.pos;
            let local_b = b - rect.pos;
            self.draw_wire.seg = vec4(local_a.x as f32, local_a.y as f32, local_b.x as f32, local_b.y as f32);
            self.draw_wire.width = width as f32;
            self.draw_wire.live_net = wire.net as f32;
            self.draw_wire.live_seg = wire.row as f32;
            self.draw_wire.along0 = wire.along0;
            self.draw_wire.seg_len = wire.length;
            self.draw_wire.draw_abs(cx, rect);
        }
        self.draw_wire.end_many_instances(cx);

        self.draw_part.draw_parts(cx, &self.host.scene.parts, &self.camera, 4.0 / dpi);

        self.draw_text.color = self.muted;
        if !self.host.light {
            self.draw_text.color.w *= 0.7;
        }
        self.draw_text.font_scale = (scale / 6.0) as f32;
        for label in &self.host.scene.labels {
            self.draw_text.draw_abs(cx, self.camera.screen(label.pos), &label.text);
        }
        self.draw_text.font_scale = 1.5;
        self.draw_text.color = self.ink;
        self.draw_text.draw_abs(cx, view.pos + dvec2(28.0, 24.0), "Live circuit");
        self.draw_text.font_scale = 1.0;
        self.draw_text.color = self.muted;
        self.draw_text
            .draw_abs(cx, view.pos + dvec2(28.0, 52.0), "5 V to 3.3 V / eight breathing LED channels");
        self.draw_text.draw_abs(
            cx,
            view.pos + dvec2(28.0, (view.size.y - 28.0).max(0.0)),
            "Voltage: colour   Current: motion   Power: glow     Wheel: zoom   Drag: pan",
        );
        cx.pop_clip_rect();
        self.host.report_frame(started.elapsed().as_secs_f64());
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                self.pointer = Some(e.abs);
                cx.set_cursor(MouseCursor::Default);
            }
            Hit::FingerHoverOut(_) => self.pointer = None,
            Hit::FingerScroll(e) => {
                let anchor = self.camera.world(e.abs);
                self.camera.zoom = (self.camera.zoom * 1.15_f64.powf(-e.scroll.y / 40.0)).clamp(0.05, 64.0);
                let shift = self.camera.world(e.abs) - anchor;
                self.camera.pan += shift;
                self.pointer = Some(e.abs);
                self.drag = None;
                self.area.redraw(cx);
                cx.set_cursor(MouseCursor::Default);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                self.pointer = Some(e.abs);
                self.drag = Some((e.abs, self.camera.pan));
                cx.set_cursor(MouseCursor::Grabbing);
            }
            Hit::FingerMove(e) => {
                if let Some((start, pan)) = self.drag {
                    self.camera.pan = pan + (e.abs - start) / self.camera.scale();
                    self.pointer = Some(e.abs);
                    self.area.redraw(cx);
                    cx.set_cursor(MouseCursor::Grabbing);
                }
            }
            Hit::FingerUp(e) => {
                self.drag = None;
                self.pointer = e.is_over.then_some(e.abs);
                cx.set_cursor(MouseCursor::Default);
            }
            _ => {}
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        cx.new_next_frame();
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        set_base_theme(vm.cx_mut(), if light_theme() { BaseTheme::Light } else { BaseTheme::Dark });
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        if let Event::NextFrame(_) = event {
            self.ui.redraw(cx);
            cx.new_next_frame();
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
