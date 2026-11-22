//! The Fab electronics canvas base's living test scene: lines, boxes and arcs in all eight
//! looks, plus voltage, signed flow and glow read from live row textures the scene writes as the
//! host would. Compare dark/light frames at pixel ratios 1 and 2.
//! Click to focus and fly in; drag an item to preview its move over a ghost of its old position.
//! Escape cancels a drag, otherwise clears focus and fits the scene. F fits, T changes theme,
//! and M assembles the scene from its centre. Wheel/pinch zoom; space-drag, middle/right drag
//! and two-finger scrolling pan, using the same input grammar as the schematic and board.
//! B flies two boxes out/back with a travel bridge, then cross-fades to their orthogonal wire.
//! S toggles selection on every third item id, G ghosts items 1 and 2, V cycles item 0's
//! violation level, E emphasises odd item ids, and D dims every fourth item id in this pane.
//! These keys change only the host's state texels; the scene's instances stay retained.
//! The strip row shows rolling sine, square, DC, and an empty strip; hover a strip to see its shadow on the others.
//! The pin row shows mode and level marks, fixed PWM and a four-second duty sweep; E emphasises all pin rows.

pub use makepad_widgets;

use fab_elec_view::canvas::{
    draw::{
        ArcInst, ArcTemplate, BoxInst, BoxTemplate, BridgeInst, BridgeTemplate, CanvasLayer,
        CanvasLive, CanvasUniforms, Common, DrawCanvasArc, DrawCanvasBox, DrawCanvasBridge,
        DrawCanvasLine, DrawCanvasMini, DrawCanvasPin, LineInst, LineTemplate, MiniInst, MiniTemplate,
        PinInst, PinTemplate,
    },
    looks::{ease_out, MOVE_SECONDS, REVEAL_SECONDS},
    Camera2d, CanvasInput, CanvasItems, CanvasNode, CanvasPane, Ladder, Look, Rung, Tokens,
};
use fab_elec_view::live::{pin_row, LiveRows, PinLiveMode, PIN_OFF};
use makepad_widgets::makepad_platform::a11y_tree;
use makepad_widgets::*;
use std::f32::consts::{PI, TAU};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*

    let CanvasBarBase = #(CanvasBar::register_widget(vm))
    let CanvasBar = set_type_default() do CanvasBarBase{
        width: Fill
        height: Fill
        draw_text +: {
            text_style: theme.font_regular{font_size: 11.0}
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Canvas bar"
                window.inner_size: vec2(1200, 800)
                body +: {
                    canvas := CanvasBar{width: Fill height: Fill}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::canvas::draw::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}

const LOOKS: [(Look, &str); 8] = [
    (Look::Committed, "Committed"),
    (Look::Focus, "Focus"),
    (Look::Preview, "Preview"),
    (Look::Ghost, "Ghost"),
    (Look::Stale, "Stale"),
    (Look::Violation, "Violation"),
    (Look::Disabled, "Disabled"),
    (Look::Proposal, "Proposal"),
];

#[derive(Clone, Copy)]
enum Shape {
    Wire,
    Dashed,
    Bus,
    Elbow,
    Part,
    Block,
    Arc,
    Junction,
    Mini(usize),
    Pin(usize),
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Self::Wire => "wire",
            Self::Dashed => "dashed wire",
            Self::Bus => "bus",
            Self::Elbow => "L wire",
            Self::Part => "part body",
            Self::Block => "block frame",
            Self::Arc => "arc",
            Self::Junction => "junction dot",
            Self::Mini(_) => "mini trace",
            Self::Pin(_) => "pin mark",
        }
    }

    /// The retained layers draw lines, boxes, strips, pins, then arcs. Picking uses this same order.
    fn layer(self) -> u8 {
        match self {
            Self::Wire | Self::Dashed | Self::Bus | Self::Elbow => 0,
            Self::Part | Self::Block => 1,
            Self::Mini(_) | Self::Pin(_) => 2,
            Self::Arc | Self::Junction => 3,
        }
    }

    fn bounds(self, center: [f64; 2]) -> [f64; 4] {
        let [hx, hy] = match self {
            Self::Wire | Self::Dashed | Self::Bus => [4.0, 0.0],
            Self::Elbow => [3.0, 2.0],
            Self::Part => [3.0, 1.5],
            Self::Block => [4.5, 3.5],
            Self::Arc => [2.5, 2.5],
            Self::Junction => [0.4, 0.4],
            Self::Mini(_) => [3.0, 0.75],
            Self::Pin(_) => return [center[0] - 0.25, center[1] - 0.5, center[0] + 1.8, center[1] + 0.5],
        };
        [center[0] - hx, center[1] - hy, center[0] + hx, center[1] + hy]
    }

    fn local_center(self) -> [f64; 2] {
        // Keep the L's specified local endpoints (0,0)-(6,0)-(6,-4), centred in its column.
        match self {
            Self::Elbow => [3.0, -2.0],
            _ => [0.0, 0.0],
        }
    }

    fn pose(self, center: [f64; 2]) -> [f32; 3] {
        let offset = self.local_center();
        [(center[0] - offset[0]) as f32, (center[1] - offset[1]) as f32, 0.0]
    }

    fn ink(self, tokens: &Tokens) -> [f32; 4] {
        match self {
            Self::Bus => tokens.wire_bus,
            Self::Part | Self::Junction | Self::Mini(_) | Self::Pin(_) => tokens.ink,
            Self::Block => tokens.block_frame,
            _ => tokens.wire_signal,
        }
    }

    fn append(self, templates: &Templates, rows: &mut Rows, c: Common, tokens: &Tokens) {
        match self {
            Self::Wire | Self::Dashed | Self::Bus | Self::Elbow => {
                let mut line = LineInst {
                    c,
                    seg: [-4.0, 0.0, 4.0, 0.0],
                    width: [1.5, 0.0],
                    ..LineInst::default()
                };
                match self {
                    Self::Dashed => line.dash = [6.0, 4.0],
                    Self::Bus => {
                        line.width = [3.0, 0.0];
                        line.cap = 1.0;
                    }
                    Self::Elbow => line.seg = [0.0, 0.0, 6.0, 0.0],
                    _ => {}
                }
                templates.line.append(&mut rows.lines, &line);
                if matches!(self, Self::Elbow) {
                    line.seg = [6.0, 0.0, 6.0, -4.0];
                    line.along0 = 6.0;
                    templates.line.append(&mut rows.lines, &line);
                }
            }
            Self::Part | Self::Block => {
                let block = matches!(self, Self::Block);
                templates.boxes.append(&mut rows.boxes, &BoxInst {
                    c,
                    rect: if block { [0.0, 0.0, 4.5, 3.5] } else { [0.0, 0.0, 3.0, 1.5] },
                    corner: if block { 1.0 } else { 0.4 },
                    stroke: [if block { 1.0 } else { 1.5 }, 0.0],
                    fill: if block { tokens.block_fill } else { tokens.part_fill },
                });
            }
            Self::Mini(slot) => {
                let mut fill = tokens.block_fill;
                fill[3] = 0.08;
                templates.mini.append(&mut rows.minis, &MiniInst {
                    c,
                    rect: [0.0, 0.0, 3.0, 0.75],
                    slot: slot as f32,
                    stroke: [1.25, 0.0],
                    fill,
                });
            }
            Self::Pin(slot) => {
                templates.line.append(&mut rows.lines, &LineInst {
                    c,
                    seg: [0.0, 0.0, 1.0, 0.0],
                    width: [1.5, 0.0],
                    ..LineInst::default()
                });
                templates.pin.append(&mut rows.pins, &PinInst {
                    c,
                    at: [1.0, 0.0], dir: [1.0, 0.0], size: 0.22, live_pin: slot as f32,
                });
            }
            Self::Arc | Self::Junction => {
                let dot = matches!(self, Self::Junction);
                templates.arc.append(&mut rows.arcs, &ArcInst {
                    c,
                    circle: [0.0, 0.0, if dot { 0.4 } else { 2.5 }, 0.0],
                    sweep: [0.0, if dot { TAU } else { 1.5 * PI }],
                    stroke: [if dot { 0.0 } else { 1.5 }, 0.0],
                    fill: if dot { tokens.ink } else { [0.0; 4] },
                    ..ArcInst::default()
                });
            }
        }
    }
}

struct Item {
    id: u32,
    shape: Shape,
    look: Look,
    name: String,
    /// The committed destination; while dragging, the proposed destination.
    home: [f64; 2],
    /// The presented centre, also used for picking and semantic nodes during assembly.
    shown: [f64; 2],
    /// Static live indices (net, wire segment, part); the values sit in the host's rows.
    live_ix: [f32; 4],
    /// The flow speed its segment row carries (0: none), for frame requests.
    flow: f32,
}

impl Item {
    fn new(id: u32, shape: Shape, look: Look, name: String, home: [f64; 2]) -> Self {
        Self { id, shape, look, name, home, shown: home, live_ix: [-1.0, -1.0, -1.0, 0.0], flow: 0.0 }
    }

    fn bounds(&self) -> [f64; 4] {
        self.shape.bounds(self.shown)
    }

    fn common(&self, at: [f64; 2], look: Look, motion: Option<Motion>, tokens: &Tokens) -> Common {
        let mut c = Common::at(self.shape.pose(at), self.shape.ink(tokens));
        c.item = self.id as f32;
        c.look = look.code();
        c.live_ix = self.live_ix;
        if let Some(motion) = motion {
            c.pose_a = [motion.center[0] as f32, motion.center[1] as f32, 0.0, 0.0];
            c.span = [motion.start as f32, REVEAL_SECONDS as f32];
        }
        c
    }
}

/// A local wrapper lets the scene implement the shared picking/semantics contract.
struct Scene(Vec<Item>);

/// The rows a simulation host would write: nets (a voltage ramp position each), wire segments
/// (flow phase and speed), parts (glow), mini traces, pins, and item/pane states. The mini traces
/// and PWM sweep advance each frame; the other values are written once and keys change emphasis/state.
/// Textures upload on first use and after a row changes.
struct LiveHost {
    nets: LiveRows,
    segs: LiveRows,
    parts: LiveRows,
    items: LiveRows,
    pane: LiveRows,
    mini: LiveRows,
    pins: LiveRows,
}

impl Default for LiveHost {
    fn default() -> Self {
        let (mut nets, mut segs, mut parts) = (LiveRows::new(), LiveRows::new(), LiveRows::new());
        nets.resize(VOLTS.len());
        for (i, level) in VOLTS.into_iter().enumerate() {
            nets.set(i, [level, 0.0, 0.0, 0.0]);
        }
        segs.resize(FLOWS.len());
        for (i, speed) in FLOWS.into_iter().enumerate() {
            segs.set(i, [0.0, speed, 0.0, 0.0]);
        }
        parts.resize(GLOWS.len());
        for (i, glow) in GLOWS.into_iter().enumerate() {
            parts.set(i, [glow, 0.0, 0.0, 0.0]);
        }
        let mut mini = LiveRows::with_off([1.0, -1.0, 0.0, -1.0]);
        mini.resize(4 * 64);
        let mut pins = LiveRows::with_off(PIN_OFF);
        pins.resize(PINS.len());
        for (slot, (_, mode, level, duty)) in PINS.into_iter().enumerate() {
            pins.set(slot, pin_row(Some(mode), level, duty, 0.0));
        }
        Self {
            nets, segs, parts,
            items: LiveRows::with_off([0.0; 4]),
            pane: LiveRows::with_off([0.0; 4]),
            mini,
            pins,
        }
    }
}

impl LiveHost {
    /// A two-second rolling window, aligned to absolute column boundaries as a host would write it.
    fn advance_mini(&mut self, now: f64) {
        let dt = 2.0 / 48.0;
        let k_open = (now / dt).floor() as i64;
        let frac = (now / dt - k_open as f64) as f32;
        for slot in 0..3 {
            let (scale_lo, scale_hi) = if slot == 2 { (1.2 - 0.033, 1.2 + 0.033) } else { (-0.06, 3.3 + 0.06) };
            self.mini.set(slot * 64, [scale_lo, scale_hi, k_open.rem_euclid(63) as f32, frac]);
            for k in k_open - 48..=k_open {
                let start = k as f64 * dt;
                let end = if k == k_open { now } else { (k + 1) as f64 * dt };
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                let mut sum = 0.0;
                for sample in 0..9 {
                    // Use the exact endpoint so neighbouring columns share their boundary value.
                    let t = if sample == 8 { end } else { start + (end - start) * sample as f64 / 8.0 };
                    let v = match slot {
                        0 => 1.65 + 1.45 * (std::f64::consts::TAU * 0.7 * t).sin(),
                        1 => if (1.3 * t).rem_euclid(1.0) < 0.5 { 3.3 } else { 0.0 },
                        _ => 1.2,
                    };
                    lo = lo.min(v);
                    hi = hi.max(v);
                    sum += v;
                }
                let mean = (sum / 9.0) as f32;
                // LiveRows skips unchanged texels; slot 3 stays entirely empty.
                self.mini.set(slot * 64 + 1 + k.rem_euclid(63) as usize, [lo as f32, hi as f32, mean, mean / 3.3]);
            }
        }
    }

    fn advance_pins(&mut self, now: f64) {
        let emphasis = self.pins.row(PIN_SWEEP)[3];
        self.pins.set(PIN_SWEEP, pin_row(Some(PinLiveMode::Pwm), Some(true), (now * 0.25).fract() as f32, emphasis));
    }

    /// Item ids are sparse; keep the gaps and texture padding at zero too.
    fn size_states(&mut self, scene: &Scene) {
        let count = scene.0.iter().map(|item| item.id as usize + 1).max().unwrap_or(0);
        self.items.resize(count);
        self.pane.resize(count);
    }

    fn change_states(&mut self, scene: &Scene, code: KeyCode) -> bool {
        let (channel, what) = match code {
            KeyCode::KeyS => (0, "selected toggled"),
            KeyCode::KeyG => (1, "ghost toggled"),
            KeyCode::KeyV => (2, "violation cycled"),
            KeyCode::KeyE => (3, "emphasis toggled"),
            KeyCode::KeyD => (0, "pane dim toggled"),
            _ => return false,
        };
        self.size_states(scene);
        let rows = if code == KeyCode::KeyD { &mut self.pane } else { &mut self.items };
        let mut changed = 0;
        for item in &scene.0 {
            let i = item.id as usize;
            let applies = match code {
                KeyCode::KeyS => i % 3 == 0,
                KeyCode::KeyG => i == 1 || i == 2,
                KeyCode::KeyV => i == 0,
                KeyCode::KeyE => i % 2 == 1,
                KeyCode::KeyD => i % 4 == 0,
                _ => false,
            };
            if !applies {
                continue;
            }
            let mut row = rows.row(i);
            row[channel] = if code == KeyCode::KeyV {
                (row[channel] + 1.0) % 4.0
            } else if row[channel] > 0.5 {
                0.0
            } else {
                1.0
            };
            rows.set(i, row);
            changed += 1;
        }
        if code == KeyCode::KeyE {
            let emphasis = if self.pins.row(0)[3] > 0.5 { 0.0 } else { 1.0 };
            for (slot, (_, mode, level, _)) in PINS.into_iter().enumerate() {
                let duty = self.pins.row(slot)[2];
                self.pins.set(slot, pin_row(Some(mode), level, duty, emphasis));
            }
        }
        log!("canvasbar: item states {what} ({changed} texels)");
        true
    }
}

const VOLTS: [f32; 9] = [0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875, 1.0];
const FLOWS: [f32; 8] = [-2.0, -1.0, -0.5, 0.5, 1.0, 2.0, 4.0, 1.0];
const GLOWS: [f32; 5] = [0.1, 0.25, 0.5, 0.75, 1.0];
const PIN_SWEEP: usize = 6;
const PINS: [(&str, PinLiveMode, Option<bool>, f32); 9] = [
    ("input low", PinLiveMode::Input, Some(false), 0.0),
    ("input high", PinLiveMode::Input, Some(true), 0.0),
    ("pull-up high", PinLiveMode::PullUp, Some(true), 0.0),
    ("output low", PinLiveMode::Output, Some(false), 0.0),
    ("output high", PinLiveMode::Output, Some(true), 0.0),
    ("PWM 25%", PinLiveMode::Pwm, Some(true), 0.25),
    ("PWM sweep", PinLiveMode::Pwm, Some(true), 0.0),
    ("peripheral", PinLiveMode::Peripheral, Some(true), 0.0),
    ("unknown", PinLiveMode::Input, None, 0.0),
];

impl Default for Scene {
    fn default() -> Self {
        let shapes = [Shape::Wire, Shape::Dashed, Shape::Bus, Shape::Elbow,
            Shape::Part, Shape::Block, Shape::Arc, Shape::Junction];
        let mut items = Vec::new();
        for (row, (look, name)) in LOOKS.into_iter().enumerate() {
            for (column, shape) in shapes.into_iter().enumerate() {
                items.push(Item::new(
                    (row * 16 + column) as u32, shape, look,
                    format!("{name} {}", shape.name()),
                    [10.0 + 12.0 * column as f64, -10.0 * row as f64],
                ));
            }
        }
        let mut next_id = 200;
        for (i, level) in VOLTS.into_iter().enumerate() {
            let mut item = Item::new(next_id, Shape::Wire, Look::Committed,
                format!("volt {level} wire"), [10.0 + 10.0 * i as f64, -90.0]);
            item.live_ix[0] = i as f32;
            items.push(item);
            next_id += 1;
        }
        for (i, speed) in FLOWS.into_iter().enumerate() {
            let shape = if i == 7 { Shape::Elbow } else { Shape::Wire };
            let mut item = Item::new(next_id, shape, Look::Committed,
                format!("flow {speed} {}", shape.name()), [10.0 + 12.0 * i as f64, -100.0]);
            item.live_ix[1] = i as f32;
            item.flow = speed;
            items.push(item);
            next_id += 1;
        }
        for (i, glow) in GLOWS.into_iter().enumerate() {
            let mut item = Item::new(next_id, Shape::Part, Look::Committed,
                format!("glow {glow} part body"), [10.0 + 12.0 * i as f64, -110.0]);
            item.live_ix[2] = i as f32;
            items.push(item);
            next_id += 1;
        }
        // Below the glow row at -110, with one world unit between the six-unit strips.
        for (slot, name) in ["sine", "square", "DC", "empty"].into_iter().enumerate() {
            items.push(Item::new(next_id, Shape::Mini(slot), Look::Committed,
                format!("{name} mini trace"), [10.0 + 7.0 * slot as f64, -120.0]));
            next_id += 1;
        }
        for (slot, (name, _, _, _)) in PINS.into_iter().enumerate() {
            items.push(Item::new(next_id, Shape::Pin(slot), Look::Committed,
                format!("{name} pin mark"), [10.0 + 7.0 * slot as f64, -130.0]));
            next_id += 1;
        }
        Self(items)
    }
}

fn intersects(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

impl Scene {
    fn item(&self, id: u32) -> Option<&Item> {
        self.0.iter().find(|item| item.id == id)
    }

    fn item_mut(&mut self, id: u32) -> Option<&mut Item> {
        self.0.iter_mut().find(|item| item.id == id)
    }

    /// Use destinations for the fit and grid, so assembly does not shrink their bounds.
    fn bounds(&self) -> [f64; 4] {
        self.0.iter().map(|item| item.shape.bounds(item.home)).reduce(|a, b| {
            [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]
        }).unwrap_or([0.0, 0.0, 1.0, 1.0])
    }

    fn animated_visible(&self, cam: &Camera2d) -> bool {
        let view = cam.visible();
        self.0.iter().any(|item| {
            (item.flow != 0.0 || matches!(item.shape, Shape::Mini(_) | Shape::Pin(PIN_SWEEP))) && intersects(item.bounds(), view)
        })
    }
}

impl CanvasItems for Scene {
    type Key = u32;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<u32> {
        let mut hits = Vec::new();
        // Ghosts are purely visual. Within each shader, the final appended item is on top.
        for layer in (0..4).rev() {
            for item in self.0.iter().rev().filter(|item| item.shape.layer() == layer) {
                let b = item.bounds();
                if at[0] >= b[0] - radius && at[0] <= b[2] + radius
                    && at[1] >= b[1] - radius && at[1] <= b[3] + radius
                {
                    hits.push(item.id);
                }
            }
        }
        hits
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<u32>> {
        let view = cam.visible();
        self.0.iter().filter(|item| intersects(item.bounds(), view)).map(|item| {
            let mut rect = cam.rect_to_screen(item.bounds());
            // A horizontal segment still has a nonempty accessibility rectangle.
            if rect.size.y < 1.0 {
                rect.pos.y -= (1.0 - rect.size.y) * 0.5;
                rect.size.y = 1.0;
            }
            CanvasNode { sel: item.id, rect, role: "item", name: item.name.clone(), value: None }
        }).collect()
    }
}

struct Templates {
    line: LineTemplate,
    boxes: BoxTemplate,
    arc: ArcTemplate,
    bridge: BridgeTemplate,
    mini: MiniTemplate,
    pin: PinTemplate,
}

impl Templates {
    fn capture(cx: &Cx, line: &DrawCanvasLine, boxes: &DrawCanvasBox, arc: &DrawCanvasArc, bridge: &DrawCanvasBridge, mini: &DrawCanvasMini, pin: &DrawCanvasPin) -> Option<Self> {
        Some(Self {
            line: LineTemplate::capture(cx, line)?,
            boxes: BoxTemplate::capture(cx, boxes)?,
            arc: ArcTemplate::capture(cx, arc)?,
            bridge: BridgeTemplate::capture(cx, bridge)?,
            mini: MiniTemplate::capture(cx, mini)?,
            pin: PinTemplate::capture(cx, pin)?,
        })
    }
}

#[derive(Default)]
struct Rows {
    lines: Vec<f32>,
    boxes: Vec<f32>,
    minis: Vec<f32>,
    pins: Vec<f32>,
    arcs: Vec<f32>,
    bridges: Vec<f32>,
    bridge_route: Vec<f32>,
}

#[derive(Clone, Copy)]
struct Drag {
    id: u32,
    origin: [f64; 2],
    from: [f64; 2],
}

#[derive(Clone, Copy)]
struct Motion {
    start: f64,
    center: [f64; 2],
}

const BRIDGE_HOME: [[f32; 2]; 2] = [[30.0, 30.0], [70.0, 30.0]];
const BRIDGE_MOVE: f32 = (MOVE_SECONDS + 0.1) as f32;
const BRIDGE_STAGGER: f32 = 0.08;
const BRIDGE_FADE: f32 = 0.12;

/// Only a key press changes these poses. The GPU advances both flights from the motion clock.
struct BridgeDemo {
    out: bool,
    start: Option<f32>,
    from: [[f32; 2]; 2],
    to: [[f32; 2]; 2],
}

impl Default for BridgeDemo {
    fn default() -> Self {
        Self { out: false, start: None, from: BRIDGE_HOME, to: BRIDGE_HOME }
    }
}

impl BridgeDemo {
    fn span(&self, end: usize) -> [f32; 2] {
        self.start.map_or([0.0, 0.0], |start| [start + end as f32 * BRIDGE_STAGGER, BRIDGE_MOVE])
    }

    fn landing(&self) -> f32 {
        let span = self.span(1);
        span[0] + span[1]
    }

    fn route_alpha(&self, now: f32) -> f32 {
        if self.start.is_none() { 1.0 } else { ((now - self.landing()) / BRIDGE_FADE).clamp(0.0, 1.0) }
    }

    fn moving(&self, now: f32) -> bool {
        self.start.is_some() && now < self.landing() + BRIDGE_FADE
    }

    fn toggle(&mut self, now: f32) {
        // Sample each end's current flight, including its stagger, before replacing either pose.
        for end in 0..2 {
            let span = self.span(end);
            let t = if span[1] <= 0.0 { 1.0 } else { ((now - span[0]) / span[1]).clamp(0.0, 1.0) };
            let u = 1.0 - t;
            let mix = 1.0 - u * u * u;
            for axis in 0..2 {
                self.from[end][axis] += (self.to[end][axis] - self.from[end][axis]) * mix;
            }
        }
        self.out = !self.out;
        self.to = BRIDGE_HOME;
        if self.out {
            self.to[0][1] += 20.0;
            self.to[1][1] -= 20.0;
        }
        self.start = Some(now);
        log!("canvasbar: bridge demo {}", if self.out { "out" } else { "back" });
    }

    fn append(&self, templates: &Templates, rows: &mut Rows, tokens: &Tokens) {
        for end in 0..2 {
            let mut c = Common::at([self.to[end][0], self.to[end][1], 0.0], tokens.ink);
            c.pose_a = [self.from[end][0], self.from[end][1], 0.0, 1.0];
            c.span = self.span(end);
            templates.boxes.append(&mut rows.boxes, &BoxInst {
                c,
                rect: [0.0, 0.0, 6.0, 4.0],
                corner: 0.4,
                stroke: [1.5, 0.0],
                fill: tokens.part_fill,
            });
        }
        let a = [self.to[0][0] + 6.0, self.to[0][1]];
        let b = [self.to[1][0] - 6.0, self.to[1][1]];
        let mut c = Common::at([0.0, 0.0, 0.0], tokens.wire_signal);
        c.pose_a = [self.from[0][0] - self.to[0][0], self.from[0][1] - self.to[0][1], 0.0, 1.0];
        c.span = self.span(0);
        templates.bridge.append(&mut rows.bridges, &BridgeInst {
            c,
            ends: [a[0], a[1], b[0], b[1]],
            b_pose: [self.from[1][0] - self.to[1][0], self.from[1][1] - self.to[1][1], 0.0, 0.0],
            b_span: self.span(1),
            bend: 0.25,
            stroke: [1.5, 0.0],
            fade: if let Some(start) = self.start { [start, 0.0, self.landing(), BRIDGE_FADE] } else { [0.0; 4] },
        });
        // Three retained segments at the landed pin positions; the layer supplies a linear fade.
        let mid = (a[0] + b[0]) * 0.5;
        let points = [a, [mid, a[1]], [mid, b[1]], b];
        let mut along = 0.0;
        for pair in points.windows(2) {
            templates.line.append(&mut rows.bridge_route, &LineInst {
                c: Common::at([0.0, 0.0, 0.0], tokens.wire_signal),
                seg: [pair[0][0], pair[0][1], pair[1][0], pair[1][1]],
                along0: along,
                width: [1.5, 0.0],
                ..LineInst::default()
            });
            along += (pair[1][0] - pair[0][0]).hypot(pair[1][1] - pair[0][1]);
        }
    }
}

fn grid_ladder() -> Ladder {
    Ladder::new(vec![
        Rung { name: "coarse", admit: 0.0, keep: 0.0, fade: (0.0, 0.0) },
        Rung { name: "fine", admit: 6.0, keep: 4.0, fade: (6.0, 12.0) },
    ])
}

#[derive(Script, ScriptHook, Widget)]
pub struct CanvasBar {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_line: DrawCanvasLine,
    #[live]
    draw_box: DrawCanvasBox,
    #[live]
    draw_mini: DrawCanvasMini,
    #[live]
    draw_pin: DrawCanvasPin,
    #[live]
    draw_arc: DrawCanvasArc,
    #[live]
    draw_bridge: DrawCanvasBridge,
    #[live]
    draw_text: DrawText,
    #[rust]
    pane: CanvasPane<u32>,
    #[rust]
    lines: CanvasLayer,
    #[rust]
    boxes: CanvasLayer,
    #[rust]
    minis: CanvasLayer,
    #[rust]
    pins: CanvasLayer,
    #[rust]
    arcs: CanvasLayer,
    #[rust]
    bridges: CanvasLayer,
    #[rust]
    bridge_route: CanvasLayer,
    #[rust]
    grid_major: CanvasLayer,
    #[rust]
    grid_minor: CanvasLayer,
    #[rust(grid_ladder())]
    ladder: Ladder,
    #[rust]
    scene: Scene,
    #[rust]
    live: LiveHost,
    #[rust(true)]
    dark: bool,
    #[rust]
    focus: Option<u32>,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    motion: Option<Motion>,
    #[rust]
    bridge_demo: BridgeDemo,
    #[rust]
    motion_t: f32,
    #[rust]
    frame: NextFrame,
    #[rust(true)]
    dirty: bool,
    #[rust]
    templates: Option<Templates>,
    #[rust]
    capture_attempted: bool,
}

impl CanvasBar {
    fn bounds(&self) -> [f64; 4] {
        let b = self.scene.bounds();
        // Include both demo destinations so fitting does not move the camera during a flight.
        [b[0].min(24.0), b[1].min(6.0), b[2].max(76.0), b[3].max(54.0)]
    }

    fn fit(&mut self, cx: &mut Cx, now: f64) {
        let bounds = self.bounds();
        self.pane.nav.fly_to_box(&self.pane.cam, bounds, 24.0, now, None);
        self.pane.animate(cx);
    }

    /// Only the clock and CPU picking positions advance each frame; GPU rows remain retained.
    fn advance_motion(&mut self, now: f64) {
        self.motion_t = now as f32;
        let Some(motion) = self.motion else { return };
        let mix = ease_out((now - motion.start) / REVEAL_SECONDS);
        for item in &mut self.scene.0 {
            let offset = item.shape.local_center();
            let from = [motion.center[0] + offset[0], motion.center[1] + offset[1]];
            item.shown = [
                from[0] + (item.home[0] - from[0]) * mix,
                from[1] + (item.home[1] - from[1]) * mix,
            ];
        }
        if now >= motion.start + REVEAL_SECONDS {
            self.finish_motion();
        }
    }

    fn finish_motion(&mut self) {
        if self.motion.take().is_some() {
            for item in &mut self.scene.0 {
                item.shown = item.home;
            }
            self.dirty = true;
        }
    }

    fn move_drag(&mut self, id: u32, at: [f64; 2]) {
        let Some(drag) = self.drag.filter(|drag| drag.id == id) else { return };
        self.finish_motion();
        if let Some(item) = self.scene.item_mut(id) {
            let home = [drag.origin[0] + at[0] - drag.from[0], drag.origin[1] + at[1] - drag.from[1]];
            if item.home != home {
                item.home = home;
                item.shown = home;
                self.dirty = true;
            }
        }
    }

    /// Pointer clicks and semantic presses take exactly the same route.
    fn input(&mut self, cx: &mut Cx, input: CanvasInput<u32>, now: f64) {
        match input {
            CanvasInput::Click { key, .. } => {
                if self.focus != key {
                    self.focus = key;
                    self.dirty = true;
                }
                if let Some(item) = key.and_then(|id| self.scene.item(id)) {
                    self.pane.nav.fly_to_box(&self.pane.cam, item.bounds(), 120.0, now, None);
                }
            }
            CanvasInput::Escape => {
                self.dirty |= self.focus.take().is_some();
                self.fit(cx, now);
            }
            CanvasInput::DragStart { key, from, at } => {
                self.finish_motion();
                if let Some(item) = self.scene.item(key) {
                    self.drag = Some(Drag { id: key, origin: item.home, from });
                    self.dirty = true;
                    self.move_drag(key, at);
                }
            }
            CanvasInput::DragMove { key, at } => self.move_drag(key, at),
            CanvasInput::DragEnd { key, at } => {
                self.move_drag(key, at);
                if self.drag.is_some_and(|drag| drag.id == key) {
                    self.drag = None;
                    self.dirty = true;
                }
            }
            CanvasInput::DragCancel { key } => {
                if let Some(drag) = self.drag.filter(|drag| drag.id == key) {
                    self.finish_motion();
                    if let Some(item) = self.scene.item_mut(key) {
                        item.home = drag.origin;
                        item.shown = drag.origin;
                    }
                    self.drag = None;
                    self.dirty = true;
                }
            }
            CanvasInput::Key { code: KeyCode::KeyT, .. } => {
                self.dark = !self.dark;
                self.dirty = true;
            }
            CanvasInput::Key { code: KeyCode::KeyF, .. } => self.fit(cx, now),
            CanvasInput::Key { code: KeyCode::KeyM, .. } => {
                let b = self.scene.bounds();
                self.motion = Some(Motion { start: now, center: [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5] });
                self.advance_motion(now);
                self.dirty = true;
            }
            CanvasInput::Key { code: KeyCode::KeyB, .. } => {
                self.bridge_demo.toggle(now as f32);
                self.dirty = true;
            }
            CanvasInput::Key { code, .. } => {
                if !self.live.change_states(&self.scene, code) {
                    return;
                }
            }
            // The catalogue has no momentary controls: a press reads as its Click or drag.
            CanvasInput::Hover(_) | CanvasInput::Camera | CanvasInput::Press { .. } => {}
            // This catalogue has single-item focus; the pane still owns the marquee grammar.
            CanvasInput::Marquee { .. } => return,
        }
        self.pane.animate(cx);
        self.draw_bg.redraw(cx);
    }

    /// Publish only after edits, state/theme changes, or the endpoints of an assembly.
    fn rebuild(&mut self, tokens: &Tokens) {
        let Some(templates) = &self.templates else { return };
        let mut rows = Rows::default();
        if let Some(drag) = self.drag {
            if let Some(item) = self.scene.item(drag.id) {
                let mut c = item.common(drag.origin, Look::Ghost, None, tokens);
                c.item = -1.0; // The origin copy is neither pickable nor hoverable.
                item.shape.append(templates, &mut rows, c, tokens);
            }
        }
        for item in &self.scene.0 {
            let look = if self.drag.is_some_and(|drag| drag.id == item.id) {
                Look::Preview
            } else if self.focus == Some(item.id) {
                Look::Focus
            } else {
                item.look
            };
            item.shape.append(templates, &mut rows, item.common(item.home, look, self.motion, tokens), tokens);
        }
        self.bridge_demo.append(templates, &mut rows, tokens);
        self.lines.set(templates.line.slots(), rows.lines);
        self.boxes.set(templates.boxes.slots(), rows.boxes);
        self.minis.set(templates.mini.slots(), rows.minis);
        self.pins.set(templates.pin.slots(), rows.pins);
        self.arcs.set(templates.arc.slots(), rows.arcs);
        self.bridges.set(templates.bridge.slots(), rows.bridges);
        self.bridge_route.set(templates.line.slots(), rows.bridge_route);

        let b = self.bounds();
        let lo = [(b[0] - 20.0).floor() as i64, (b[1] - 20.0).floor() as i64];
        let hi = [(b[2] + 20.0).ceil() as i64, (b[3] + 20.0).ceil() as i64];
        let mut major = Vec::new();
        let mut minor = Vec::new();
        for axis in 0..2 {
            for n in lo[axis]..=hi[axis] {
                let is_major = n % 10 == 0;
                let ink = if is_major { tokens.grid_major } else { tokens.grid };
                let seg = if axis == 0 {
                    [n as f32, lo[1] as f32, n as f32, hi[1] as f32]
                } else {
                    [lo[0] as f32, n as f32, hi[0] as f32, n as f32]
                };
                templates.line.append(if is_major { &mut major } else { &mut minor }, &LineInst {
                    c: Common::at([0.0, 0.0, 0.0], ink),
                    seg,
                    width: [1.0, 0.0],
                    ..LineInst::default()
                });
            }
        }
        self.grid_major.set(templates.line.slots(), major);
        self.grid_minor.set(templates.line.slots(), minor);
        self.dirty = false;
    }

    fn labels(&mut self, cx: &mut Cx2d, tokens: &Tokens, weight: f32) {
        if self.pane.cam.px_per_unit * 10.0 < 10.0 || weight <= 0.0 {
            return;
        }
        let color = tokens.ink_dim;
        self.draw_text.color = vec4(color[0], color[1], color[2], color[3] * weight);
        cx.push_clip_rect(self.pane.cam.view);
        let rows = LOOKS.into_iter().enumerate().map(|(row, (_, name))| (name, -10.0 * row as f64))
            .chain([("volt", -90.0), ("flow", -100.0), ("glow", -110.0), ("mini", -120.0), ("pin", -130.0)]);
        for (name, y) in rows {
            let anchor = self.pane.cam.to_screen([6.0, y]);
            let size = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), name).size_in_lpxs;
            let pos = anchor - dvec2(size.width as f64 + 8.0, size.height as f64 * 0.5);
            self.draw_text.draw_abs(cx, pos, name);
        }
        cx.pop_clip_rect();
    }
}

impl Widget for CanvasBar {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let now = cx.seconds_since_app_start();
        self.advance_motion(now);
        if self.frame.is_event(event).is_some() {
            self.frame = NextFrame::default();
            self.draw_bg.redraw(cx);
        }
        let inputs = self.pane.handle_event(cx, event, self.draw_bg.area(), &self.scene);
        for input in inputs {
            self.input(cx, input, now);
        }
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        self.pane.semantics_parts(out);
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        if let Some(input) = self.pane.a11y_press(cx, request) {
            cx.set_key_focus(self.draw_bg.area());
            let now = cx.seconds_since_app_start();
            self.input(cx, input, now);
            a11y_tree::ActionResult::Handled
        } else {
            a11y_tree::ActionResult::Unhandled
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let bounds = self.bounds();
        self.pane.place(cx, rect, Some(bounds));
        if !self.capture_attempted {
            self.capture_attempted = true;
            self.live.size_states(&self.scene);
            // capture logs the first failure; do not retry (and log) every frame.
            self.templates = Templates::capture(cx, &self.draw_line, &self.draw_box, &self.draw_arc, &self.draw_bridge, &self.draw_mini, &self.draw_pin);
        }
        if self.templates.is_none() {
            return DrawStep::done();
        }
        let now = cx.seconds_since_app_start();
        self.advance_motion(now);
        let tokens = Tokens::for_theme(self.dark);
        let bg = tokens.canvas;
        self.draw_bg.color = vec4(bg[0], bg[1], bg[2], bg[3]);
        self.draw_bg.draw_abs(cx, rect);
        if self.dirty {
            self.rebuild(tokens);
        }

        let fine = self.ladder.at(self.pane.cam.device_px_per_unit()).weights[1];
        let uniforms = CanvasUniforms {
            drag_item: -1.0, drag_offset: [0.0; 2],
            cam: self.pane.cam.uniforms(),
            motion_t: self.motion_t,
            hover_item: self.pane.hovered().map_or(-1.0, |id| *id as f32),
            hover_mix: self.pane.hover_mix(),
            layer_alpha: 1.0,
            time: now as f32,
            live_time: now as f32,
            light: !self.dark,
            clip: [rect.pos.x as f32, rect.pos.y as f32,
                (rect.pos.x + rect.size.x) as f32, (rect.pos.y + rect.size.y) as f32],
        };
        // The host's part: bind this frame's rows (an upload only when a row changed).
        self.live.advance_mini(now);
        self.live.advance_pins(now);
        let nets = self.live.nets.texture(cx).clone();
        let live = CanvasLive {
            // Disabled slots borrow an existing handle until the builders replace it.
            items: nets.clone(),
            items_on: false,
            pane: nets.clone(),
            pane_on: false,
            mini: nets.clone(),
            nets,
            segs: self.live.segs.texture(cx).clone(),
            parts: self.live.parts.texture(cx).clone(),
            pins: self.live.pins.texture(cx).clone(),
        }.with_items(self.live.items.texture(cx).clone())
            .with_pane(self.live.pane.texture(cx).clone())
            .with_mini(self.live.mini.texture(cx).clone());
        uniforms.with(tokens, |u| self.grid_major.draw(cx, &mut self.draw_line.draw_vars, u, &live));
        CanvasUniforms { layer_alpha: fine, ..uniforms }.with(tokens, |u| {
            self.grid_minor.draw(cx, &mut self.draw_line.draw_vars, u, &live);
        });
        CanvasUniforms { layer_alpha: self.bridge_demo.route_alpha(self.motion_t), ..uniforms }.with(tokens, |u| {
            self.bridge_route.draw(cx, &mut self.draw_line.draw_vars, u, &live);
        });
        uniforms.with(tokens, |u| {
            self.lines.draw(cx, &mut self.draw_line.draw_vars, u, &live);
            self.bridges.draw(cx, &mut self.draw_bridge.draw_vars, u, &live);
            self.boxes.draw(cx, &mut self.draw_box.draw_vars, u, &live);
            let mini_hover = [self.pane.hovered().and_then(|id| self.scene.item(*id)).map_or(-1.0, |item| {
                if let Shape::Mini(slot) = item.shape { slot as f32 } else { -1.0 }
            })];
            let mut mini_uniforms = u.to_vec();
            mini_uniforms.push((live_id!(mini_hover), &mini_hover));
            self.minis.draw(cx, &mut self.draw_mini.draw_vars, &mini_uniforms, &live);
            self.pins.draw(cx, &mut self.draw_pin.draw_vars, u, &live);
            self.arcs.draw(cx, &mut self.draw_arc.draw_vars, u, &live);
        });
        self.labels(cx, tokens, fine);
        self.pane.gather_nodes(&self.scene);
        self.pane.animate(cx);
        let ghost_animated_visible = self.drag.is_some_and(|drag| {
            self.scene.item(drag.id).is_some_and(|item| {
                (item.flow != 0.0 || matches!(item.shape, Shape::Mini(_) | Shape::Pin(PIN_SWEEP)))
                    && intersects(item.shape.bounds(drag.origin), self.pane.cam.visible())
            })
        });
        if self.motion.is_some() || self.bridge_demo.moving(self.motion_t)
            || self.scene.animated_visible(&self.pane.cam) || ghost_animated_visible
        {
            if self.frame == NextFrame::default() {
                self.frame = cx.new_next_frame();
            }
        } else {
            self.frame = NextFrame::default();
        }
        DrawStep::done()
    }
}
