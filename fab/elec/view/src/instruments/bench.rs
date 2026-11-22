//! One bench acquisition, stable channels keyed by net, and a vertically reorderable lane stack.
//! Preview probes own independent feeds; real lanes share a trigger and the pinned time row/strip.
//! Stills, run ghosts, landing blooms and FLIP retain the existing per-net pictures. Drive
//! lanes precede the signal lanes and continue to read only the host's `BenchView`.

use super::{
    acquire::{LaneSetup, NetProbe, SimChannel},
    delay::{lane_at, write_label as write_delay_label, DelayBracket, DelayGesture},
    controls_panel::ControlsPanel,
    logic_view::PANEL_SHARE,
    look::{InstrumentLook, CHANNEL_HUES},
    drive_lane::{DriveLane, GeneratorAction},
    fw_events::{fw_at, FwEvents},
    fw_lane::{FwLane, TimeMap},
    phosphor::Hits,
    phosphor_view::Pushed,
    scope::{Channel, Domain, FftSettings, LabelCache, LaneExpect, Persistence, ScopeAction, SignalLane, ScopeSettings, Trigger, DEFAULT_LANE_HEIGHT, MIN_LANE_HEIGHT},
    scope_host::{volts, ScopeHost},
    scrub::{draw_spectrum_still, draw_still, Still, StillScratch},
    simfeed::{Feature, LimitedSpec, SimScopeConfig, SimScopeFeed},
    span::End,
    trend::{TrendBound, TrendPin, TrendQuantity},
    sources::BenchView,
    source_bar::{simulation_caps, Answers as SourceAnswers, SeeAs, SourceBar, SourceBarAction, SourceChip, SourceState},
};
use crate::{
    canvas::looks::Tokens,
    live::{
        peek::{carries_peek, PeekGesture},
        probe::{carries_probe, probe_drag_item, ProbeGesture},
        sim::SimAction,
        strip::{GhostState as RunGhost, RecordRow, StripModel, TimeStrip, TimeStripAction, TrendRow},
        tag::{DrawProbeTag, LaneSide, ProbeTag, TagKind, TagState, TagStyle},
    },
    snapshot::{next_tag, ElecLive, Refusal, ViewAction},
};
use fab_elec_bench::{source::{resolve, ChanId, ChannelInfo, ChannelSetup, ClockKind, Coupling, DepthWant, ModeWant, Probe, Quantity, RateWant, Setup, Side, SourceId, SourceInfo, TimeSetup, TriggerRun, TriggerSpec}, trigger::{EdgeTrigger, Qualifier, Slope, Window}, SimCmd};
use fab_elec_circuit::NetKind;
use fab_elec_core::ids::NetRef;
use fab_elec_design::{BenchOp, CursorSource, DesignOp, ProbeOf, ProbeQuantity, TimeCursor};
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use std::sync::Arc;
use crate::trace::{draw::DrawTraceSeg, view::StoreSource};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.ProbeTagTip = set_type_default() do #(ProbeTagTip::register_widget(vm)) {
        width: Fit height: Fit
        draw_tag: mod.draw.DrawProbeTag {}
        draw_text +: {text_style: theme.font_regular {font_size: 13}}
    }
    mod.widgets.BenchInstrumentsBase = #(BenchInstruments::register_widget(vm))
    mod.widgets.BenchInstruments = set_type_default() do mod.widgets.BenchInstrumentsBase {
        width: Fill height: Fill
        flow: Down
        spacing: 0
        padding: Inset{left: 12 right: 12 top: 12 bottom: 12}
        draw_empty +: {color: theme.color_text_meta text_style: theme.font_regular {font_size: 13}}
        draw_chip_text +: {color: theme.color_text text_style: theme.font_regular {font_size: 11}}
        draw_delay: mod.draw.DrawTraceSeg {}
        probe_tip: mod.widgets.DragChip {swatch +: {visible: true}}
        probe_tag_tip: mod.widgets.ProbeTagTip {}
        draw_probe_tag: mod.draw.DrawProbeTag {}
        scroll_bars: mod.widgets.ScrollBars {show_scroll_x: false show_scroll_y: true}
        strip: mod.widgets.TimeStrip {}
        source_bar: mod.widgets.SourceBar {}
        generator: mod.widgets.DriveLane {}
        fw_lane: mod.widgets.FwLane {}
        controls_panel: mod.widgets.ControlsPanel {}
        Card := mod.widgets.SignalLane {closable: true shared_time: true}
    }
}

/// A retained drag visual; its strings change only with the carried identity.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct ProbeTagTip {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[live] draw_tag: DrawProbeTag,
    #[live] draw_text: DrawText,
    #[rust] area: Area,
    #[rust] label: String,
    #[rust] pin: String,
    #[rust] answer: String,
    #[rust] answers: LabelCache,
    #[rust(TagStyle { hue: vec4(1.0, 1.0, 1.0, 1.0), kind: TagKind::Volt, side: LaneSide::Sim, state: TagState::Carried })]
    style: TagStyle,
}

impl ProbeTagTip {
    fn set_identity(&mut self, cx: &mut Cx, label: &str, pin: &str, answer: &str, style: TagStyle) {
        if self.label == label && self.pin == pin && self.answer == answer && self.style == style { return; }
        self.label.clear(); self.label.push_str(label);
        self.pin.clear(); self.pin.push_str(pin);
        self.answer.clear(); self.answer.push_str(answer);
        self.style = style;
        self.area.redraw(cx);
    }
}

impl WidgetNode for ProbeTagTip {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _cx: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
}

impl Widget for ProbeTagTip {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let tag = ProbeTag::from_label(&self.label, &self.pin);
        let metrics = tag.measure(cx, &mut self.draw_tag, &self.draw_text);
        let size = tag.layout(metrics, f64::INFINITY, self.style).size;
        self.answers.begin_frame(&self.draw_text, base_theme(cx));
        let answer = if self.answer.is_empty() { None } else { Some(self.answers.layout(cx, &self.draw_text, &self.answer)) };
        let extra = answer.as_ref().map_or(0.0, |text| 8.0 + f64::from(text.size_in_lpxs.width * self.draw_text.font_scale));
        let rect = cx.walk_turtle_with_area(&mut self.area, Walk { width: Size::Fixed(size.x + extra + 12.0), height: Size::Fixed(size.y + 12.0), ..walk });
        let at = rect.pos + dvec2(6.0, 6.0);
        tag.draw(cx, &mut self.draw_tag, &mut self.draw_text, Rect { pos: at, size }, self.style);
        if let Some(answer) = answer {
            self.draw_text.color = self.style.hue;
            self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: at + dvec2(size.x + 8.0, 3.0), size: dvec2(extra - 8.0, size.y - 3.0) }), &answer);
        }
        DrawStep::done()
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        makepad_platform::a11y::set_string(&mut node.name, &self.label);
        makepad_platform::a11y::set_string(&mut node.value, &self.answer);
    }
}

/// A lowered net name and its channel label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchProbe {
    pub net: String,
    pub label: String,
}

impl BenchProbe {
    /// The cards a bench's own scope channels (`b.probe`) ask for, in call order under their labels:
    /// one per voltage of a net. A differential voltage, a current or a power has no card yet; the
    /// second value counts them.
    pub fn from_bench(probes: &[fab_elec_lang::BenchProbe]) -> (Vec<BenchProbe>, usize) {
        let cards: Vec<BenchProbe> = probes.iter()
            .filter(|probe| probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none())
            .map(|probe| BenchProbe { net: probe.of.target.clone(), label: probe.label.clone() })
            .collect();
        let skipped = probes.len() - cards.len();
        (cards, skipped)
    }
}

/// A bench source edit for the host to apply.
#[derive(Clone, Debug, Default)]
pub enum BenchAction {
    #[default]
    None,
    RemoveProbe { net: String },
}

/// The person removes the probe of `net` (its card's x, or its tag dragged off the bench). A host
/// that feeds the pane hears `BenchAction::RemoveProbe`; while the pane follows the electronics
/// host (`followed`: the bench it shows and that bench's scope channels), the channel also goes
/// from the bench's document as one undo step, so the probe leaves every view.
pub(super) fn remove_probe(cx: &mut Cx, uid: WidgetUid, followed: Option<(&str, &[fab_elec_lang::BenchProbe])>, net: String) {
    if let Some(op) = followed.and_then(|(bench, probes)| crate::live::probe::remove_probe_op(bench, probes, &net)) {
        cx.widget_action(uid, op);
    }
    cx.widget_action(uid, BenchAction::RemoveProbe { net });
}

/// Stable channel layout and independently movable display order. Membership alone advances the
/// layout generation; reordering tags never changes the solver's channel indices.
///
/// Each channel's hue belongs to its probe: a probe keeps the hue it got when it came, whatever
/// comes or goes around it, and a new probe takes the lowest hue no other probe wears (past
/// [`CHANNEL_HUES`] probes, the one fewest wear). The Bench lanes, the Scope's lanes and the
/// probe flags on the sheet and board all read it, through [`PeekGesture::hue_of`].
#[derive(Default)]
pub struct LaneStack {
    channels: Vec<String>,
    /// By channel: its probe's hue.
    hues: Vec<usize>,
    display: Vec<String>,
    overlays: Vec<(String, String)>,
    generation: u64,
}

impl LaneStack {
    pub fn channels(&self) -> &[String] { &self.channels }
    /// By channel, its probe's hue (an index into the look's channel colours).
    pub fn hues(&self) -> &[usize] { &self.hues }
    /// The hue of `net`'s channel, if it has one.
    pub fn hue_of(&self, net: &str) -> Option<usize> { self.channels.iter().position(|n| n == net).map(|i| self.hues[i]) }
    /// The hue a probe that comes next takes.
    pub fn next_hue(&self) -> usize { next_hue(&self.hues) }
    pub fn display(&self) -> &[String] { &self.display }
    pub fn generation(&self) -> u64 { self.generation }
    pub fn sync(&mut self, probes: &[BenchProbe]) -> bool {
        let changed = probes.len() != self.channels.len() || probes.iter().any(|p| !self.channels.contains(&p.net));
        if changed {
            let kept: Vec<Option<usize>> = probes.iter().map(|p| self.hue_of(&p.net)).collect();
            let mut hues: Vec<usize> = kept.iter().flatten().copied().collect();
            self.hues = kept.into_iter().map(|hue| hue.unwrap_or_else(|| {
                let hue = next_hue(&hues);
                hues.push(hue);
                hue
            })).collect();
            self.channels = probes.iter().map(|p| p.net.clone()).collect();
            self.overlays.retain(|(guest, host)| self.channels.contains(guest) && self.channels.contains(host));
            self.display.retain(|net| self.channels.contains(net));
            for probe in probes { if !self.display.contains(&probe.net) { self.display.push(probe.net.clone()); } }
            self.generation += 1;
        } else { self.display = probes.iter().map(|p| p.net.clone()).collect(); }
        changed
    }
    pub fn host_of(&self, net: &str) -> Option<&str> {
        self.overlays.iter().find(|(guest, _)| guest == net).map(|(_, host)| host.as_str())
    }
    pub fn guest_of(&self, net: &str) -> Option<&str> {
        self.overlays.iter().find(|(_, host)| host == net).map(|(guest, _)| guest.as_str())
    }
    pub fn overlay(&mut self, guest: &str, host: &str) -> bool {
        if guest == host || !self.channels.iter().any(|net| net == guest) || !self.channels.iter().any(|net| net == host)
            || self.overlays.iter().any(|(a, b)| a == guest || b == guest || a == host || b == host) { return false; }
        self.overlays.push((guest.into(), host.into()));
        true
    }
    pub fn separate(&mut self, net: &str) -> bool {
        let Some(at) = self.overlays.iter().position(|(guest, host)| guest == net || host == net) else { return false };
        self.overlays.remove(at);
        true
    }
    pub fn reorder(&mut self, net: &str, to: usize) -> bool {
        let Some(from) = self.display.iter().position(|n| n == net) else { return false };
        let to = to.min(self.display.len().saturating_sub(1));
        if from == to { return false; }
        let net = self.display.remove(from);
        self.display.insert(to, net);
        true
    }
}

struct TagDrag {
    net: String, original: Vec<String>, over: bool, target: Option<String>, target_rect: Option<Rect>,
    pair: Option<(String, String, usize)>,
}

/// The lowest hue none of `taken` is; with every hue taken, the one taken fewest times.
pub fn next_hue(taken: &[usize]) -> usize {
    (0..CHANNEL_HUES).min_by_key(|&hue| (taken.iter().filter(|&&t| t == hue).count(), hue)).unwrap_or(0)
}

fn overlay_host<'a>(stack: &'a LaneStack, drag: Option<&'a TagDrag>, net: &str) -> Option<&'a str> {
    drag.filter(|drag| drag.net == net).and_then(|drag| drag.target.as_deref()).or_else(|| stack.host_of(net))
}

fn overlay_guest<'a>(stack: &'a LaneStack, drag: Option<&'a TagDrag>, net: &str) -> Option<&'a str> {
    drag.filter(|drag| drag.target.as_deref() == Some(net)).map(|drag| drag.net.as_str()).or_else(|| stack.guest_of(net))
}
const LANE_DRAG: LiveId = live_id!(elec_lane);
const TRIGGER_DRAG: LiveId = live_id!(elec_trigger);
struct TriggerDrag { net: String, original: f64, level: f64, grab: f64 }

struct KeptDelay {
    key: u64,
    typed: [bool; 2],
    bracket: DelayBracket,
    edges: [Option<Feature>; 2],
    periods: [f64; 2],
    label: String,
    upper: End,
}

#[derive(Clone, Copy)]
struct DelayDrag {
    lane: usize,
    edge: Feature,
    period: f64,
    found: bool,
    edit: Option<(usize, End)>,
    abs: DVec2,
    target: Option<(usize, Feature)>,
}

/// The pointer travels this far from a press on the probe chip before the press is a drag.
pub(super) const DRAG_SLOP: f64 = 4.0;
/// A ghost card shows through a veil of the bench background this opaque.
const GHOST_VEIL: f32 = 0.5;
/// A ghost the probe left or the list removed fades out over this many seconds.
const GHOST_FADE: f64 = 0.15;
/// A dropped probe's ghost waits this long for the probe list to name its net.
const DROP_WAIT: f64 = 2.0;
/// A new card's veil lifts over this many seconds, ease-out.
const BLOOM_LIFT: f64 = 0.22;
/// Its outline and phosphor flash fade over this many seconds.
const BLOOM_GLOW: f64 = 0.6;
/// The phosphor gain at the start of a bloom.
const BLOOM_FLASH: f32 = 2.5;
/// A card slides to its new place over this many seconds, with cubic ease-out.
const FLIP_TIME: f64 = 0.22;

/// A previewed net or a removed probe whose card is fading out.
struct Ghost {
    net: String,
    label: String,
    /// A removed probe's place in the stack, which its lane keeps while it fades.
    at: Option<usize>,
    state: GhostState,
}

#[derive(Clone, Copy, PartialEq)]
enum GhostState {
    /// The carried probe is over the net.
    Hover,
    /// The probe was dropped on the net; the ghost waits for the probe list.
    Dropped(Instant),
    /// The probe left the net or the probe list removed it.
    Leaving(Instant),
}

/// The UI thread's CPU time for the scope's frame counts: Linux reads it from `/proc`; other
/// platforms have no reading (and no file).
#[derive(Default)]
struct ThreadCpu {
    #[cfg(target_os = "linux")]
    schedstat: Option<std::fs::File>,
}

impl ThreadCpu {
    /// Nanoseconds the calling thread has run. The file opens on the first call, from the UI
    /// thread, so it stays that thread's entry.
    #[cfg(target_os = "linux")]
    fn now_ns(&mut self) -> Option<u64> {
        use std::os::unix::fs::FileExt;
        if self.schedstat.is_none() { self.schedstat = std::fs::File::open("/proc/thread-self/schedstat").ok(); }
        let mut buf = [0u8; 64];
        let n = self.schedstat.as_ref()?.read_at(&mut buf, 0).ok()?;
        std::str::from_utf8(&buf[..n]).ok()?.split_whitespace().next()?.parse().ok()
    }

    #[cfg(not(target_os = "linux"))]
    fn now_ns(&mut self) -> Option<u64> { None }
}

/// A card the probe list just gained: its veil lifts, its outline and phosphor flash fade.
struct Bloom {
    start: Instant,
    /// The veil it starts under: a ghost's when the ghost is promoted, opaque when there was none.
    veil: f32,
}

/// A card's offset from its target when its slide starts.
struct Flip {
    from: DVec2,
    start: Instant,
}

struct BenchLane {
    widget: WidgetRef,
    /// On screen in the bench's viewport (or dragged, or moving).
    visible: bool,
    /// The time view holds a picture. Only a primed card off screen is Hidden: a card below the fold
    /// still takes one raster, so it never scrolls in blank.
    primed: bool,
    /// `Cx::repaint_id` when it came back on screen, until a later frame has judged what the reveal showed.
    reveal_at: Option<u64>,
    setup: Option<LaneSetup>,
    channel: Option<usize>,
    host: ScopeHost<SimScopeFeed>,
    start_failed: bool,
    measurement_generation: u64,
    bracket_generation: u64,
    preview_setup: Option<(LaneSetup, EdgeTrigger, Window, Option<f64>, Qualifier)>,
    row: Option<(u64, Option<usize>)>,
    hits: Option<Hits>,
    spectrum_hits: Option<Hits>,
    spectrum_pending: bool,
    scratch: StillScratch,
    shown: Option<StillKey>,
    /// The hits are a still awaiting upload, rather than a spent buffer.
    still_pending: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct StillKey {
    at: f64,
    generation: u64,
    // Front trimming preserves the end, so both bounds belong to the key.
    base: usize,
    end: usize,
    window: Window,
    spectrum: Option<(FftSettings, (u32, u32))>,
    bracket: Option<(f64, f64)>,
    volts: (f64, f64),
    plot: (u32, u32),
}

#[derive(Clone, Copy, PartialEq)]
struct LiveKey {
    record: (f64, f64),
    ratio: Option<f64>,
    t: f64,
    playing: bool,
    scrub: Option<f64>,
    trace: Option<(u64, usize, usize)>,
    index: u64,
    /// The run waits for its first firmware image.
    compiling: bool,
}

/// Whether the run waits for its first firmware image: one compiles and none ran yet.
fn compiling_first_image(sim: &crate::live::sim::SimLive) -> bool {
    sim.firmware.as_ref().is_some_and(|fw| fw.running_rev.is_none() && fw.compiling_rev.is_some())
}

impl LiveKey {
    fn read(cx: &Cx) -> Option<Self> {
        let sim = &cx.get_global_ref::<ElecLive>()?.sim;
        Some(Self { record: sim.record, ratio: sim.ratio, t: sim.t, playing: sim.playing, scrub: sim.scrub,
            trace: sim.trace.as_ref().map(|trace| (trace.generation, trace.base, trace.len())), index: sim.index.generation,
            compiling: compiling_first_image(sim) })
    }
}

/// The solver node a probe's net reads: the lowering names each node by its circuit net's name
/// (`Design::probe_net`), except the ground net's, which is "0". Runs when a feed starts: O(nets).
pub(crate) fn probe_node(cx: &mut Cx, net: &str) -> String {
    let ground = cx.get_global_ref::<ElecLive>().is_some_and(|live| {
        let design = live.snap.live();
        match live.run_bench() {
            Some(bench) => bench.out.circuit(design).and_then(|c| c.net(net)).is_some_and(|n| n.kind == NetKind::Ground),
            None => design.units.iter().any(|unit| unit.circuit.net(net).is_some_and(|n| n.kind == NetKind::Ground)),
        }
    });
    if ground { "0".to_owned() } else { net.to_owned() }
}

pub(super) fn new_card(cx: &mut Cx, template: &ScriptObjectRef) -> WidgetRef {
    cx.with_vm(|vm| WidgetRef::script_from_value(vm, template.as_object().into()))
}


/// The cards' timebase for a bench that names no scenario length: a few PWM periods.
const DEFAULT_TIME_PER_DIV: f64 = 20e-6;

/// The Bench's id for its probes ask (`SimAction::Probes`).
const BENCH_PROBES: u64 = 0x4245_4e43_4850_5242;

/// The cards' timebase for a bench: its scenario (`run.stop`) fills the ten divisions, so a 200 ms
/// chase shows whole steps where the default would show 200 us of one level.
pub fn bench_time_per_div(stop: Option<f64>) -> f64 {
    stop.filter(|stop| stop.is_finite() && *stop > 0.0).map_or(DEFAULT_TIME_PER_DIV, |stop| stop / 10.0)
}

/// A new lane's height: four lanes at the default height fill the panel, and more share that
/// height down to the minimum, so a bench of many probes opens without scrolling past most.
pub fn new_lane_height(lanes: usize) -> f64 {
    (DEFAULT_LANE_HEIGHT * 4.0 / lanes.max(1) as f64).clamp(MIN_LANE_HEIGHT, DEFAULT_LANE_HEIGHT)
}

pub(crate) fn initial_settings(label: &str) -> ScopeSettings {
    ScopeSettings {
        answers: None,
        channels: vec![Channel {
            name: label.to_owned(),
            unit: "V".into(),
            volts_per_div: 1.0,
            offset_div: 2.0,
            on: true,
            color_index: 0,
        }],
        time_per_div: DEFAULT_TIME_PER_DIV,
        domain: Domain::Time,
        fft: FftSettings::default(),
        trigger: Trigger {
            channel: 0,
            level: 1.65,
            slope: Slope::Rising,
            holdoff: 0.0,
            auto: true,
            qualify: Qualifier::None,
            zone: None,
        },
        position: 0.2,
        persistence: Persistence::Seconds(0.2),
        intensity: 1000.0,
        running: true,
        single: false,
    }
}

/// Draws one scope over all probes while the bench is visible.
#[derive(Script, WidgetRegister, WidgetRef, WidgetSet)]
pub struct BenchInstruments {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_empty: DrawText,
    #[live]
    scroll_bars: ScrollBars,
    /// The time strip's template; the strip is a child widget, so the semantic tree reaches it.
    #[live]
    strip: ScriptObjectRef,
    #[rust]
    strip_widget: Option<WidgetRef>,
    #[rust]
    strip_model: Option<StripModel>,
    #[rust]
    event_generation: Option<u64>,
    #[rust]
    latest_trigger_at: Option<f64>,
    #[rust]
    events_pending: bool,
    #[live]
    source_bar: ScriptObjectRef,
    #[rust]
    source_bar_widget: Option<WidgetRef>,
    /// The run board's controls, generated from its parts (`controls_panel`), under the lanes.
    #[live]
    controls_panel: ScriptObjectRef,
    #[rust]
    controls_panel_widget: Option<WidgetRef>,
    #[rust]
    source_chip: Option<SourceChip>,
    #[rust]
    see_as: SeeAs,
    #[rust]
    limited: Option<LimitedSpec>,
    #[rust]
    source_timer: Timer,
    #[rust]
    source_stats: super::feed::FeedStats,
    #[rust]
    source_stats_at: Option<Instant>,
    #[rust(std::env::var("ELEC_SCOPE_PERF").is_ok_and(|value| value == "1"))]
    perf: bool,
    #[rust]
    perf_at: Option<Instant>,
    #[rust]
    perf_rastered: Vec<u64>,
    #[rust]
    perf_presented: Vec<u64>,
    /// The last drawing seen by `frame`: its id, wall time and this thread's CPU time (Linux).
    #[rust]
    perf_draw: Option<(u64, Instant, Option<u64>)>,
    /// Per drawing: UI thread CPU and wall gap since the previous one, in microseconds.
    #[rust]
    perf_ui_us: Vec<u32>,
    #[rust]
    perf_gap_us: Vec<u32>,
    /// Cards that came back on screen with a picture kept from before, and those of them whose first frame
    /// on screen painted no picture (a flash of an empty screen).
    #[rust]
    perf_revealed: u32,
    #[rust]
    perf_reveal_empty: u32,
    #[rust]
    perf_cpu: ThreadCpu,
    #[rust]
    ghost: Option<RunGhost>,
    #[rust(true)]
    following: bool,
    #[rust(1.0)]
    rate: f64,
    #[rust]
    last_heard: Option<(f64, bool)>,
    /// The probe chip a probe drag starts from.
    #[live]
    draw_probe: DrawColor,
    /// The probe riding the pointer.
    #[live]
    probe_tip: ScriptObjectRef,
    #[rust]
    tip: Option<WidgetRef>,
    #[rust]
    tag_tip: Option<WidgetRef>,
    #[live]
    probe_tag_tip: ScriptObjectRef,
    #[live]
    draw_probe_tag: DrawProbeTag,
    /// Veils, outlines and dashes.
    #[live]
    draw_shape: DrawColor,
    #[live]
    draw_chip_text: DrawText,
    #[rust]
    chip_cache: LabelCache,
    /// Room kept below the time strip; none by default.
    #[live(0.0)]
    bottom_space: f64,
    #[rust]
    template: Option<ScriptObjectRef>,
    #[live]
    generator: ScriptObjectRef,
    #[live]
    fw_lane: ScriptObjectRef,
    #[rust]
    fw_lane_widget: Option<WidgetRef>,
    #[rust]
    fw_events: Option<Arc<dyn FwEvents>>,
    #[rust]
    source_bench: String,
    /// A host that calls `set_bench` feeds this pane; until one does, it follows `ElecLive`.
    #[rust]
    bench_fed: bool,
    /// The host's bench views this pane last followed (`ElecLive`'s snapshot), while unfed.
    #[rust]
    followed_benches: Option<(Arc<Vec<BenchView>>, Option<Arc<str>>)>,
    /// The bench's own scope channels as last applied to the cards.
    #[rust]
    bench_probes: Vec<fab_elec_lang::BenchProbe>,
    /// The bench's scenario length (`run.stop`) as last applied to the timebase.
    #[rust]
    bench_stop: Option<f64>,
    #[rust]
    generator_order: Vec<String>,
    #[rust]
    generators: HashMap<String, WidgetRef>,
    #[rust]
    probes: Vec<BenchProbe>,
    #[rust]
    expects: HashMap<String, Vec<LaneExpect>>,
    #[rust]
    bounds: Vec<TrendBound>,
    #[rust]
    stop_bounds: Vec<usize>,
    #[rust]
    stop_on_fail: HashSet<(String, String)>,
    #[rust]
    stop_pending: bool,
    #[rust]
    stop_playing: bool,
    #[rust]
    stop_resume_armed: bool,
    #[rust]
    violation_stop_seen: Option<(u64, u64, u64)>,
    #[rust]
    violation_jump: Option<f64>,
    #[rust]
    cards: HashMap<String, BenchLane>,
    #[rust]
    heights: HashMap<String, f64>,
    #[rust]
    resizing: Option<String>,
    #[rust]
    stack: LaneStack,
    #[rust]
    overlay_routes: HashMap<String, WidgetRef>,
    #[rust]
    semantic_summary: std::rc::Rc<std::cell::RefCell<super::semantics::Summary>>,
    #[rust]
    visible: Vec<BenchProbe>,
    #[rust]
    host: ScopeHost<SimScopeFeed>,
    #[rust(initial_settings(""))]
    scope_settings: ScopeSettings,
    #[rust]
    bracket: Option<(f64, f64)>,
    #[rust]
    bracket_generation: u64,
    #[rust]
    delays: Vec<KeptDelay>,
    #[rust]
    pins: Vec<TrendPin>,
    #[rust]
    delay_drag: Option<DelayDrag>,
    #[rust]
    delay_label: String,
    #[live]
    draw_delay: DrawTraceSeg,
    #[rust]
    trigger_net: Option<String>,
    #[rust]
    installed: Option<u64>,
    #[rust]
    installed_see_as: Option<SeeAs>,
    #[rust]
    setups: Vec<LaneSetup>,
    #[rust]
    configuration: Option<(EdgeTrigger, usize, Window, Option<f64>, Qualifier, Option<super::acquire::Zone>)>,
    #[rust]
    start_failed: bool,
    #[rust]
    measurement_generation: u64,
    #[rust]
    spark_source: Option<Arc<StoreSource>>,
    /// The numbering the cards' record rows were looked up in, told apart by identity.
    #[rust]
    rows_index: Option<Arc<fab_elec_bench::ValueIndex>>,
    /// The strip's rows last asked of the bench to keep whole over the run (`SimAction::Probes`).
    #[rust]
    asked_probes: Vec<usize>,
    #[rust]
    tag_drag: Option<TagDrag>,
    #[rust]
    trigger_drag: Option<TriggerDrag>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    live_key: Option<LiveKey>,
    /// The draw event (`Cx::drawing_id`) current when the bench asked for a redraw it has not drawn yet.
    #[rust]
    asked_draw: Option<u64>,
    #[rust]
    probe_hover: bool,
    /// A press on the probe chip that has not moved past the slop yet.
    #[rust]
    press: Option<DVec2>,
    /// The pointer, while a probe drag from this bench is on.
    #[rust]
    carrying: Option<DVec2>,
    #[rust]
    ghosts: Vec<Ghost>,
    #[rust]
    peek_hover: Option<BenchProbe>,
    #[rust]
    blooms: HashMap<String, Bloom>,
    /// The last unshifted target of each lane, in bench content coordinates.
    #[rust]
    placed: HashMap<String, DVec2>,
    /// A lane `show_lane` asked for before it was placed: scrolled to once it is.
    #[rust]
    reveal: Option<String>,
    #[rust]
    flips: HashMap<String, Flip>,
    #[rust]
    last_order: Vec<String>,
    #[rust]
    last_width: f64,
    /// A card made ahead from the template, so its shaders are built when the bench first draws and a
    /// new probe's card is ready (I21); the next probe takes it and the next draw makes another.
    #[rust]
    spare: Option<WidgetRef>,
    /// Warm the generator's shaders at bench open, before the host supplies its first sources (I21).
    #[rust]
    generator_spare: Option<WidgetRef>,
}

impl ScriptHook for BenchInstruments {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.template = None;
            self.spare = None;
            self.generator_spare = None;
            self.tip = None;
            self.tag_tip = None;
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        self.host.set_latency_enabled(self.perf);
        // Root templates outside eval applies, as FlatList does.
        if !apply.is_eval() {
            if let Some(object) = value.as_object() {
                vm.vec_with(object, |vm, values| {
                    for value in values {
                        if value.key.as_id() == Some(id!(Card)) {
                            if let Some(object) = value.value.as_object() {
                                self.template = Some(vm.new_object_ref(object));
                            }
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            self.placed.clear();
            self.flips.clear();
            self.last_order.clear();
            if let Some(template) = &self.template {
                for entry in self.cards.values_mut() {
                    entry.widget.script_apply(vm, apply, scope, template.as_object().into());
                }
            }
            for widget in self.generators.values_mut() {
                widget.script_apply(vm, apply, scope, self.generator.as_object().into());
            }
            if let Some(widget) = &mut self.fw_lane_widget {
                widget.script_apply(vm, apply, scope, self.fw_lane.as_object().into());
            }
            if let Some(widget) = &mut self.source_bar_widget {
                widget.script_apply(vm, apply, scope, self.source_bar.as_object().into());
            }
            if let Some(widget) = &mut self.controls_panel_widget {
                widget.script_apply(vm, apply, scope, self.controls_panel.as_object().into());
            }
            if let Some(widget) = &mut self.strip_widget {
                widget.script_apply(vm, apply, scope, self.strip.as_object().into());
            }
        }
    }
}

#[path = "bench_nodes.rs"]
mod nodes;

impl BenchInstruments {
    /// The host supplies a read-only firmware store; no source is installed by default.
    pub fn set_fw_events(&mut self, cx: &mut Cx, events: Option<Arc<dyn FwEvents>>) {
        self.fw_events = events;
        if self.fw_events.is_some() {
            let template = &self.fw_lane;
            let widget = self.fw_lane_widget.get_or_insert_with(|| new_card(cx, template));
            if let Some(mut lane) = widget.borrow_mut::<FwLane>() { lane.set_events(cx, self.fw_events.clone()); }
        } else {
            self.fw_lane_widget = None;
        }
        cx.widget_tree_mark_dirty(self.uid);
        self.redraw(cx);
    }

    /// The time strip, made from its template on first use.
    fn strip_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        let template = &self.strip;
        self.strip_widget.get_or_insert_with(|| new_card(cx, template)).clone()
    }

    fn source_bar_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        let template = &self.source_bar;
        self.source_bar_widget.get_or_insert_with(|| new_card(cx, template)).clone()
    }


    /// The lanes read out the time resolution the run steps at (`LiveStatus::res`, 0 under Exact),
    /// a lane made since included. Per live change one compare per lane; a lane redraws only on a
    /// changed resolution.
    fn sync_res(&mut self, cx: &mut Cx) {
        let Some(res) = cx.get_global_ref::<ElecLive>().map(|live| if live.sim.status.mode == fab_elec_bench::SimMode::Exact { 0.0 } else { live.sim.status.res }) else { return };
        for entry in self.cards.values() {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                lane.set_sim_res(cx, res);
            }
        }
    }

    fn controls_panel_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        let template = &self.controls_panel;
        self.controls_panel_widget.get_or_insert_with(|| new_card(cx, template)).clone()
    }

    /// The end of the strip's span: the record's newest time.
    fn strip_end(&self) -> f64 {
        self.strip_widget.as_ref().and_then(|strip| strip.borrow::<TimeStrip>().map(|strip| strip.state().span.1)).unwrap_or(0.0)
    }

    /// Acquisition publishes immutable chunks; pickup is independent of record length.
    /// A contended mailbox gets another UI frame even while the simulator is paused.
    fn sync_events(&mut self, cx: &mut Cx) -> bool {
        let pins = self.trend_pins();
        let (bounds, armed) = (&self.bounds, &self.stop_bounds);
        let playing = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.playing);
        if playing && !self.stop_playing && !self.stop_resume_armed && !armed.is_empty() { self.stop_pending = true; }
        if playing { self.stop_resume_armed = false; }
        self.stop_playing = playing;
        let feed = self.host.feed().filter(|_| self.installed == Some(self.stack.generation));
        let trends_pending = feed.is_some_and(|feed| !feed.set_trends(&pins));
        let bounds_pending = feed.is_some_and(|feed| !feed.set_bounds(bounds));
        if self.stop_pending && !bounds_pending {
            if let Some(feed) = feed {
                if feed.arm_stop_on_violation(armed) { self.stop_pending = false; self.violation_jump = None; }
            }
        }
        let events = match feed {
            Some(feed) => match feed.events() {
                Some(events) => Some(events),
                None => { self.events_pending = true; return false; }
            }
            None => None,
        };
        self.events_pending = trends_pending || bounds_pending || self.stop_pending && feed.is_some();
        self.latest_trigger_at = events.as_ref().and_then(|events| events.list.len().checked_sub(1).and_then(|index| events.list.get(index)));
        if self.violation_stop_seen.is_some_and(|(configuration, epoch, _)| events.as_ref().is_none_or(|snapshot| (snapshot.configuration, snapshot.event_epoch) != (configuration, epoch))) {
            self.violation_stop_seen = None; self.violation_jump = None;
        }
        if !self.stop_pending && !bounds_pending && !armed.is_empty() {
            if let (Some(feed), Some(snapshot)) = (feed, &events) {
                if let Some((request, t)) = feed.violation_stop(snapshot) {
                    let key = (snapshot.configuration, snapshot.event_epoch, request);
                    if self.violation_stop_seen != Some(key) {
                        self.violation_stop_seen = Some(key);
                        self.violation_jump = Some(t);
                    }
                } else if snapshot.stopped.is_some() { self.events_pending = true; }
            }
        }
        self.events_pending |= self.violation_jump.is_some();
        let generation = events.as_ref().map(|events| events.generation);
        let changed = generation != self.event_generation;
        if changed { self.sync_distributions(cx, events.as_deref()); }
        let strip = self.strip_widget(cx);
        if let Some(mut strip) = strip.borrow_mut::<TimeStrip>() {
            if changed {
                strip.set_events(cx, events);
                self.event_generation = generation;
            }
            let channel = self.trigger_net.as_ref().and_then(|net| self.cards.get(net)).and_then(|entry| entry.channel).unwrap_or(0);
            strip.set_event_channel(cx, channel);
        }
        changed
    }

    fn sync_distributions(&mut self, cx: &mut Cx, events: Option<&super::simfeed::EventSnapshot>) {
        for (net, entry) in &self.cards {
            let channel = self.stack.channels.iter().position(|name| name == net);
            let columns: Vec<_> = events.into_iter().flat_map(|events| events.columns.iter())
                .filter(|series| Some(series.lane) == channel).collect();
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                lane.set_distribution_ranges(cx, columns.iter().filter_map(|series| series.range.map(|range| (series.quantity, range))).collect());
                lane.set_distributions(cx, &columns.iter().map(|series| (series.quantity, series.distribution.clone())).collect::<Vec<_>>());
            }
        }
    }

    /// The glyph and host API use the same settings/reconfiguration path.
    pub fn set_probe_domain(&mut self, cx: &mut Cx, net: &str, domain: Domain) {
        self.sync_cards(cx);
        let Some(entry) = self.cards.get_mut(net) else { return };
        if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
            let mut settings = lane.settings().clone();
            if settings.domain != domain {
                settings.set_domain(domain);
                lane.set_settings(cx, settings);
                lane.phosphor().clear(cx);
                entry.shown = None;
            }
        }
        self.redraw(cx);
    }

    /// The evaluated bench enters here from a host that feeds the pane; the pane no longer follows
    /// `ElecLive` after it.
    pub fn set_bench(&mut self, cx: &mut Cx, sources: BenchView) {
        self.bench_fed = true;
        self.show_bench(cx, sources);
    }

    /// An unfed pane follows the chosen bench, including its refusal when it failed to build.
    fn follow_bench(&mut self, cx: &mut Cx) {
        if self.bench_fed {
            return;
        }
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        let benches = &live.snap.benches;
        if self.followed_benches.as_ref().is_some_and(|(shown, choice)| Arc::ptr_eq(shown, benches) && *choice == live.chosen_bench) {
            return;
        }
        let row = crate::snapshot::bench_index(benches, live.chosen_bench.as_deref()).unwrap_or(0);
        let view = benches.get(row).cloned();
        self.followed_benches = Some((benches.clone(), live.chosen_bench.clone()));
        if let Some(view) = view {
            self.show_bench(cx, view);
        } else if !self.source_bench.is_empty() {
            self.cancel_generators(cx);
            self.source_bench.clear();
            self.generators.clear();
            self.generator_order.clear();
            self.bench_probes.clear();
            self.set_probes(cx, Vec::new());
            self.bench_stop = None;
            self.scope_settings.time_per_div = bench_time_per_div(None);
            self.sync_shared(cx);
            cx.widget_tree_mark_dirty(self.uid);
            self.redraw(cx);
        }
    }

    /// Show one evaluated bench. Reuse cards by stub path, keeping an active pin's value.
    fn show_bench(&mut self, cx: &mut Cx, sources: BenchView) {
        let changed_bench = self.source_bench != sources.out.name;
        if changed_bench {
            self.cancel_generators(cx);
            self.generators.clear();
            self.source_bench = sources.out.name.clone();
        }
        let mut seen = HashSet::new();
        let mut order = Vec::with_capacity(sources.out.spec.stubs.len());
        for stub in &sources.out.spec.stubs {
            if !seen.insert(stub.path.clone()) {
                continue;
            }
            order.push(stub.path.clone());
            let title = cx.get_global_ref::<ElecLive>().and_then(|live| sources.out.circuit(live.snap.live()))
                .and_then(|circuit| circuit.comp(&stub.path)).map_or_else(|| stub.path.clone(), |comp| comp.refdes.clone());
            let widget = self.generators.entry(stub.path.clone()).or_insert_with(|| {
                self.generator_spare.take().unwrap_or_else(|| new_card(cx, &self.generator))
            });
            if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                if card.set_source(cx, &sources, stub, title) {
                    cx.widget_action(self.uid, ViewAction::CancelPreview);
                }
                card.set_window(cx, self.scope_settings.window());
            }
        }
        let uid = self.uid;
        self.generators.retain(|stub, widget| {
            if seen.contains(stub) { return true; }
            if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                if card.cancel_drag(cx) { cx.widget_action(uid, ViewAction::CancelPreview); }
            }
            false
        });
        if changed_bench || self.generator_order != order {
            self.generator_order = order;
            cx.widget_tree_mark_dirty(self.uid);
            self.redraw(cx);
        }
        // The bench's scope channels are its cards whenever they change. A host's set_probes in
        // between stands until the bench's list moves.
        if sources.out.probes != self.bench_probes {
            self.bench_probes = sources.out.probes.clone();
            let (cards, skipped) = BenchProbe::from_bench(&self.bench_probes);
            if skipped > 0 { log!("bench {}: {skipped} probe(s) read a current, a power or a difference: no card for those yet", sources.out.name); }
            self.set_probes(cx, cards);
        }
        // A new bench, or a new scenario length, sets the timebase; a user's timebase otherwise stands.
        if changed_bench || sources.out.stop != self.bench_stop {
            self.bench_stop = sources.out.stop;
            self.scope_settings.time_per_div = bench_time_per_div(sources.out.stop);
            self.sync_shared(cx);
            self.redraw(cx);
        }
    }

    fn cancel_generators(&mut self, cx: &mut Cx) {
        for widget in self.generators.values() {
            if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                if card.cancel_drag(cx) { cx.widget_action(self.uid, ViewAction::CancelPreview); }
            }
        }
    }

    /// Route by the label's stub path; each card shows a sequence once. `None` clears all cards.
    pub fn set_refusal(&mut self, cx: &mut Cx, refusal: Option<Refusal>) {
        for widget in self.generators.values() {
            if let Some(mut card) = widget.borrow_mut::<DriveLane>() {
                card.set_refusal(cx, refusal.as_ref());
            }
        }
    }

    /// Retain expectations by net, including nets without a probe yet. Lane membership and
    /// display order never change which net owns a bound.
    pub fn set_expects(&mut self, cx: &mut Cx, expects: Vec<LaneExpect>) {
        self.expects.clear();
        for expect in expects { self.expects.entry(expect.net.clone()).or_default().push(expect); }
        self.stop_on_fail.retain(|(net, name)| self.expects.get(net).is_some_and(|expects| expects.iter().any(|expect| expect.name == *name && TrendQuantity::from_name(&expect.answer).is_some())));
        for (net, entry) in &self.cards {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                let primary = lane.sources().get(lane.primary_source()).and_then(|source| source.channel).and_then(|channel| self.stack.channels.get(channel)).unwrap_or(net);
                lane.set_expects(cx, self.expects.get(primary).map_or(&[][..], Vec::as_slice));
            }
        }
        self.sync_trends(cx);
    }

    /// Keep cards by net name, preserving their settings, feeds and pictures. A net the list gains
    /// blooms: a ghost card becomes the probe's card in place, trace and all. Removed probes fade out.
    /// Brings the lane of `net` into view and lights it as a new lane lights, without the veil: a
    /// probe's card on the sheet or the board opened it here. False when no lane shows `net`.
    pub fn show_lane(&mut self, cx: &mut Cx, net: &str) -> bool {
        if !self.probes.iter().any(|probe| probe.net == net) {
            return false;
        }
        self.reveal = Some(net.to_owned());
        self.scroll_to_reveal(cx);
        self.blooms.insert(net.to_owned(), Bloom { start: Instant::now(), veil: 0.0 });
        self.redraw(cx);
        true
    }

    /// Scrolls the lane `show_lane` asked for into view once it has a place (the Bench drew it).
    fn scroll_to_reveal(&mut self, cx: &mut Cx) {
        let Some(net) = self.reveal.take_if(|net| self.placed.contains_key(net)) else { return };
        let height = self.heights.get(&net).copied().unwrap_or(DEFAULT_LANE_HEIGHT);
        self.scroll_bars.scroll_into_view(cx, Rect { pos: self.placed[&net], size: dvec2(1.0, height) });
    }

    pub fn set_probes(&mut self, cx: &mut Cx, probes: Vec<BenchProbe>) {
        let mut seen = HashSet::new();
        let probes: Vec<BenchProbe> = probes.into_iter().filter(|probe| seen.insert(probe.net.clone())).collect();
        let now = Instant::now();
        for probe in &probes {
            if !self.probes.iter().any(|old| old.net == probe.net) {
                let veil = if self.ghosts.iter().any(|ghost| ghost.net == probe.net) { GHOST_VEIL } else { 1.0 };
                self.blooms.insert(probe.net.clone(), Bloom { start: now, veil });
            }
        }
        for (at, probe) in self.probes.iter().enumerate() {
            if !seen.contains(&probe.net) && !self.ghosts.iter().any(|ghost| ghost.net == probe.net) {
                let state = GhostState::Leaving(now);
                self.ghosts.push(Ghost { net: probe.net.clone(), label: probe.label.clone(), at: Some(at), state });
            }
        }
        self.ghosts.retain(|ghost| !seen.contains(&ghost.net));
        let generation = self.stack.generation;
        if !self.probes.iter().map(|p| &p.net).eq(probes.iter().map(|p| &p.net)) {
            let zone_net = self.scope_settings.trigger.zone.and_then(|zone| self.stack.channels.get(zone.lane)).cloned();
            self.stack.sync(&probes);
            self.scope_settings.trigger.zone = self.scope_settings.trigger.zone.and_then(|zone| {
                let lane = zone_net.as_ref().and_then(|net| self.stack.channels.iter().position(|name| name == net))?;
                Some(super::acquire::Zone { lane, ..zone })
            });
        }
        let gesture = cx.global::<PeekGesture>();
        gesture.channels.clone_from(&self.stack.channels);
        gesture.hues.clone_from(&self.stack.hues);
        self.probes = probes;
        self.heights.retain(|net, _| seen.contains(net));
        if self.resizing.as_ref().is_some_and(|net| !seen.contains(net)) { self.resizing = None; }
        if self.stack.generation != generation { self.cancel_delay(); }
        self.delays.retain(|kept| seen.contains(&kept.bracket.from.0) && seen.contains(&kept.bracket.to.0));
        self.pins.retain(|pin| seen.contains(&pin.net));
        if self.trigger_net.as_ref().is_none_or(|net| !seen.contains(net)) { self.trigger_net = self.stack.channels.first().cloned(); }
        for net in &self.stack.channels { if let Some(entry) = self.cards.get_mut(net) { entry.host.stop(); } }
        if self.probes.is_empty() { self.host.stop(); self.installed = None; self.installed_see_as = None; }
        self.rebuild_shown();
        let ghosts = &self.ghosts;
        self.cards.retain(|net, _| seen.contains(net) || ghosts.iter().any(|ghost| ghost.net == *net));
        self.blooms.retain(|net, _| seen.contains(net));
        if self.probes.is_empty() {
            self.scroll_bars.set_scroll_pos(cx, dvec2(0.0, 0.0));
        }
        self.sync_cards(cx);
        self.sync_trends(cx);
        self.refresh_trigger_summary();
        self.push_delays(cx);
        cx.widget_tree_mark_dirty(self.uid);
        self.redraw(cx);
    }

    /// Rebuild only on a layout/ghost edit, never during an unchanged draw.
    fn rebuild_shown(&mut self) {
        let summary: Vec<_> = self.stack.display.iter().filter(|net| overlay_host(&self.stack, self.tag_drag.as_ref(), net).is_none()).filter_map(|net| {
            let probe = self.probes.iter().find(|probe| &probe.net == net)?;
            let guest = overlay_guest(&self.stack, self.tag_drag.as_ref(), net).and_then(|net| self.probes.iter().find(|probe| probe.net == net));
            let label = guest.map_or_else(|| probe.label.clone(), |guest| format!("{} + {}", probe.label.lines().next().unwrap_or(""), guest.label.lines().next().unwrap_or("")));
            Some((probe.net.as_str(), label))
        }).collect();
        self.semantic_summary.borrow_mut().lanes(summary.iter().map(|(net, label)| (*net, label.as_str())));
        self.visible.clear();
        for net in &self.stack.display {
            if overlay_host(&self.stack, self.tag_drag.as_ref(), net).is_some()
                || self.tag_drag.as_ref().is_some_and(|drag| !drag.over && drag.net == *net) { continue; }
            if let Some(probe) = self.probes.iter().find(|p| p.net == *net) { self.visible.push(probe.clone()); }
        }
        for ghost in &self.ghosts {
            if self.probes.iter().any(|p| p.net == ghost.net) { continue; }
            let probe = BenchProbe { net: ghost.net.clone(), label: ghost.label.clone() };
            let at = ghost.at.unwrap_or(self.visible.len()).min(self.visible.len());
            self.visible.insert(at, probe);
        }
    }

    fn sync_cards(&mut self, cx: &mut Cx) {
        let shown = self.visible.clone();
        let new_height = new_lane_height(self.stack.channels.len());
        let Some(template) = &self.template else { return };
        let spare = &mut self.spare;
        for probe in &shown {
            let entry = self.cards.entry(probe.net.clone()).or_insert_with(|| {
                let widget = spare.take().unwrap_or_else(|| new_card(cx, template));
                if let Some(mut card) = widget.borrow_mut::<SignalLane>() {
                    card.set_settings(cx, initial_settings(&probe.label));
                    card.set_sources(&[super::scope::LaneSource { side: fab_elec_bench::source::Side::Sim, name: "sim".into(), channel: None }]);
                    let power = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.snap.live().units.iter().any(|unit| unit.circuit.net(&probe.net).is_some_and(|n| n.kind == NetKind::Power)));
                    card.set_collapsed(cx, power);
                }
                cx.widget_tree_insert_child(self.uid, LiveId::from_str(&probe.net), widget.clone());
                let mut host = ScopeHost::default();
                host.set_latency_enabled(false);
                BenchLane {
                    widget,
                    visible: false,
                    primed: false,
                    reveal_at: None,
                    setup: None,
                    channel: None,
                    host,
                    start_failed: false,
                    measurement_generation: 0,
                    bracket_generation: 0,
                    preview_setup: None,
                    row: None,
                    hits: None,
                    spectrum_hits: None,
                    spectrum_pending: false,
                    scratch: StillScratch::default(),
                    shown: None,
                    still_pending: false,
                }
            });
            if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                card.set_channel_label(cx, 0, &probe.label);
                entry.channel = self.stack.channels.iter().position(|net| *net == probe.net);
                // A departing ghost keeps its picture; a re-added probe starts at the default.
                if entry.channel.is_some() { card.set_height(cx, self.heights.get(&probe.net).copied().unwrap_or(new_height)); }
                card.set_delay_enabled(entry.channel.is_some());
                card.set_tag_ghost(cx, self.ghosts.iter().any(|ghost| ghost.net == probe.net));
                let channel = entry.channel.unwrap_or(self.stack.channels.len());
                let hue = self.stack.hue_of(&probe.net).unwrap_or_else(|| self.stack.next_hue());
                let guest = overlay_guest(&self.stack, self.tag_drag.as_ref(), &probe.net);
                let trigger = self.trigger_net.as_deref() == Some(probe.net.as_str()) || guest.is_some_and(|net| self.trigger_net.as_deref() == Some(net));
                card.set_identity(cx, &probe.net, channel, hue, trigger);
                if let Some(guest) = guest.and_then(|net| self.probes.iter().find(|probe| probe.net == net)) {
                    card.set_sources(&[
                        super::scope::LaneSource { side: Side::Sim, name: probe.label.clone(), channel: entry.channel },
                        super::scope::LaneSource { side: Side::Sim, name: guest.label.clone(), channel: self.stack.channels.iter().position(|net| *net == guest.net) },
                    ]);
                } else { card.set_sources(&[super::scope::LaneSource { side: Side::Sim, name: "sim".into(), channel: None }]); }
                card.set_overlay_preview(cx, self.tag_drag.as_ref().is_some_and(|drag| drag.target.as_deref() == Some(probe.net.as_str())));
                card.attach_summary(self.semantic_summary.clone());
                card.sync_time(cx, &self.scope_settings);
                card.set_bracket(cx, self.bracket);
                let primary = card.sources().get(card.primary_source()).and_then(|source| source.channel).and_then(|channel| self.stack.channels.get(channel)).unwrap_or(&probe.net);
                card.set_expects(cx, self.expects.get(primary).map_or(&[][..], Vec::as_slice));
                card.set_pins(cx, self.pins.iter().filter(|pin| pin.net == *primary).map(|pin| pin.quantity).collect());
                card.set_stop_on_fail(cx, self.stop_on_fail.iter().filter(|(kept, _)| kept == primary).map(|(_, name)| name.clone()).collect());
            }
        }
        self.sync_overlay_routes(cx);
    }

    fn sync_overlay_routes(&mut self, cx: &mut Cx) {
        let mut routes = HashMap::new();
        for net in &self.stack.channels {
            if let Some(host) = overlay_host(&self.stack, self.tag_drag.as_ref(), net) {
                if let Some(entry) = self.cards.get(host) { routes.insert(net.clone(), entry.widget.clone()); }
            }
        }
        if routes.len() != self.overlay_routes.len() || routes.iter().any(|(net, widget)| self.overlay_routes.get(net).is_none_or(|old| old.widget_uid() != widget.widget_uid())) {
            for entry in self.cards.values_mut() { entry.shown = None; entry.still_pending = false; entry.spectrum_pending = false; }
            self.measurement_generation = 0;
            if !cx.next_frame_is_pending(self.next_frame) { self.next_frame = cx.new_next_frame(); }
        }
        for (net, entry) in &mut self.cards { entry.channel = self.stack.channels.iter().position(|channel| channel == net); }
        self.overlay_routes = routes;
    }

    fn sync_shared(&mut self, cx: &mut Cx) {
        self.refresh_trigger_summary();
        self.sync_fit_period();
        for (net, entry) in &mut self.cards {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                entry.channel = self.stack.channels.iter().position(|n| n == net);
                lane.set_delay_enabled(entry.channel.is_some());
                let channel = entry.channel.unwrap_or(self.stack.channels.len());
                let hue = self.stack.hue_of(net).unwrap_or_else(|| self.stack.next_hue());
                let guest = overlay_guest(&self.stack, self.tag_drag.as_ref(), net);
                let trigger = self.trigger_net.as_ref() == Some(net) || guest.is_some_and(|net| self.trigger_net.as_deref() == Some(net));
                lane.set_identity(cx, net, channel, hue, trigger);
                let primary = lane.sources().get(lane.primary_source()).and_then(|source| source.channel).and_then(|channel| self.stack.channels.get(channel)).unwrap_or(net);
                lane.set_expects(cx, self.expects.get(primary).map_or(&[][..], Vec::as_slice));
                lane.sync_time(cx, &self.scope_settings);
            }
        }
        let window = self.scope_settings.window();
        for widget in self.generators.values() {
            if let Some(mut lane) = widget.borrow_mut::<DriveLane>() { lane.set_window(cx, window); }
        }
        if !self.scope_settings.running { self.host.stop(); }
    }

    fn trend_pins(&self) -> Vec<(usize, TrendQuantity)> {
        self.pins.iter().filter_map(|pin| self.stack.channels.iter().position(|net| *net == pin.net).map(|lane| (lane, pin.quantity))).collect()
    }

    fn trend_bounds(&self) -> (Vec<TrendBound>, Vec<usize>) {
        let mut bounds = Vec::new();
        let mut armed = Vec::new();
        for (lane, net) in self.stack.channels.iter().enumerate() {
            for expect in self.expects.get(net).into_iter().flatten() {
                let Some(quantity) = TrendQuantity::from_name(&expect.answer) else { continue };
                if self.stop_on_fail.contains(&(net.clone(), expect.name.clone())) { armed.push(bounds.len()); }
                bounds.push(TrendBound { pin: (lane, quantity), min: expect.min, max: expect.max });
            }
        }
        (bounds, armed)
    }

    fn sync_trends(&mut self, cx: &mut Cx) {
        let (bounds, armed) = self.trend_bounds();
        if self.bounds != bounds || self.stop_bounds != armed {
            self.stop_pending = true; self.violation_jump = None;
            self.bounds = bounds; self.stop_bounds = armed;
        }
        for (net, entry) in &self.cards {
            if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                let primary = card.sources().get(card.primary_source()).and_then(|source| source.channel).and_then(|channel| self.stack.channels.get(channel)).unwrap_or(net);
                card.set_pins(cx, self.pins.iter().filter(|pin| pin.net == *primary).map(|pin| pin.quantity).collect());
                card.set_stop_on_fail(cx, self.stop_on_fail.iter().filter(|(kept, _)| kept == primary).map(|(_, name)| name.clone()).collect());
            }
        }
        let rows = self.pins.iter().filter_map(|pin| {
            let lane = self.stack.channels.iter().position(|net| *net == pin.net)?;
            let probe = self.probes.iter().find(|probe| probe.net == pin.net)?;
            Some(TrendRow { lane, quantity: pin.quantity, label: format!("{} {}", pin.quantity.name(), probe.label) })
        }).collect();
        if let Some(mut strip) = self.strip_widget(cx).borrow_mut::<TimeStrip>() { strip.set_trend_rows(cx, rows); }
        self.sync_record(cx);
        // The existing event pickup retries a busy pin slot, including on a paused source.
        self.events_pending = true;
        self.redraw(cx);
    }

    /// Stills, sparks and strip rows share the generation's net-to-signal mapping.
    fn index_record_rows(&mut self, cx: &Cx) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        let index = &live.sim.index;
        // By identity, not generation: the empty numbering before the first lowering and a design's first
        // lowering can share a generation, and a row looked up in the empty one would stay None.
        if !self.rows_index.as_ref().is_some_and(|known| Arc::ptr_eq(known, index)) {
            self.rows_index = Some(index.clone());
            for entry in self.cards.values_mut() { entry.row = None; }
        }
        for (net, entry) in &mut self.cards {
            if entry.row.is_none() {
                let row = live.snap.live().units.iter()
                    .find_map(|unit| unit.circuit.net(net).map(|net| NetRef { board: unit.id, net: net.id }))
                    .and_then(|net| index.net(net));
                entry.row = Some((index.generation, row));
            }
        }
    }

    fn sync_record(&mut self, cx: &mut Cx) {
        self.index_record_rows(cx);
        let store = cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.trace.clone()
            .filter(|store| store.generation == live.sim.index.generation));
        let generation = store.as_ref().map(|store| store.generation);
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        let widget = self.strip_widget(cx);
        let Some(mut strip) = widget.borrow_mut::<TimeStrip>() else { return };
        let rows = || self.visible.iter().filter_map(|probe| {
            let entry = self.cards.get(&probe.net)?;
            let lane = entry.widget.borrow::<SignalLane>()?;
            if lane.sources().get(lane.primary_source())?.side != Side::Sim { return None; }
            let (indexed, row) = entry.row?;
            if Some(indexed) != generation { return None; }
            let hue = look.channel(lane.settings().channels.first().map_or(0, |channel| channel.color_index));
            Some((row?, probe.label.as_str(), vec4(hue[0], hue[1], hue[2], hue[3])))
        });
        // the strip's rows are probes: the record keeps them whole over the run
        let mut probes: Vec<usize> = rows().map(|(row, ..)| row).collect();
        probes.sort_unstable();
        probes.dedup();
        if probes != self.asked_probes {
            self.asked_probes = probes.clone();
            cx.action(SimAction::Probes { id: BENCH_PROBES, rows: probes });
        }
        if strip.record_rows().iter().map(|row| (row.row, row.label.as_str(), row.hue)).eq(rows()) {
            strip.set_record_store(cx, store);
        } else {
            let rows = rows().map(|(row, label, hue)| RecordRow { row, label: label.to_owned(), hue }).collect();
            strip.set_record(cx, store, rows);
        }
    }

    fn set_bracket(&mut self, cx: &mut Cx, bracket: Option<(f64, f64)>) {
        if self.bracket == bracket { return; }
        self.bracket = bracket;
        if let Some(feed) = self.host.feed() { self.bracket_generation = feed.set_span(bracket); }
        for entry in self.cards.values_mut() {
            if entry.channel.is_none() {
                if let Some(feed) = entry.host.feed() { entry.bracket_generation = feed.set_span(bracket); }
            }
            if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() { card.set_bracket(cx, bracket); }
        }
    }

    fn sync_fit_period(&mut self) {
        let period = self.trigger_net.as_ref().and_then(|net| {
            let host = overlay_host(&self.stack, self.tag_drag.as_ref(), net);
            let lane = self.cards.get(host.unwrap_or(net))?.widget.borrow::<SignalLane>()?;
            lane.measured_source_period(usize::from(host.is_some()))
        });
        for entry in self.cards.values_mut() {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() { lane.set_time_fit_period(period); }
        }
    }

    fn delay_target(&self, cx: &Cx, abs: DVec2) -> Option<usize> {
        if !self.scroll_bars.area().rect(cx).contains(abs) { return None; }
        lane_at(self.stack.display.iter().filter_map(|net| {
            if self.overlay_routes.contains_key(net) { return None; }
            let entry = self.cards.get(net)?;
            Some((entry.channel?, entry.widget.area().rect(cx)))
        }), abs)
    }

    fn cancel_delay(&mut self) {
        self.delay_drag = None;
        for entry in self.cards.values() {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() { lane.cancel_delay_drag(); }
        }
    }

    fn preview_delay(&mut self, cx: &Cx, abs: DVec2) {
        let Some(drag) = self.delay_drag else { return };
        let target = self.delay_target(cx, abs).filter(|&lane| if drag.edit.is_some() { lane == drag.lane } else { lane != drag.lane })
            .and_then(|lane| {
                let net = self.stack.channels.get(lane)?;
                self.cards.get(net)?.widget.borrow::<SignalLane>()?.delay_edge(cx, abs).map(|edge| (lane, edge))
            });
        self.delay_drag = Some(DelayDrag { abs, target, ..drag });
    }

    fn handle_delay(&mut self, cx: &mut Cx, lane: usize, gesture: DelayGesture) {
        match gesture {
            DelayGesture::Start { abs, edge, edit } => {
                if lane >= self.stack.channels.len() || edit.is_some_and(|(index, _)| index >= self.delays.len()) { return; }
                self.delay_drag = Some(DelayDrag { lane, edge, period: 0.0, found: true, edit, abs, target: None });
                // Reserve every lane's preview slot and longest possible pair before any move.
                let capacity = self.stack.channels.iter().map(String::len).max().unwrap_or(0) * 2 + 96;
                if self.delay_label.capacity() < capacity { self.delay_label.reserve(capacity - self.delay_label.len()); }
                self.preview_delay(cx, abs);
            }
            DelayGesture::Move(abs) => {
                if self.delay_drag.is_none_or(|drag| drag.lane != lane) { return; }
                self.preview_delay(cx, abs);
            }
            DelayGesture::End(abs) => {
                if self.delay_drag.is_none_or(|drag| drag.lane != lane) { return; }
                self.preview_delay(cx, abs);
                let drag = self.delay_drag.take().unwrap();
                if let Some((index, end)) = drag.edit {
                    if let Some((_, edge)) = drag.target {
                        let kept = &mut self.delays[index];
                        let endpoint = if end == End::Start { &mut kept.bracket.from } else { &mut kept.bracket.to };
                        endpoint.1 = edge.kind; endpoint.2 = edge.t;
                        kept.typed[usize::from(end == End::Finish)] = false;
                    } else if self.delay_target(cx, abs) != Some(lane) { self.delays.remove(index); }
                } else if let Some((target, edge)) = drag.target.filter(|_| drag.found) {
                    let from = self.stack.channels[lane].clone();
                    let to = self.stack.channels[target].clone();
                    let label = String::with_capacity(from.len() + to.len() + 96);
                    self.delays.push(KeptDelay {
                        key: WidgetUid::new().0, typed: [false; 2],
                        bracket: DelayBracket { from: (from, drag.edge.kind, drag.edge.t), to: (to, edge.kind, edge.t) },
                        edges: [Some(drag.edge), Some(edge)], periods: [drag.period, 0.0], label, upper: End::Start,
                    });
                }
            }
            DelayGesture::Remove(index) => {
                let editing = self.delay_drag.and_then(|drag| drag.edit.map(|(index, _)| index));
                self.cancel_delay();
                if let Some(index) = editing.or(index).filter(|&index| index < self.delays.len()) { self.delays.remove(index); }
            }
        }
        self.follow_delays(cx);
        self.push_delays(cx);
        self.redraw(cx);
    }

    /// Only feature lists and measured periods, never waveform samples or worker locks.
    fn follow_delays(&mut self, cx: &mut Cx) {
        let mut changed = false;
        if let Some(drag) = self.delay_drag.as_mut().filter(|drag| drag.edit.is_none()) {
            let edge = self.stack.channels.get(drag.lane).and_then(|net| self.cards.get(net))
                .and_then(|entry| entry.widget.borrow::<SignalLane>())
                .and_then(|lane| lane.follow_delay(drag.edge.kind, drag.edge.t, &mut drag.period));
            changed |= drag.found != edge.is_some() || edge.is_some_and(|edge| edge != drag.edge);
            drag.found = edge.is_some();
            if let Some(edge) = edge { drag.edge = edge; }
        }
        for kept in &mut self.delays {
            let before = kept.edges;
            for i in 0..2 {
                if kept.typed[i] { continue; }
                let endpoint = if i == 0 { &mut kept.bracket.from } else { &mut kept.bracket.to };
                kept.edges[i] = self.cards.get(&endpoint.0).and_then(|entry| entry.widget.borrow::<SignalLane>())
                    .and_then(|lane| lane.follow_delay(endpoint.1, endpoint.2, &mut kept.periods[i]));
                if let Some(edge) = kept.edges[i] { endpoint.2 = edge.t; }
            }
            if before != kept.edges || kept.label.is_empty() {
                let (from, to) = (&kept.bracket.from, &kept.bracket.to);
                write_delay_label(&mut kept.label, &from.0, kept.edges[0].map(|edge| edge.kind),
                    &to.0, kept.edges[1].map(|edge| edge.kind), kept.edges[0].zip(kept.edges[1]).map(|(a, b)| b.t - a.t));
                changed = true;
            }
        }
        if changed { self.redraw(cx); }
    }

    fn push_delays(&mut self, cx: &mut Cx) {
        let drag = self.delay_drag;
        let capacity = self.stack.channels.iter().map(String::len).max().unwrap_or(0) * 2 + 96;
        if let Some(drag) = drag {
            if let Some((index, end)) = drag.edit {
                let kept = &self.delays[index];
                let mut edges = kept.edges;
                edges[usize::from(end == End::Finish)] = drag.target.map(|(_, edge)| edge);
                write_delay_label(&mut self.delay_label, &kept.bracket.from.0, edges[0].map(|edge| edge.kind),
                    &kept.bracket.to.0, edges[1].map(|edge| edge.kind), edges[0].zip(edges[1]).map(|(a, b)| b.t - a.t));
            } else {
                let target = drag.target.map(|(lane, _)| self.stack.channels[lane].as_str()).unwrap_or("");
                write_delay_label(&mut self.delay_label, &self.stack.channels[drag.lane], drag.found.then_some(drag.edge.kind), target,
                    drag.target.map(|(_, edge)| edge.kind), drag.target.filter(|_| drag.found).map(|(_, edge)| edge.t - drag.edge.t));
            }
        }
        let display = &self.stack.display;
        let upper = |from: &str, to: &str| {
            let a = display.iter().position(|net| net == from);
            let b = display.iter().position(|net| net == to);
            if a <= b { End::Start } else { End::Finish }
        };
        for kept in &mut self.delays { kept.upper = upper(&kept.bracket.from.0, &kept.bracket.to.0); }
        let preview_upper = drag.and_then(|drag| drag.target.map(|(to, _)| upper(&self.stack.channels[drag.lane], &self.stack.channels[to])));
        for (net, entry) in &self.cards {
            let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() else { continue };
            lane.prepare_delays(self.delays.len() + usize::from(drag.is_some()), capacity);
            for (index, kept) in self.delays.iter().enumerate() {
                let (from, to) = (&kept.bracket.from.0, &kept.bracket.to.0);
                let end = if net == from { Some(End::Start) } else if net == to { Some(End::Finish) } else { None };
                let editing = drag.filter(|drag| drag.edit.is_some_and(|(i, _)| i == index));
                let mut edge = end.and_then(|end| kept.edges[usize::from(end == End::Finish)]);
                if let Some(drag) = editing.filter(|drag| drag.edit.map(|(_, end)| end) == end) { edge = drag.target.map(|(_, edge)| edge); }
                let label: &str = if end == Some(kept.upper) { if editing.is_some() { &self.delay_label } else { &kept.label } } else { "" };
                lane.set_delay_mark(cx, index, end, edge, label);
            }
            if let Some(drag) = drag {
                let from = &self.stack.channels[drag.lane];
                let to = drag.target.map(|(lane, _)| self.stack.channels[lane].as_str());
                let end = if drag.edit.is_some() { None } else if net == from { Some(End::Start) }
                    else if to == Some(net.as_str()) { Some(End::Finish) } else { None };
                let edge = match end { Some(End::Start) => drag.found.then_some(drag.edge), Some(End::Finish) => drag.target.map(|(_, edge)| edge), _ => None };
                let label = if end.is_some() && end == preview_upper { self.delay_label.as_str() } else { "" };
                lane.set_delay_mark(cx, self.delays.len(), end, edge, label);
            }
        }
    }

    /// Compare small maps in channel order. Allocation and solver-name resolution happen only on
    /// membership/configuration edits; a reorder changes only the cards whose visibility changes.
    fn sync_feed(&mut self, cx: &mut Cx) {
        if self.stack.channels.is_empty() || !self.scope_settings.running { return; }
        let trigger_channel = self.trigger_net.as_ref().and_then(|net| self.stack.channels.iter().position(|n| n == net)).unwrap_or(0);
        let mut changed = self.setups.len() != self.stack.channels.len();
        let mut source_changed = changed;
        for (i, net) in self.stack.channels.iter().enumerate() {
            let Some(entry) = self.cards.get(overlay_host(&self.stack, self.tag_drag.as_ref(), net).unwrap_or(net)) else { return };
            let Some(lane) = entry.widget.borrow::<SignalLane>() else { return };
            let Some(setup) = entry.setup else { return };
            let setup = LaneSetup { drawn: entry.visible || !entry.primed, ..setup };
            if i == trigger_channel {
                if let Some(channel) = lane.settings().channels.first() { self.scope_settings.channels[0].volts_per_div = channel.volts_per_div; }
            }
            if let Some(old) = self.setups.get_mut(i) {
                source_changed |= old.map != setup.map || old.spectrum != setup.spectrum;
                changed |= *old != setup; *old = setup;
            }
            else { self.setups.push(setup); }
        }
        self.setups.truncate(self.stack.channels.len());
        let settings = &self.scope_settings;
        let configuration = (settings.trigger(), trigger_channel, settings.window(), settings.auto_after(), settings.trigger.qualify, settings.trigger.zone);
        source_changed |= self.configuration != Some(configuration);
        changed |= source_changed;
        // Edits keep the feed and worker: one atomic check per point, a short lock per retune,
        // and sample storage resized only when its length changes.
        if self.installed != Some(self.stack.generation) || !self.host.running() || self.installed_see_as != Some(self.see_as) {
            if self.see_as == SeeAs::BenchScope { self.sync_source(cx, true); }
            let channels = self.stack.channels.iter().enumerate().map(|(i, net)| SimChannel::voltage(ChanId(i as u32), NetProbe { pos: probe_node(cx, net), neg: None, scale: 1.0 })).collect();
            let map = self.setups[trigger_channel].map;
            let config = SimScopeConfig { plot: (map.w, map.h), volts: (map.v_top - map.v_px * map.h as f64, map.v_top),
                trigger: configuration.0, qualifier: configuration.4, zone: configuration.5, window: configuration.2, auto: configuration.3, frame_every: Duration::from_millis(8) };
            match SimScopeFeed::show_lanes(cx, channels, config, trigger_channel, self.setups.clone(), self.limited.clone()) {
                Ok(feed) => {
                    self.bracket_generation = feed.set_span(self.bracket);
                    feed.set_trends(&self.trend_pins());
                    feed.set_bounds(&self.bounds);
                    self.stop_pending = true;
                    self.violation_stop_seen = None; self.violation_jump = None;
                    self.host.install(feed);
                    if let Some(mut strip) = self.strip_widget.as_ref().and_then(|widget| widget.borrow_mut::<TimeStrip>()) {
                        if strip.waiting_for_trigger() { strip.arm_next_trigger(cx, 0); }
                    }
                    self.installed = Some(self.stack.generation);
                    self.installed_see_as = Some(self.see_as);
                    self.configuration = Some(configuration);
                    self.measurement_generation = 0;
                    self.start_failed = false;
                    if self.see_as == SeeAs::BenchScope { self.source_stats = self.host.stats(); self.source_stats_at = Some(Instant::now()); }
                    self.sync_source(cx, self.see_as == SeeAs::Exact);
                }
                Err(error) => { if !self.start_failed { log!("bench instruments: could not start lanes: {error:?}"); } self.start_failed = true; }
            }
        } else if changed {
            if source_changed && self.see_as == SeeAs::BenchScope {
                self.sync_source(cx, true);
                if let Some(spec) = self.limited.clone() { self.host.set_limited(spec); }
            }
            self.host.configure_lanes(configuration.0, configuration.4, trigger_channel, configuration.2, self.setups.clone(), configuration.3, configuration.5);
            self.configuration = Some(configuration);
            if source_changed && self.see_as == SeeAs::Exact { self.sync_source(cx, true); }
        }
    }

    fn sync_source(&mut self, cx: &mut Cx, configuration_changed: bool) {
        let state = match cx.get_global_ref::<ElecLive>() {
            None => SourceState::Gone,
            Some(live) if compiling_first_image(&live.sim) => SourceState::Compiling,
            Some(live) if !live.sim.playing || !self.scope_settings.running => SourceState::Paused,
            Some(_) if self.host.stats().unresolved => SourceState::Unsimulated,
            Some(live) if live.sim.ratio.is_none() => SourceState::Waiting,
            Some(_) => SourceState::Live,
        };
        if self.source_chip.as_ref().is_some_and(|chip| chip.state == state) && !configuration_changed { return; }
        if configuration_changed || self.source_chip.is_none() {
            let channels = self.stack.channels.iter().enumerate().map(|(i, net)| ChannelInfo {
                id: ChanId(i as u32), name: net.clone(), quantity: Quantity::Volt,
            }).collect();
            let caps = Arc::new(match self.see_as {
                SeeAs::Exact => simulation_caps(channels),
                SeeAs::BenchScope => fab_elec_instr::Limits::bench_scope().caps(channels),
            });
            let trigger_channel = self.trigger_net.as_ref().and_then(|net| self.stack.channels.iter().position(|n| n == net)).unwrap_or(0);
            let trigger = self.scope_settings.trigger();
            let window = self.scope_settings.window();
            let span = window.pre + window.post;
            let trigger = TriggerSpec { kind: self.scope_settings.trigger.qualify.kind(), channel: ChanId(trigger_channel as u32), level: trigger.level,
                hyst: trigger.hyst, slope: trigger.slope, holdoff: trigger.holdoff, run: self.scope_settings.auto_after().map_or(TriggerRun::Normal, TriggerRun::Auto) };
            let resolved = resolve(&caps, &Setup {
                channels: self.setups.iter().enumerate().map(|(i, lane)| ChannelSetup {
                    id: ChanId(i as u32), on: true, window: (lane.map.v_top - lane.map.v_px * lane.map.h as f64, lane.map.v_top),
                    coupling: Coupling::Dc, probe: Probe::default(), bw_limit: None, invert: false,
                }).collect(),
                time: TimeSetup { span, pre: window.pre, rate: RateWant::Auto { per_px: 2.0 }, depth: DepthWant::Auto, plot_px: self.setups.first().map_or(1, |s| s.map.w) },
                trigger: trigger.clone(),
                mode: if self.see_as == SeeAs::BenchScope { ModeWant::Block } else { ModeWant::Stream }, drives: Vec::new(),
            }, None);
            let resolved = Arc::new(resolved);
            self.limited = (self.see_as == SeeAs::BenchScope).then(|| LimitedSpec { bits: caps.channels.iter().map(|c| c.bits).collect(),
                resolved: resolved.clone(), trigger, pre: window.pre, block_period: caps.modes.block_period });
            for (i, net) in self.stack.channels.iter().enumerate() {
                if let Some(mut lane) = self.cards.get(net).and_then(|e| e.widget.borrow_mut::<SignalLane>()) {
                    lane.set_capabilities(cx, caps.clone(), Some(resolved.clone()), ChanId(i as u32));
                }
            }
            self.source_chip = Some(SourceChip { info: SourceInfo { id: SourceId(0), side: Side::Sim, name: if self.see_as == SeeAs::BenchScope { "Bench scope" } else { "Simulation" }.into(), serial: None, clock: ClockKind::Simulated },
                state, caps, resolved: Some(resolved), trigger_kind: self.scope_settings.trigger.qualify.kind(), note: None });
        }
        // a probe the run does not simulate is named, never called stale: `1 probe: net not
        // simulated (I2C clock)`; O(cards)
        let note = (state == SourceState::Unsimulated).then(|| {
            let names: Vec<&str> = self.stack.channels.iter().filter(|net| self.cards.get(*net).is_some_and(|e| e.host.stats().unresolved))
                .map(|net| self.probes.iter().find(|p| &p.net == net).map_or(net.as_str(), |p| p.label.as_str())).collect();
            match names.len() {
                0 => "a probe: net not simulated".to_owned(),
                1 => format!("1 probe: net not simulated ({})", names[0]),
                n => format!("{n} probes: nets not simulated ({})", names.join(", ")),
            }
        });
        let bar = self.source_bar_widget(cx);
        let chip = self.source_chip.as_mut().unwrap();
        chip.state = state;
        chip.note = note;
        if let Some(mut source) = bar.borrow_mut::<SourceBar>() {
            let changed = source.set_see_as(self.see_as);
            if source.set_sources(std::slice::from_ref(chip)) || changed { source.redraw(cx); }
            if configuration_changed && self.see_as == SeeAs::BenchScope { source.set_answers(cx, SourceAnswers::default()); }
        }
        if matches!(state, SourceState::Live | SourceState::Waiting | SourceState::Unsimulated) {
            if self.source_timer.is_empty() {
                self.source_stats = self.host.stats(); self.source_stats_at = Some(Instant::now());
                self.source_timer = cx.start_interval(1.0);
            }
        } else { cx.stop_timer(self.source_timer); self.source_timer = Timer::empty(); }
    }

    fn update_source_answers(&mut self, cx: &mut Cx) {
        let stats = self.host.stats();
        let now = Instant::now();
        let elapsed = self.source_stats_at.map_or(1.0, |at| now.duration_since(at).as_secs_f64());
        // the rate the transport reads out: the host's over its window, not one publication's
        let ratio = cx.get_global_ref::<ElecLive>().and_then(|live| live.sim.status.rate_achieved);
        let answers = SourceAnswers::from_stats(stats, self.source_stats, elapsed, None, ratio);
        self.source_stats = stats; self.source_stats_at = Some(now);
        if let Some(mut bar) = self.source_bar_widget(cx).borrow_mut::<SourceBar>() { bar.set_answers(cx, answers); }
        self.sync_source(cx, false);
    }

    fn handle_strip_actions(&mut self, cx: &mut Cx, actions: &Actions, strip: &WidgetRef) {
        for action in actions.filter_widget_actions_cast::<TimeStripAction>(strip.widget_uid()) {
            match action {
                TimeStripAction::StopAtTrigger => {
                    let events = if self.host.running() { self.host.feed().map_or(0, SimScopeFeed::trigger_events) } else { 0 };
                    if let Some(mut strip) = strip.borrow_mut::<TimeStrip>() { strip.arm_next_trigger(cx, events); }
                }
                TimeStripAction::Cursor(c) => {
                    if c.playing && !self.stop_playing && !self.stop_resume_armed {
                        self.stop_pending = true;
                        self.stop_resume_armed = true;
                        if let Some(feed) = self.host.feed() {
                            self.stop_pending = !feed.set_bounds(&self.bounds) || !feed.arm_stop_on_violation(&self.stop_bounds);
                        }
                    }
                    if !c.playing { self.stop_resume_armed = false; self.violation_jump = None; }
                    let fw = if c.playing { None } else { self.fw_events.as_ref().map(|events| fw_at(events.as_ref(), c.t)) };
                    let c = TimeCursor { fw, ..c };
                    if let Some(mut lane) = self.fw_lane_widget.as_ref().and_then(|widget| widget.borrow_mut::<FwLane>()) {
                        lane.set_cursor(cx, c.fw());
                    }
                    cx.action(ViewAction::Cursor(c));
                    if c.playing && !self.scope_settings.running {
                        self.scope_settings.running = true;
                        self.sync_shared(cx);
                    }
                    if c.rate != self.rate {
                        self.rate = c.rate;
                        cx.action(SimAction::Cmd(SimCmd::SetRate(c.rate)));
                    }
                    if self.last_heard.map(|(_, playing)| playing) != Some(c.playing) {
                        cx.action(SimAction::Cmd(if c.playing { SimCmd::Play } else { SimCmd::Pause }));
                    }
                    self.following = c.playing || c.t >= self.strip_end();
                }
                TimeStripAction::Ghost(g) => {
                    self.ghost = Some(g);
                    for entry in self.cards.values_mut() {
                        if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                            card.set_ghost_on_top(cx, g == RunGhost::OnTop);
                        }
                    }
                }
                TimeStripAction::StepAhead(dt) => cx.action(SimAction::Cmd(SimCmd::Step(dt))),
                TimeStripAction::Start | TimeStripAction::Record(_) | TimeStripAction::Loop(_) | TimeStripAction::Stop | TimeStripAction::None => {}
            }
        }
    }

    fn handle_source_actions(&mut self, cx: &mut Cx, actions: &Actions, bar: &WidgetRef) {
        for action in actions.filter_widget_actions_cast::<SourceBarAction>(bar.widget_uid()) {
            match action {
                SourceBarAction::SeeAs(see_as) => {
                    if self.see_as == see_as { continue; }
                    self.see_as = see_as;
                    self.limited = None;
                    self.installed = None;
                    self.installed_see_as = None;
                    if let Some(mut source) = bar.borrow_mut::<SourceBar>() {
                        source.set_see_as(see_as);
                        source.set_answers(cx, SourceAnswers::default());
                    }
                    if self.stack.channels.is_empty() || !self.scope_settings.running { self.sync_source(cx, true); }
                }
                SourceBarAction::Look { persistence, intensity } => {
                    self.scope_settings.persistence = persistence;
                    self.scope_settings.intensity = intensity;
                    self.sync_shared(cx);
                }
                SourceBarAction::Clear => {
                    for entry in self.cards.values_mut() {
                        if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() { lane.phosphor().clear(cx); lane.spectrum_view().clear(cx); }
                        entry.primed = false;
                        // Keep the still key: a paused picture stays blank until the cursor moves.
                        entry.still_pending = false;
                        entry.spectrum_pending = false;
                    }
                }
                SourceBarAction::None => {}
            }
            self.redraw(cx);
        }
    }

    fn begin_trigger_drag(&mut self, cx: &mut Cx, net: String, abs: DVec2, grab: f64) {
        let tip = self.tip.get_or_insert_with(|| cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.probe_tip.as_object().into()))).clone();
        let slope = match self.scope_settings.trigger.slope { Slope::Rising => "↑", Slope::Falling => "↓", Slope::Either => "↕" };
        tip.label(cx, ids!(label)).set_text(cx, &format!("‹T {slope}"));
        let ink = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light).channel(self.stack.hue_of(&net).unwrap_or(0));
        let color = vec4(ink[0], ink[1], ink[2], ink[3]);
        let mut swatch = tip.widget(cx, ids!(swatch));
        script_apply_eval!(cx, swatch, { draw_bg +: { color: #(color) } });
        let level = self.scope_settings.trigger.level;
        cx.start_dragging_with(vec![DragItem::String { value: net.clone(), internal_id: Some(TRIGGER_DRAG) }], tip, dvec2(10.0, 10.0));
        let mut drag = TriggerDrag { net, original: level, level, grab };
        let _ = self.preview_trigger_drag(cx, &mut drag, abs);
        self.trigger_drag = Some(drag);
        cx.set_cursor(MouseCursor::Hand);
        self.redraw(cx);
    }

    fn trigger_target(&self, cx: &Cx, abs: DVec2) -> Option<usize> {
        if !self.scroll_bars.area().rect(cx).contains(abs) { return None; }
        self.visible.iter().find_map(|probe| {
            let lane = self.cards.get(&probe.net)?.widget.borrow::<SignalLane>()?;
            if !lane.area().rect(cx).contains(abs) { return None; }
            let trigger_here = self.trigger_net.as_ref().filter(|net| **net == probe.net
                || overlay_host(&self.stack, self.tag_drag.as_ref(), net) == Some(probe.net.as_str()));
            trigger_here.and_then(|net| self.stack.channels.iter().position(|name| name == net))
                .or_else(|| lane.sources().get(lane.primary_source()).and_then(|source| source.channel))
                .or_else(|| self.stack.channels.iter().position(|net| *net == probe.net))
        })
    }

    fn preview_trigger_drag(&mut self, cx: &mut Cx, drag: &mut TriggerDrag, abs: DVec2) -> Option<usize> {
        let target = self.trigger_target(cx, abs);
        if target.is_some_and(|i| self.stack.channels[i] == drag.net) {
            if let Some(lane) = self.cards.get(overlay_host(&self.stack, self.tag_drag.as_ref(), &drag.net).unwrap_or(&drag.net)).and_then(|entry| entry.widget.borrow::<SignalLane>()) {
                drag.level = lane.trigger_level_at(cx, abs, drag.grab);
                self.scope_settings.trigger.level = drag.level;
            }
        } else {
            // Crossing lanes moves the source, preserving the level held at the initial press.
            drag.level = drag.original;
            self.scope_settings.trigger.level = drag.original;
        }
        self.sync_shared(cx);
        for (net, entry) in &self.cards {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                lane.set_trigger_ghost(cx, target.filter(|&i| self.stack.channels[i] == *net
                    || overlay_host(&self.stack, self.tag_drag.as_ref(), &self.stack.channels[i]) == Some(net.as_str())).map(|_| drag.level));
            }
        }
        target
    }

    fn handle_trigger_drag(&mut self, cx: &mut Cx, event: &Event) {
        let Some(mut drag) = self.trigger_drag.take() else { return };
        let ours = |items: &[DragItem]| items.iter().any(|item| matches!(item, DragItem::String { internal_id: Some(id), .. } if *id == TRIGGER_DRAG));
        let drop = match event {
            Event::Drag(e) if ours(&e.items) => {
                let target = self.preview_trigger_drag(cx, &mut drag, e.abs);
                if let DragHit::Drag(hit) = event.drag_hits(cx, self.scroll_bars.area()) {
                    if let Ok(mut response) = hit.response.try_lock() { *response = if target.is_some() { DragResponse::Move } else { DragResponse::None }; }
                }
                self.trigger_drag = Some(drag);
                self.redraw(cx);
                return;
            }
            Event::Drop(e) if ours(&e.items) => {
                let target = self.preview_trigger_drag(cx, &mut drag, e.abs);
                if target.is_some() { let _ = event.drag_hits(cx, self.scroll_bars.area()); }
                target
            }
            Event::DragEnd => None,
            _ => { self.trigger_drag = Some(drag); return; }
        };
        if let Some(index) = drop {
            let net = self.stack.channels[index].clone();
            if let Some(lane) = self.cards.get(overlay_host(&self.stack, self.tag_drag.as_ref(), &net).unwrap_or(&net)).and_then(|entry| entry.widget.borrow::<SignalLane>()) {
                self.scope_settings.trigger.level = lane.clamp_trigger_level(drag.level);
                self.trigger_net = Some(net);
            }
        } else { self.scope_settings.trigger.level = drag.original; }
        for entry in self.cards.values() {
            if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() { lane.set_trigger_ghost(cx, None); }
        }
        self.sync_shared(cx);
        self.redraw(cx);
    }

    fn begin_tag_drag(&mut self, cx: &mut Cx, net: String, _abs: DVec2) {
        let Some(probe) = self.probes.iter().find(|probe| probe.net == net) else { return };
        let tip = self.tag_tip.get_or_insert_with(|| cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.probe_tag_tip.as_object().into()))).clone();
        let ink = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light).channel(self.stack.hue_of(&net).unwrap_or(0));
        let (kind, side) = self.cards.get(&net).and_then(|entry| entry.widget.borrow::<SignalLane>()).map_or((TagKind::Volt, LaneSide::Sim), |lane| {
            (TagKind::from_unit(lane.settings().channels.first().map_or("V", |channel| channel.unit.as_str())), lane.sources().get(lane.primary_source()).map_or(LaneSide::Sim, |source| source.side.into()))
        });
        if let Some(mut tag) = tip.borrow_mut::<ProbeTagTip>() {
            tag.set_identity(cx, &probe.label, &net, "", TagStyle { hue: vec4(ink[0], ink[1], ink[2], ink[3]), kind, side, state: TagState::Carried });
        }
        cx.start_dragging_with(vec![DragItem::String { value: net.clone(), internal_id: Some(LANE_DRAG) }], tip, dvec2(14.0, 12.0));
        let original = self.stack.display.clone();
        let pair = self.stack.overlays.iter().find(|(guest, host)| *guest == net || *host == net).map(|(guest, host)| {
            let primary = self.cards.get(host).and_then(|entry| entry.widget.borrow::<SignalLane>()).map_or(0, |lane| lane.primary_source());
            (guest.clone(), host.clone(), primary)
        });
        if let Some((guest, host, _)) = &pair {
            self.stack.separate(&net);
            if host == &net {
                if let Some(at) = self.stack.display.iter().position(|net| net == host) { self.stack.reorder(guest, at); }
            }
        }
        self.tag_drag = Some(TagDrag { net, original, over: true, target: None, target_rect: None, pair });
        self.rebuild_shown();
        self.sync_cards(cx);
        cx.set_cursor(MouseCursor::Grabbing);
        self.redraw(cx);
        self.request_frame(cx);
    }

    fn tag_overlay_target(&self, cx: &Cx, drag: &TagDrag, abs: DVec2) -> Option<(String, Rect, bool)> {
        // Retain the entered hit region while FLIP moves the target out from under the pointer.
        if let (Some(net), Some(rect)) = (&drag.target, drag.target_rect) {
            if rect.contains(abs) && self.stack.channels.contains(net) && self.stack.guest_of(net).is_none()
                && self.cards.get(net).and_then(|entry| entry.widget.borrow::<SignalLane>()).is_some_and(|lane| lane.overlay_plot(cx).is_some()) { return Some((net.clone(), rect, false)); }
        }
        for probe in &self.visible {
            if probe.net == drag.net || !self.stack.channels.contains(&probe.net) { continue; }
            let Some(lane) = self.cards.get(&probe.net).and_then(|entry| entry.widget.borrow::<SignalLane>()) else { continue };
            let Some(plot) = lane.overlay_plot(cx) else { continue };
            if self.stack.guest_of(&probe.net).is_some() {
                if plot.contains(abs) { return Some((probe.net.clone(), plot, true)); }
                continue;
            }
            let rect = Rect { pos: plot.pos + dvec2(0.0, plot.size.y * 0.25), size: dvec2(plot.size.x, plot.size.y * 0.5) };
            if rect.contains(abs) { return Some((probe.net.clone(), rect, false)); }
        }
        None
    }

    fn handle_tag_drag(&mut self, cx: &mut Cx, event: &Event) {
        let Some(mut drag) = self.tag_drag.take() else { return };
        let ours = |items: &[DragItem]| items.iter().any(|item| matches!(item, DragItem::String { internal_id: Some(id), .. } if *id == LANE_DRAG));
        match event {
            Event::Drag(e) if ours(&e.items) => {
                let over = self.scroll_bars.area().rect(cx).contains(e.abs);
                if let DragHit::Drag(hit) = event.drag_hits(cx, self.scroll_bars.area()) {
                    *hit.response.lock().unwrap_or_else(|p| p.into_inner()) = if over { DragResponse::Move } else { DragResponse::None };
                }
                drag.over = over;
                let target = if over { self.tag_overlay_target(cx, &drag, e.abs) } else { None };
                let refused = target.as_ref().is_some_and(|(_, _, full)| *full);
                drag.target = target.as_ref().filter(|(_, _, full)| !*full).map(|(net, _, _)| net.clone());
                drag.target_rect = target.filter(|(_, _, full)| !*full).map(|(_, rect, _)| rect);
                if let Some(mut tip) = self.tag_tip.as_ref().and_then(|tip| tip.borrow_mut::<ProbeTagTip>()) {
                    let reason = if refused { "overlays two already" } else { "" };
                    if tip.answer != reason {
                        let (label, pin, style) = (tip.label.clone(), tip.pin.clone(), tip.style);
                        tip.set_identity(cx, &label, &pin, reason, style);
                    }
                }
                if over && drag.target.is_none() {
                    let mut before = None;
                    for net in &self.stack.display {
                        if net == &drag.net || self.stack.host_of(net).is_some() { continue; }
                        if let Some(entry) = self.cards.get(net) {
                            let rect = entry.widget.area().rect(cx);
                            // Use the unshifted target so an in-flight FLIP cannot oscillate the order.
                            let y = self.placed.get(net).map_or(rect.pos.y, |pos| self.area().rect(cx).pos.y + pos.y - self.scroll_bars.get_scroll_pos().y);
                            if e.abs.y <= y + rect.size.y * 0.5 { before = Some(net.clone()); break; }
                        }
                    }
                    let from = self.stack.display.iter().position(|net| *net == drag.net).unwrap_or(0);
                    let to = before.as_ref().and_then(|net| self.stack.display.iter().position(|name| name == net))
                        .map_or(self.stack.display.len().saturating_sub(1), |at| at - usize::from(from < at));
                    self.stack.reorder(&drag.net, to);
                }
            }
            Event::Drop(e) if ours(&e.items) => {
                if !self.scroll_bars.area().rect(cx).contains(e.abs) {
                    remove_probe(cx, self.uid, (!self.bench_fed).then_some((self.source_bench.as_str(), &self.bench_probes[..])), drag.net);
                } else {
                    let _ = event.drag_hits(cx, self.scroll_bars.area());
                    if let Some((host, _, false)) = self.tag_overlay_target(cx, &drag, e.abs) { self.stack.overlay(&drag.net, &host); }
                }
                self.rebuild_shown();
                self.sync_cards(cx);
                self.redraw(cx);
                return;
            }
            Event::DragEnd => {
                // A successful drop consumed the drag above. A bare end restores the original pair too.
                self.stack.display = drag.original;
                if let Some((guest, host, _)) = &drag.pair { self.stack.overlay(guest, host); }
                self.rebuild_shown();
                self.sync_cards(cx);
                if let Some((_, host, primary)) = &drag.pair {
                    if let Some(mut lane) = self.cards.get(host).and_then(|entry| entry.widget.borrow_mut::<SignalLane>()) {
                        if lane.primary_source() != *primary { lane.swap_sources(cx); }
                    }
                }
                self.sync_shared(cx);
                self.sync_trends(cx);
                self.redraw(cx);
                return;
            }
            _ => { self.tag_drag = Some(drag); return; }
        }
        self.tag_drag = Some(drag);
        self.rebuild_shown();
        self.sync_cards(cx);
        self.redraw(cx);
    }

    fn animating(&self) -> bool {
        !self.ghosts.is_empty() || self.blooms.keys().any(|net| self.cards.get(net).is_some_and(|entry| entry.visible)) || !self.flips.is_empty()
    }

    fn request_frame(&mut self, cx: &mut Cx) {
        let scrub = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.scrub.is_some());
        let wanted = self.carrying.is_some() || self.tag_drag.is_some() || self.host.wants_frame() || self.animating()
            || self.events_pending
            || self.cards.iter().any(|(net, entry)| entry.host.wants_frame() || entry.still_pending || entry.spectrum_pending
                || (scrub && entry.shown.is_some_and(|shown| self.overlay_routes.get(net).unwrap_or(&entry.widget).borrow::<SignalLane>()
                    .is_some_and(|lane| lane.plot_px().is_some_and(|plot| plot != shown.plot) || lane.spectrum_setup() != shown.spectrum))));
        if wanted && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    /// Follow the gesture: the net under the carried probe gets a ghost card, a ghost the probe left
    /// fades out, and a faded or unanswered ghost goes with its card. Returns whether a ghost changed.
    fn update_ghosts(&mut self, cx: &mut Cx) -> bool {
        let now = Instant::now();
        let mut changed = false;
        if self.peek_hover.as_ref().is_some_and(|probe| cx.global::<PeekGesture>().state.open.as_ref().is_none_or(|open| open.net != probe.net)) {
            self.peek_hover = None;
        }
        let hovered = self.peek_hover.as_ref().map(|probe| probe.net.clone()).or_else(|| match self.carrying {
            Some(_) => cx.global::<ProbeGesture>().hovered().map(str::to_owned),
            None => None,
        });
        for ghost in &mut self.ghosts {
            let before = ghost.state;
            let under = hovered.as_deref() == Some(ghost.net.as_str());
            if let Some(probe) = self.peek_hover.as_ref().filter(|probe| probe.net == ghost.net) {
                if ghost.label != probe.label || ghost.at.is_some() {
                    ghost.label.clone_from(&probe.label);
                    ghost.at = None;
                    changed = true;
                }
            }
            ghost.state = match ghost.state {
                GhostState::Hover if !under => GhostState::Leaving(now),
                GhostState::Leaving(_) if under => GhostState::Hover,
                GhostState::Dropped(at) if (now - at).as_secs_f64() > DROP_WAIT => GhostState::Leaving(now),
                state => state,
            };
            changed |= ghost.state != before;
        }
        if let Some(net) = hovered {
            if !self.probes.iter().any(|probe| probe.net == net) && !self.ghosts.iter().any(|ghost| ghost.net == net) {
                let label = self.peek_hover.as_ref().filter(|probe| probe.net == net).map_or_else(|| net.clone(), |probe| probe.label.clone());
                self.ghosts.push(Ghost { label, net, at: None, state: GhostState::Hover });
                changed = true;
            }
        }
        let before = self.ghosts.len();
        self.ghosts.retain(|ghost| !matches!(ghost.state, GhostState::Leaving(at) if (now - at).as_secs_f64() >= GHOST_FADE));
        if self.ghosts.len() != before {
            changed = true;
            let (probes, ghosts) = (&self.probes, &self.ghosts);
            self.cards.retain(|net, _| probes.iter().any(|probe| probe.net == *net) || ghosts.iter().any(|ghost| ghost.net == *net));
            cx.widget_tree_mark_dirty(self.uid);
        }
        if changed { self.rebuild_shown(); }
        changed
    }

    /// End finished blooms, giving their screens back their own look.
    fn update_blooms(&mut self, cx: &mut Cx) {
        let light = base_theme(cx) == BaseTheme::Light;
        let cards = &mut self.cards;
        self.blooms.retain(|net, bloom| {
            if bloom.start.elapsed().as_secs_f64() < BLOOM_GLOW || cards.get(net).is_some_and(|entry| !entry.visible) {
                return true;
            }
            if let Some(mut card) = cards.get_mut(net).and_then(|entry| entry.widget.borrow_mut::<SignalLane>()) {
                let look = card.settings().look(light);
                card.phosphor().set_look(cx, look);
            }
            false
        });
    }

    /// A probe drag from this bench: follow the pointer, and hand a dropped-on ghost over to the
    /// probe list's answer.
    fn handle_carry(&mut self, cx: &mut Cx, event: &Event) {
        let ended = match event {
            Event::Drag(drag) if carries_probe(&drag.items) => {
                if self.carrying.is_some() {
                    self.carrying = Some(drag.abs);
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.refresh_tip(cx);
                    // Read the target again after every pane has handled this move.
                    self.request_frame(cx);
                }
                return;
            }
            Event::Drop(drop) if carries_probe(&drop.items) => Some(true),
            Event::DragEnd => Some(false),
            _ => None,
        };
        if let Some(dropped) = ended {
            if self.carrying.take().is_some() {
                let now = Instant::now();
                for ghost in &mut self.ghosts {
                    if matches!(ghost.state, GhostState::Hover) {
                        ghost.state = if dropped { GhostState::Dropped(now) } else { GhostState::Leaving(now) };
                    }
                }
                self.redraw(cx);
                self.request_frame(cx);
            }
        }
    }

    /// A carried peek previews the appended lane and uses the ordinary bench operation path.
    fn handle_peek(&mut self, cx: &mut Cx, event: &Event) {
        match event {
            Event::Drag(drag) if carries_peek(&drag.items).is_some() => {
                let net = carries_peek(&drag.items).unwrap();
                let open = cx.global::<PeekGesture>().state.open.as_ref().filter(|open| open.net == net).cloned();
                if let DragHit::Drag(hit) = event.drag_hits(cx, self.scroll_bars.area()) {
                    let over = hit.state != DragState::Out && open.is_some();
                    if let Ok(mut response) = hit.response.try_lock() { *response = if over { DragResponse::Copy } else { DragResponse::None }; }
                    self.peek_hover = open.filter(|_| over).map(|open| BenchProbe { net: open.net, label: open.label });
                    self.update_ghosts(cx);
                    self.redraw(cx);
                    self.request_frame(cx);
                }
            }
            Event::Drop(drop) if carries_peek(&drop.items).is_some() => {
                let net = carries_peek(&drop.items).unwrap();
                let open = cx.global::<PeekGesture>().state.open.as_ref().filter(|open| open.net == net).cloned();
                if let DragHit::Drop(_) = event.drag_hits(cx, self.scroll_bars.area()) {
                    if let Some(open) = open {
                        for ghost in &mut self.ghosts {
                            if ghost.net == net { ghost.state = GhostState::Dropped(Instant::now()); }
                        }
                        cx.widget_action(self.uid, ViewAction::Ops {
                            ops: vec![DesignOp::Bench { bench: self.source_bench.clone(), op: BenchOp::AddProbe { of: ProbeOf::v(open.net), label: open.label } }],
                            place: vec![],
                            label: "Probe net".into(),
                        });
                    }
                }
                self.peek_hover = None;
                self.update_ghosts(cx);
                self.redraw(cx);
                self.request_frame(cx);
            }
            Event::DragEnd | Event::WindowLostFocus(_) => {
                if self.peek_hover.take().is_some() {
                    self.update_ghosts(cx);
                    self.redraw(cx);
                    self.request_frame(cx);
                }
            }
            _ => {}
        }
    }

    /// The probe chip: a hand over it, a press past the slop starts the probe drag.
    fn handle_probe_chip(&mut self, cx: &mut Cx, event: &Event) {
        match event.hits(cx, self.draw_probe.area()) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                cx.set_cursor(MouseCursor::Hand);
                if !self.probe_hover {
                    self.probe_hover = true;
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.probe_hover = false;
                self.redraw(cx);
            }
            Hit::FingerDown(down) => {
                self.press = Some(down.abs);
                cx.set_cursor(MouseCursor::Grabbing);
            }
            Hit::FingerMove(moved) => {
                if self.press.is_some_and(|start| (moved.abs - start).length() > DRAG_SLOP) {
                    self.press = None;
                    // The drag owns the pointer now; no hover-out follows.
                    self.probe_hover = false;
                    self.carrying = Some(moved.abs);
                    let tip = self.tag_tip.get_or_insert_with(|| cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.probe_tag_tip.as_object().into()))).clone();
                    self.refresh_tip(cx);
                    cx.start_dragging_with(vec![probe_drag_item()], tip, dvec2(14.0, 12.0));
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.redraw(cx);
                }
            }
            Hit::FingerUp(_) => self.press = None,
            _ => {}
        }
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn outline(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4], width: f64) {
        let (p, s) = (rect.pos, rect.size);
        self.quad(cx, Rect { pos: p, size: dvec2(s.x, width) }, color);
        self.quad(cx, Rect { pos: dvec2(p.x, p.y + s.y - width), size: dvec2(s.x, width) }, color);
        self.quad(cx, Rect { pos: dvec2(p.x, p.y + width), size: dvec2(width, s.y - 2.0 * width) }, color);
        self.quad(cx, Rect { pos: dvec2(p.x + s.x - width, p.y + width), size: dvec2(width, s.y - 2.0 * width) }, color);
    }

    /// A one-pixel dashed outline: a ghost's mark.
    fn dashes(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        const DASH: f64 = 6.0;
        const STEP: f64 = 10.0;
        let (x0, y0) = (rect.pos.x.round(), rect.pos.y.round());
        let (x1, y1) = ((rect.pos.x + rect.size.x).round() - 1.0, (rect.pos.y + rect.size.y).round() - 1.0);
        let mut x = x0;
        while x < x1 {
            let w = DASH.min(x1 - x);
            self.quad(cx, Rect { pos: dvec2(x, y0), size: dvec2(w, 1.0) }, color);
            self.quad(cx, Rect { pos: dvec2(x, y1), size: dvec2(w, 1.0) }, color);
            x += STEP;
        }
        let mut y = y0;
        while y < y1 {
            let h = DASH.min(y1 - y);
            self.quad(cx, Rect { pos: dvec2(x0, y), size: dvec2(1.0, h) }, color);
            self.quad(cx, Rect { pos: dvec2(x1, y), size: dvec2(1.0, h) }, color);
            y += STEP;
        }
    }

    /// A ghost's veil and dashes, or a bloom's lifting veil and fading outline, over a drawn card.
    fn draw_card_marks(&mut self, cx: &mut Cx2d, net: &str, rect: Rect, ink: [f32; 4]) {
        let bg = self.draw_bg.color;
        let now = Instant::now();
        if let Some(state) = self.ghosts.iter().find(|ghost| ghost.net == net).map(|ghost| ghost.state) {
            let veil = match state {
                GhostState::Hover | GhostState::Dropped(_) => GHOST_VEIL,
                GhostState::Leaving(at) => {
                    let t = ((now - at).as_secs_f64() / GHOST_FADE).min(1.0) as f32;
                    GHOST_VEIL + (1.0 - GHOST_VEIL) * t
                }
            };
            self.quad(cx, rect, [bg.x, bg.y, bg.z, veil]);
            self.dashes(cx, rect, [ink[0], ink[1], ink[2], 0.9 * (1.0 - veil) / (1.0 - GHOST_VEIL)]);
        } else if let Some(bloom) = self.blooms.get(net) {
            let t = (now - bloom.start).as_secs_f64();
            let lift = 1.0 - (1.0 - (t / BLOOM_LIFT).min(1.0)).powi(3);
            let veil = bloom.veil * (1.0 - lift as f32);
            if veil > 0.0 {
                self.quad(cx, rect, [bg.x, bg.y, bg.z, veil]);
            }
            let glow = (1.0 - t / BLOOM_GLOW).max(0.0) as f32;
            if glow > 0.0 {
                self.outline(cx, rect, [ink[0], ink[1], ink[2], glow], 2.0);
            }
        }
    }

    fn delay_line(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: [f32; 4], width: f32) {
        let pad = f64::from(width) * 0.5 + 1.0;
        let rect = Rect { pos: dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad), size: dvec2((b.x - a.x).abs() + pad * 2.0, (b.y - a.y).abs() + pad * 2.0) };
        let (a, b) = (a - rect.pos, b - rect.pos);
        self.draw_delay.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_delay.width = width;
        self.draw_delay.seg = vec4(a.x as f32, a.y as f32, b.x as f32, b.y as f32);
        self.draw_delay.draw_abs(cx, rect);
    }

    /// Paint after all cards and their veils, in bench coordinates, across the lane gaps.
    fn draw_delay_drag(&mut self, cx: &mut Cx2d) {
        let Some(drag) = self.delay_drag else { return };
        let (net, edge) = if let Some((index, end)) = drag.edit {
            let kept = &self.delays[index];
            let other = usize::from(end == End::Start);
            let net = if other == 0 { &kept.bracket.from.0 } else { &kept.bracket.to.0 };
            (net, kept.edges[other])
        } else { (&self.stack.channels[drag.lane], drag.found.then_some(drag.edge)) };
        let start = edge.and_then(|edge| self.cards.get(net)?.widget.borrow::<SignalLane>()?.delay_point(cx, edge));
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        let color = look.channel(self.stack.hues.get(drag.lane).copied().unwrap_or(drag.lane));
        if let Some(start) = start { self.delay_line(cx, start, drag.abs, color, 1.0); }
        // The carried end stays on the pointer; its snapped landing is already in the target lane.
        self.delay_line(cx, drag.abs, drag.abs, color, 9.0);
        self.delay_line(cx, drag.abs, drag.abs, look.canvas.canvas, 6.0);
        let label = std::mem::take(&mut self.delay_label);
        let text = self.chip_cache.layout(cx, &self.draw_chip_text, &label);
        let size = dvec2(f64::from(text.size_in_lpxs.width * self.draw_chip_text.font_scale) + 12.0, 22.0);
        let rect = Rect { pos: drag.abs + dvec2(12.0, 12.0), size };
        self.quad(cx, rect, look.canvas.canvas);
        self.draw_chip_text.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_chip_text.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: rect.pos + dvec2(6.0, 3.0), size: rect.size - dvec2(12.0, 3.0) }), &text);
        self.delay_label = label;
    }

    /// The chip a probe drag starts from, in the turtle's flow.
    fn draw_probe_chip(&mut self, cx: &mut Cx2d, ink: [f32; 4]) {
        let tag = ProbeTag { title: "+ probe", pin: "" };
        let style = TagStyle { hue: vec4(ink[0], ink[1], ink[2], ink[3]), kind: TagKind::Volt, side: LaneSide::Sim,
            state: if self.probe_hover || self.carrying.is_some() { TagState::Hover } else { TagState::Rest } };
        let metrics = tag.measure(cx, &mut self.draw_probe_tag, &self.draw_chip_text);
        let size = tag.layout(metrics, f64::INFINITY, style).size;
        let rect = cx.walk_turtle(Walk::new(Size::Fixed(size.x), Size::Fixed(size.y)));
        self.draw_probe.color = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_probe.draw_abs(cx, rect);
        tag.draw(cx, &mut self.draw_probe_tag, &mut self.draw_chip_text, rect, style);
    }

    /// The answer follows the carried identity.
    fn carry_answer(&self, cx: &Cx) -> String {
        let mean = cx.get_global_ref::<ProbeGesture>().and_then(ProbeGesture::hovered)
            .and_then(|net| self.cards.get(net))
            .and_then(|entry| match entry.channel {
                Some(channel) => self.host.feed().and_then(|feed| feed.measurements_lane(channel)),
                None => entry.host.feed().and_then(|feed| feed.measurements_lane(0)),
            })
            .and_then(|(_, values)| values.into_iter().find(|row| row.name == "mean"));
        mean.map_or_else(String::new, |row| format!("{:.2} {}", row.value, row.unit))
    }

    fn refresh_tip(&mut self, cx: &mut Cx) {
        let Some(tip) = self.tag_tip.as_ref() else { return };
        let net = cx.get_global_ref::<ProbeGesture>().and_then(ProbeGesture::hovered).map(str::to_owned);
        let refusal = cx.get_global_ref::<ProbeGesture>().and_then(|gesture| gesture.refusal.clone());
        let pin = net.as_deref().unwrap_or("");
        let label = refusal.as_deref().unwrap_or_else(|| self.visible.iter().chain(&self.probes).find(|probe| probe.net == pin).map_or(if pin.is_empty() { "probe" } else { pin }, |probe| probe.label.as_str()));
        let ink = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light).channel(self.stack.hue_of(pin).unwrap_or_else(|| self.stack.next_hue()));
        let (kind, side) = self.cards.get(pin).and_then(|entry| entry.widget.borrow::<SignalLane>()).map_or((TagKind::Volt, LaneSide::Sim), |lane| {
            (TagKind::from_unit(lane.settings().channels.first().map_or("V", |channel| channel.unit.as_str())), lane.sources().get(lane.primary_source()).map_or(LaneSide::Sim, |source| source.side.into()))
        });
        let answer = if refusal.is_some() { String::new() } else { self.carry_answer(cx) };
        if let Some(mut tag) = tip.borrow_mut::<ProbeTagTip>() {
            tag.set_identity(cx, label, if refusal.is_some() { "" } else { pin }, &answer, TagStyle { hue: vec4(ink[0], ink[1], ink[2], ink[3]), kind, side, state: TagState::Carried });
        };
    }

    fn stop(&mut self) {
        self.host.stop();
        for entry in self.cards.values_mut() {
            entry.host.stop();
        }
    }

    /// ELEC_SCOPE_PERF: one sample per drawing of the UI thread's CPU time and the wall gap since the
    /// previous drawing (a read of this thread's scheduler clock, Linux only; elsewhere gaps only).
    fn note_perf_frame(&mut self, drawing_id: u64) {
        if self.perf_draw.is_some_and(|(id, ..)| id == drawing_id) { return; }
        let now = Instant::now();
        let cpu = self.perf_cpu.now_ns();
        if let Some((_, at, before)) = self.perf_draw {
            self.perf_gap_us.push(now.duration_since(at).as_micros().min(u128::from(u32::MAX)) as u32);
            if let (Some(cpu), Some(before)) = (cpu, before) { self.perf_ui_us.push((cpu.saturating_sub(before) / 1000).min(u64::from(u32::MAX)) as u32); }
        }
        self.perf_draw = Some((drawing_id, now, cpu));
    }


    fn report_perf(&mut self) {
        if !self.host.running() || !self.scope_settings.running || !self.live_key.is_some_and(|key| key.playing && key.scrub.is_none()) {
            // Not reported while paused or scrubbing: per-drawing samples do not pile up meanwhile.
            self.perf_ui_us.clear();
            self.perf_gap_us.clear();
            self.perf_revealed = 0;
            self.perf_reveal_empty = 0;
            return;
        }
        let now = Instant::now();
        let since = *self.perf_at.get_or_insert(now);
        let elapsed = now.duration_since(since).as_secs_f64();
        if elapsed < 1.0 { return; }
        if let Some(feed) = self.host.feed() { feed.lane_waveforms(&mut self.perf_rastered); }
        let counters = self.host.counters();
        self.perf_presented.resize(self.perf_presented.len().max(counters.presented.len()), 0);
        for (total, count) in self.perf_presented.iter_mut().zip(&counters.presented) { *total += count; }
        let rastered = self.perf_rastered.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        let presented = self.perf_presented.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
        let mut latency = counters.latency_us.clone();
        latency.sort_unstable();
        let percentile = |p: usize| {
            if latency.is_empty() { "-".to_owned() }
            else { format!("{:.3}", latency[(latency.len() * p).div_ceil(100) - 1] as f64 / 1000.0) }
        };
        let ui_us = self.perf_ui_us.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let gap_us = self.perf_gap_us.iter().map(u32::to_string).collect::<Vec<_>>().join(" ");
        let offscreen = self.visible.iter().filter(|probe| self.cards.get(&probe.net).is_some_and(|entry| !entry.visible)).count();
        log!("scope perf: {:.3} s draws {} takes {} pushes {} busy {} hidden {} offscreen {} revealed {} empty {} up {} B packed {} B rects {} refused {} rastered [{}] presented [{}] lat p50 {} p95 {} max {} ms ui_us [{}] gap_us [{}]",
            elapsed, counters.draws, counters.takes, counters.pushes, counters.busy, counters.hidden, offscreen, self.perf_revealed, self.perf_reveal_empty,
            counters.upload_bytes, counters.packed_bytes, counters.rects, self.host.stats().refused, rastered, presented, percentile(50), percentile(95), percentile(100), ui_us, gap_us);
        self.perf_ui_us.clear();
        self.perf_gap_us.clear();
        self.perf_revealed = 0;
        self.perf_reveal_empty = 0;
        self.host.reset_counters();
        self.perf_at = Some(now);
    }

    fn frame(&mut self, cx: &mut Cx) {
        // Hidden: two draw events passed since the bench asked for a redraw, and neither walked it.
        // Counted in draw events, not calls: the sim's signal runs this several times per display frame.
        if self.asked_draw.is_some_and(|at| cx.drawing_id() >= at + 2) {
            self.asked_draw = None;
            self.cancel_generators(cx);
            self.cancel_delay();
            self.stop();
            return;
        }
        let live_key = LiveKey::read(cx);
        let mut changed = self.live_key != live_key;
        self.live_key = live_key;
        if changed {
            self.sync_source(cx, false);
            self.sync_res(cx);
        }
        if self.carrying.is_some() { self.refresh_tip(cx); }
        changed |= self.update_ghosts(cx);
        changed |= self.sync_events(cx);
        let sim = cx.get_global_ref::<ElecLive>().map(|live| (live.sim.scrub, live.sim.trace.clone(), live.sim.index.clone()));
        let scrubbing = sim.as_ref().is_some_and(|(scrub, ..)| scrub.is_some());
        if scrubbing && self.delay_drag.is_some() { self.cancel_delay(); }
        let mut rows = None;
        let mut spark_columns = None;
        let mut spectra = None;
        if !scrubbing {
            if self.perf { self.note_perf_frame(cx.drawing_id()); }
            self.host.note_draw(cx.drawing_id());
            let (channels, cards, routes, stack, drag) = (&self.stack.channels, &self.cards, &self.overlay_routes, &self.stack, &self.tag_drag);
            changed |= self.host.present_lanes(|channel, spectrum, hits| {
                let Some(net) = channels.get(channel) else { return Pushed::WrongSize(hits) };
                let routed = if routes.is_empty() { None } else { routes.get(net) };
                let Some(widget) = routed.or_else(|| cards.get(net).map(|entry| &entry.widget)) else { return Pushed::WrongSize(hits) };
                if cards.get(overlay_host(stack, drag.as_ref(), net).unwrap_or(net)).is_none_or(|entry| !entry.visible && entry.primed) { return Pushed::Hidden(hits); }
                let Some(mut lane) = widget.borrow_mut::<SignalLane>() else { return Pushed::Busy(hits) };
                let source = usize::from(routed.is_some());
                if spectrum { lane.push_spectrum_frame(cx, source, hits) } else { lane.push_source_frame(cx, source, hits) }
            });
            if let Some(feed) = self.host.feed() {
                if feed.measurement_generation() != self.measurement_generation {
                    if let Some((generation, values, sparks, spectrum)) = feed.measurement_snapshot() {
                        self.measurement_generation = generation;
                        rows = Some(values);
                        spark_columns = Some(sparks);
                        spectra = Some(spectrum);
                        changed = true;
                    }
                }
            }
        }

        if scrubbing {
            if let Some((_, Some(trace), _)) = &sim {
                if self.spark_source.as_ref().is_none_or(|source| !Arc::ptr_eq(&source.store, trace)) {
                    self.spark_source = Some(Arc::new(StoreSource { store: trace.clone() }));
                }
            }
        }
        let trigger_state = self.host.feed().map(SimScopeFeed::trigger_state);
        if let Some(state) = trigger_state {
            if let Some(word) = self.semantic_summary.borrow_mut().state(state) {
                cx.a11y.announce(word, a11y_tree::Live::Polite);
                changed = true;
            }
        }
        let features = if scrubbing { None } else { self.host.feed().and_then(SimScopeFeed::features) };
        let spans = if self.bracket.is_some() { self.host.feed().and_then(SimScopeFeed::spans) } else { None };
        self.index_record_rows(cx);
        for (net, entry) in &mut self.cards {
            let routed = if self.overlay_routes.is_empty() { None } else { self.overlay_routes.get(net) };
            let source = usize::from(routed.is_some());
            if let Some(mut card) = routed.unwrap_or(&entry.widget).borrow_mut::<SignalLane>() {
                if source == 0 {
                    card.set_bracket(cx, self.bracket);
                    card.set_bracket_visible(cx, !scrubbing);
                    if self.bracket.is_some() && !scrubbing {
                        let own_spans = if entry.channel.is_none() { entry.host.feed().and_then(SimScopeFeed::spans) } else { None };
                        let (answer, requested, channel) = if let Some(channel) = entry.channel {
                            (spans.as_ref(), self.bracket_generation, channel)
                        } else { (own_spans.as_ref(), entry.bracket_generation, 0) };
                        if let Some((generation, stats)) = answer {
                            card.set_span_stats(cx, stats.get(channel).copied().flatten(), *generation == requested);
                        } else { card.set_span_stats(cx, None, false); }
                    }
                    // A still has no live-capture geometry, including while its raster is pending.
                    let capture = if scrubbing { None } else if entry.channel.is_some() { features.clone() }
                        else { entry.host.feed().and_then(SimScopeFeed::features) };
                    card.set_features(cx, capture, entry.channel.unwrap_or(0));
                    if card.collapsed() {
                        if let Some((Some(at), _, index)) = &sim {
                            if let (Some(source), Some((_, Some(row)))) = (&self.spark_source, entry.row) {
                                if source.store.generation == index.generation {
                                    card.spark().set_trace(cx, source.clone(), row);
                                    let window = self.scope_settings.window();
                                    card.spark().set_window(cx, *at - window.pre, *at + window.post);
                                }
                            }
                        } else if let (Some(columns), Some(channel)) = (&spark_columns, entry.channel) {
                            if let Some(columns) = columns.get(channel) { card.spark().set_columns(cx, channel, columns); }
                        }
                    }
                }
                if let Some((Some(at), trace, index)) = &sim {
                    // An unprimed card off screen still takes its first still, like its first live raster.
                    if !entry.visible && entry.primed {
                        entry.shown = None;
                        entry.still_pending = false;
                        entry.spectrum_pending = false;
                        continue;
                    }
                    let Some(store) = trace else { continue };
                    if store.generation != index.generation {
                        continue;
                    }
                    let Some(plot) = card.plot_px().filter(|&(w, h)| w > 0 && h > 0) else { continue };
                    let key = StillKey {
                        at: *at,
                        generation: store.generation,
                        base: store.base,
                        end: store.base + store.len(),
                        window: card.settings().window(),
                        spectrum: card.spectrum_setup(),
                        bracket: card.spectrum().and(self.bracket),
                        volts: volts(card.settings(), plot),
                        plot,
                    };
                    if entry.shown != Some(key) {
                        changed = true;
                        entry.still_pending = false;
                        entry.spectrum_pending = false;
                        if source == card.primary_source() { card.set_spectrum_measurements(cx, None); }
                        match entry.row.and_then(|(_, row)| row) {
                            Some(row) => {
                                let hits = entry.hits.get_or_insert_with(|| Hits::new(plot.0, plot.1));
                                let (still, mut measurements) = if store.is_empty() || *at < store.time.at(0) || *at > store.time.at(store.len() - 1) {
                                    hits.reset(plot.0, plot.1);
                                    (Still::NoSamples, Vec::new())
                                } else { draw_still(store, row, *at, card.settings(), self.bracket, plot, &mut entry.scratch, hits) };
                                if let Some((_, pixels)) = key.spectrum.filter(|_| matches!(still, Still::Drawn)) {
                                    let hits = entry.spectrum_hits.get_or_insert_with(|| Hits::new(pixels.0, pixels.1));
                                    let (spectral, rows) = draw_spectrum_still(store, row, *at, card.settings(), self.bracket, pixels, &mut entry.scratch, hits);
                                    measurements.extend(rows);
                                    entry.spectrum_pending = matches!(spectral, Still::Drawn);
                                    if entry.spectrum_pending && source == card.primary_source() { card.set_spectrum_measurements(cx, entry.scratch.spectrum_measurements()); }
                                }
                                if card.overlay_pair() { card.set_source_measurements(cx, source, &measurements); }
                                else { card.set_measurements(cx, measurements); }
                                match still {
                                    Still::Drawn => {
                                        entry.still_pending = true;
                                        if source == card.primary_source() { card.set_measurement_note(cx, None); }
                                    }
                                    Still::NoSamples => { if source == card.primary_source() { card.set_measurement_note(cx, Some("no samples at this instant")); } }
                                }
                            }
                            None => {
                                card.set_source_measurements(cx, source, &[]);
                                if source == card.primary_source() { card.set_measurement_note(cx, Some("net not simulated")); }
                            }
                        }
                        card.clear_source_picture(cx, source);
                        entry.shown = Some(key);
                    }
                    if entry.spectrum_pending {
                        if let Some(hits) = entry.spectrum_hits.take() {
                            match card.push_source_still(cx, source, true, hits) {
                                Pushed::Taken(old) => { changed = true; entry.spectrum_hits = old; entry.spectrum_pending = false; }
                                Pushed::Busy(hits) => entry.spectrum_hits = Some(hits),
                                Pushed::Hidden(hits) => { entry.spectrum_hits = Some(hits); entry.spectrum_pending = false; entry.shown = None; }
                                Pushed::WrongSize(hits) => { entry.spectrum_hits = Some(hits); entry.shown = None; }
                            }
                        }
                    }
                    if entry.still_pending {
                        if let Some(hits) = entry.hits.take() {
                            match card.push_source_still(cx, source, false, hits) {
                                Pushed::Taken(old) => {
                                    changed = true;
                                    entry.hits = old;
                                    entry.still_pending = false;
                                }
                                Pushed::Busy(hits) => entry.hits = Some(hits),
                                Pushed::Hidden(hits) => { entry.hits = Some(hits); entry.still_pending = false; entry.shown = None; }
                                Pushed::WrongSize(hits) => {
                                    entry.hits = Some(hits);
                                    entry.shown = None;
                                }
                            }
                        }
                    }
                    continue;
                }
                // Back to live: the still stays until live frames outweigh it (a paused head keeps a picture).
                if entry.shown.take().is_some() || entry.still_pending || entry.spectrum_pending {
                    changed = true;
                    entry.still_pending = false;
                    entry.spectrum_pending = false;
                    entry.measurement_generation = 0;
                    self.measurement_generation = 0;
                }
                if let Some(channel) = entry.channel {
                    if let Some(state) = trigger_state { card.set_trigger_state(state); }
                    card.set_measurement_note(cx, None);
                    if let Some(rows) = &rows {
                        if card.overlay_pair() {
                            let rows: Vec<_> = rows.iter().filter(|row| row.lane == channel).cloned().map(|mut row| { row.lane = 0; row }).collect();
                            card.set_source_measurements(cx, source, &rows);
                        } else { card.set_lane_measurements(cx, rows, channel); }
                        if source == card.primary_source() { card.set_spectrum_measurements(cx, spectra.as_ref().and_then(|s| s.get(channel)).copied().flatten()); }
                    }
                } else {
                    if entry.visible || !entry.primed { changed |= entry.host.present(cx, &mut card).is_some(); }
                    else { changed |= entry.host.present_lanes(|_, _, hits| Pushed::Hidden(hits)); }
                    if let Some(feed) = entry.host.feed() { card.set_trigger_state(feed.trigger_state()); }
                    card.set_measurement_note(cx, entry.host.stats().unresolved.then_some("net not simulated"));
                    if let Some(feed) = entry.host.feed() {
                        if feed.measurement_generation() != entry.measurement_generation {
                            if let Some((generation, measurements, sparks, spectra)) = feed.measurement_snapshot() {
                                card.set_lane_measurements(cx, &measurements, 0);
                                card.set_spectrum_measurements(cx, spectra.first().copied().flatten());
                                if card.collapsed() { if let Some(columns) = sparks.first() { card.spark().set_columns(cx, 0, columns); } }
                                entry.measurement_generation = generation;
                                changed = true;
                            }
                        }
                    }
                }
                if !card.settings().running {
                    entry.host.stop();
                }
            }
        }
        self.sync_fit_period();
        self.follow_delays(cx);
        if let Some(drag) = self.delay_drag { self.preview_delay(cx, drag.abs); }
        self.push_delays(cx);
        if self.perf { self.report_perf(); }
        if changed || self.animating() {
            self.redraw(cx);
            self.asked_draw.get_or_insert(cx.drawing_id());
        }
        self.request_frame(cx);
    }
}

/// The colour a bench card's first channel draws in.
fn channel_ink(dark: bool) -> [f32; 4] {
    InstrumentLook::for_theme(dark).channel(0)
}

impl WidgetNode for BenchInstruments {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }

    fn area(&self) -> Area {
        self.draw_bg.area()
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.draw_bg.redraw(cx);
        self.scroll_bars.redraw(cx);
        if let Some(bar) = &self.source_bar_widget { bar.redraw(cx); }
        if let Some(strip) = &self.strip_widget {
            strip.redraw(cx);
        }
    }

    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        if let Some(bar) = &self.source_bar_widget { visit(LiveId::from_str("source bar"), bar.clone()); }
        if let Some(panel) = &self.controls_panel_widget { visit(LiveId::from_str("controls"), panel.clone()); }
        if let Some(strip) = &self.strip_widget {
            visit(LiveId::from_str("strip"), strip.clone());
        }
        for stub in &self.generator_order {
            if let Some(widget) = self.generators.get(stub) {
                visit(LiveId::from_str(&format!("generator:{stub}")), widget.clone());
            }
        }
        if let Some(widget) = &self.fw_lane_widget { visit(id!(fw_lane), widget.clone()); }
        let ghosts = self.ghosts.iter().map(|ghost| &ghost.net).filter(|net| !self.probes.iter().any(|probe| probe.net == **net));
        for net in self.probes.iter().map(|probe| &probe.net).chain(ghosts) {
            if self.overlay_routes.contains_key(net) { continue; }
            if let Some(entry) = self.cards.get(net) {
                visit(LiveId::from_str(net), entry.widget.clone());
            }
        }
    }

    fn find_widgets_from_point(&self, cx: &Cx, point: DVec2, found: &mut dyn FnMut(&WidgetRef)) {
        if let Some(bar) = &self.source_bar_widget { bar.find_widgets_from_point(cx, point, found); }
        if let Some(panel) = &self.controls_panel_widget { panel.find_widgets_from_point(cx, point, found); }
        if self.scroll_bars.area().rect(cx).contains(point) {
            for widget in self.generators.values() {
                widget.find_widgets_from_point(cx, point, found);
            }
            for (net, entry) in &self.cards {
                if !self.overlay_routes.contains_key(net) { entry.widget.find_widgets_from_point(cx, point, found); }
            }
        }
    }
}

impl Widget for BenchInstruments {
    fn clips_content(&self) -> bool {
        true
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.role_description, "scope");
        makepad_platform::a11y::set_string(&mut node.name, "Scope");
        makepad_platform::a11y::set_string(&mut node.value, &self.semantic_summary.borrow().text);
    }

    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let id_key = match lookup {
            SemanticLookup::Terms(q) => q.id_term().filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0).and_then(|id| cx.a11y.action_part_key(id)),
            SemanticLookup::Key(_) => None,
        };
        let Some(lookup) = super::semantics::Lookup::new(lookup, id_key) else { return };
        let ghosts = self.ghosts.iter().filter(|ghost| !self.stack.display.contains(&ghost.net)).map(|ghost| &ghost.net);
        for net in self.stack.display.iter().chain(ghosts) {
            if self.overlay_routes.contains_key(net) { continue; }
            if let Some(lane) = self.cards.get(net).and_then(|entry| entry.widget.borrow::<SignalLane>()) { lane.lookup(cx, &lookup, out); }
        }
        for stub in &self.generator_order {
            if let Some(lane) = self.generators.get(stub).and_then(|widget| widget.borrow::<DriveLane>()) { lane.lookup(cx, &lookup, out); }
        }
        if let Some(lane) = self.fw_lane_widget.as_ref().and_then(|widget| widget.borrow::<FwLane>()) { lane.lookup(cx, &lookup, out); }
        self.lookup_delays(cx, &lookup, out);
        let key = LiveId::from_str("probe chip").0;
        if lookup.wants(key) {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Button, name: "probe".into(), actions: a11y_tree::Actions::FOCUS | a11y_tree::Actions::PRESS, ..Default::default() };
            super::semantics::bounds(&mut node, makepad_platform::a11y::snapshot_rect(&self.draw_probe.area(), cx));
            lookup.push(key, node, out);
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions as A};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        if key == LiveId::from_str("probe chip").0 {
            if request.action == A::SCROLL_INTO_VIEW {
                self.scroll_bars.scroll_into_view_abs(cx, self.draw_probe.area().rect(cx)); self.redraw(cx); return ActionResult::Handled;
            }
            // The platform's ordinary focus/centre-press fallback follows the probe gesture.
            return ActionResult::Unhandled;
        }
        for (net, entry) in &self.cards {
            if self.overlay_routes.contains_key(net) { continue; }
            let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() else { continue };
            let result = lane.lookup_action(cx, key, request);
            if result != ActionResult::Unhandled {
                if request.action == A::SCROLL_INTO_VIEW { self.scroll_bars.scroll_into_view_abs(cx, lane.area().rect(cx)); }
                return result;
            }
        }
        for stub in &self.generator_order {
            let Some(mut lane) = self.generators.get(stub).and_then(|widget| widget.borrow_mut::<DriveLane>()) else { continue };
            let result = lane.lookup_action(cx, key, request);
            if result != ActionResult::Unhandled {
                if request.action == A::SCROLL_INTO_VIEW { self.scroll_bars.scroll_into_view_abs(cx, lane.area().rect(cx)); }
                return result;
            }
        }
        self.delay_lookup_action(cx, key, request)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // Unfed, the pane follows the electronics host: its snapshots arrive with signals.
        if matches!(event, Event::Signal) {
            self.follow_bench(cx);
        }
        self.handle_peek(cx, event);
        if self.source_timer.is_event(event).is_some() { self.update_source_answers(cx); }
        if matches!(event, Event::Signal) || self.next_frame.is_event(event).is_some()
            || (matches!(event, Event::NextFrame(_)) && self.live_key != LiveKey::read(cx)) {
            self.frame(cx);
        }
        let strip = self.strip_widget(cx);
        let strip_h = strip.borrow::<TimeStrip>().map(|strip| strip.height());
        let actions = cx.capture_actions(|cx| {
            if let Some(mut transport) = strip.borrow_mut::<TimeStrip>() {
                if let Some(t) = self.violation_jump {
                    if transport.stop_on_violation(cx, t) { self.violation_jump = None; }
                }
                if transport.waiting_for_trigger() && self.host.running() {
                    if let Some(feed) = self.host.feed() { transport.observe_trigger(cx, feed.trigger_events()); }
                }
            }
            if let Event::KeyDown(key) = event {
                if matches!(key.key_code, KeyCode::KeyS | KeyCode::LBracket | KeyCode::RBracket) && self.cards.values().any(|entry| cx.has_key_focus(entry.widget.area())) {
                    if let Some(mut transport) = strip.borrow_mut::<TimeStrip>() { transport.handle_key(cx, key.key_code, key.modifiers.shift); }
                }
            }
            strip.handle_event(cx, event, scope);
        });
        if strip.borrow::<TimeStrip>().map(|strip| strip.height()) != strip_h { self.redraw(cx); }
        self.handle_strip_actions(cx, &actions, &strip);
        let bar = self.source_bar_widget(cx);
        let actions = cx.capture_actions(|cx| bar.handle_event(cx, event, scope));
        self.handle_source_actions(cx, &actions, &bar);
        let panel = self.controls_panel_widget(cx);
        panel.handle_event(cx, event, scope);
        let is_scroll = matches!(event, Event::Scroll(_));
        if !is_scroll {
            self.scroll_bars.handle_event(cx, event, scope);
        }
        let axes = self.scroll_bars.scroll_container_axes(cx, event);
        cx.begin_scroll_container(axes);
        for widget in self.generators.values_mut() {
            widget.handle_event(cx, event, scope);
        }
        for (net, entry) in &mut self.cards {
            if !self.overlay_routes.contains_key(net) { entry.widget.handle_event(cx, event, scope); }
        }
        self.handle_probe_chip(cx, event);
        cx.end_scroll_container(axes);
        self.handle_carry(cx, event);
        self.handle_tag_drag(cx, event);
        self.handle_trigger_drag(cx, event);
        if is_scroll {
            self.scroll_bars.handle_event(cx, event, scope);
        }
        if let Event::Actions(actions) = event {
            self.handle_strip_actions(cx, actions, &strip);
            self.handle_source_actions(cx, actions, &bar);
            for (stub, widget) in &self.generators {
                for action in actions.filter_widget_actions_cast::<GeneratorAction>(widget.widget_uid()) {
                    let action = match action {
                        GeneratorAction::SetDrive { pin, drive, field, preview } => {
                            let label = format!("{stub} {pin} {field}");
                            let ops = vec![DesignOp::Bench { bench: self.source_bench.clone(), op: BenchOp::SetDrive { stub: stub.clone(), pin, drive } }];
                            if preview { ViewAction::Preview { notes: vec![], tag: next_tag(), place: vec![], ops, label } } else { ViewAction::Ops { place: vec![], ops, label } }
                        }
                        GeneratorAction::CommitPreview => ViewAction::CommitPreview,
                        GeneratorAction::None => continue,
                    };
                    cx.widget_action(self.uid, action);
                }
            }
            for action in actions {
                if let Some(ViewAction::Cursor(c)) = action.downcast_ref::<ViewAction>() {
                    if let Some(mut strip) = strip.borrow_mut::<TimeStrip>() {
                        strip.hear(cx, *c);
                    }
                    if let Some(mut lane) = self.fw_lane_widget.as_ref().and_then(|widget| widget.borrow_mut::<FwLane>()) {
                        lane.set_cursor(cx, c.fw());
                    }
                    self.following = c.playing || c.t >= self.strip_end();
                }
                match action.downcast_ref::<SimAction>() {
                    Some(SimAction::BeginGesture) => {
                        for entry in self.cards.values_mut() {
                            if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                                if card.settings().running {
                                    card.freeze_ghost(cx);
                                    card.set_ghost_on_top(cx, false);
                                }
                            }
                        }
                        self.ghost = Some(RunGhost::UnderLive);
                        self.redraw(cx);
                    }
                    Some(SimAction::ClearGhost) => {
                        for entry in self.cards.values_mut() {
                            if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                                card.clear_ghost(cx);
                            }
                        }
                        self.ghost = None;
                        self.redraw(cx);
                    }
                    _ => {}
                }
            }
            let light = base_theme(cx) == BaseTheme::Light;
            let mut dragged = None;
            let mut trigger_dragged = None;
            let mut domain = None;
            let mut bracket = None;
            let mut pins_changed = false;
            let mut sources_swapped = false;
            for (net, entry) in &mut self.cards {
                let uid = entry.widget.widget_uid();
                if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                    for action in actions.filter_widget_actions_cast::<ScopeAction>(uid) {
                        let primary = card.sources().get(card.primary_source()).and_then(|source| source.channel).and_then(|channel| self.stack.channels.get(channel)).unwrap_or(net);
                        let trigger_here = self.trigger_net.as_ref() == Some(net)
                            || overlay_guest(&self.stack, self.tag_drag.as_ref(), net).is_some_and(|guest| self.trigger_net.as_deref() == Some(guest));
                        match action {
                            ScopeAction::Bracket(span) => { bracket = Some(span); }
                            ScopeAction::Pin { name, on } => {
                                if self.stack.channels.contains(net) {
                                    if let Some(quantity) = TrendQuantity::from_name(name) {
                                        let pin = TrendPin { net: primary.clone(), quantity };
                                        if on { if !self.pins.contains(&pin) { self.pins.push(pin); } }
                                        else { self.pins.retain(|kept| *kept != pin); }
                                    }
                                }
                                pins_changed = true;
                            }
                            ScopeAction::StopOnFail { expect, on } => {
                                let key = (primary.clone(), expect);
                                if on { self.stop_on_fail.insert(key); } else { self.stop_on_fail.remove(&key); }
                                self.stop_pending = true; self.violation_jump = None;
                                pins_changed = true;
                            }
                            ScopeAction::Close => {
                                if self.probes.iter().any(|probe| probe.net == *net) {
                                    remove_probe(cx, self.uid, (!self.bench_fed).then_some((self.source_bench.as_str(), &self.bench_probes[..])), primary.clone());
                                }
                            }
                            ScopeAction::TriggerLane => {
                                if self.stack.channels.contains(net) { self.trigger_net = Some(primary.clone()); }
                            }
                            ScopeAction::DragTrigger { abs, grab } => {
                                if trigger_here { trigger_dragged = self.trigger_net.clone().map(|net| (net, abs, grab)); }
                            }
                            ScopeAction::DragTag { abs, channel } => {
                                let carried = channel.and_then(|channel| self.stack.channels.get(channel)).unwrap_or(net);
                                if self.stack.channels.contains(carried) { dragged = Some((carried.clone(), abs)); }
                            }
                            ScopeAction::Resize { height, done } => {
                                if entry.channel.is_some() {
                                    if let Some(stored) = self.heights.get_mut(net) { *stored = height; }
                                    else { self.heights.insert(net.clone(), height); }
                                    if done { self.resizing = None; }
                                    else if self.resizing.as_ref() != Some(net) { self.resizing = Some(net.clone()); }
                                }
                            }
                            ScopeAction::Reconfigure(settings) => {
                                if self.stack.channels.contains(net) {
                                    self.scope_settings.time_per_div = settings.time_per_div;
                                    self.scope_settings.position = settings.position;
                                    self.scope_settings.running = settings.running;
                                    self.scope_settings.trigger.zone = settings.trigger.zone;
                                    if trigger_here { self.scope_settings.trigger = settings.trigger; }
                                    domain = Some((net.clone(), settings.domain));
                                } else { entry.host.handle_action(cx, &mut card, ScopeAction::Reconfigure(settings), light); }
                            }
                            ScopeAction::Clear => { if entry.shown.take().is_some() { entry.still_pending = true; } }
                            ScopeAction::SwapSources { .. } => { pins_changed = true; sources_swapped = true; }
                            ScopeAction::Delay(_) | ScopeAction::None => {}
                        }
                    }
                }
            }
            if sources_swapped {
                // Refresh primary-only notes and spectrum markers even on a paused scrub.
                for entry in self.cards.values_mut() { entry.shown = None; }
                self.measurement_generation = 0;
                if !cx.next_frame_is_pending(self.next_frame) { self.next_frame = cx.new_next_frame(); }
            }
            if pins_changed { self.sync_trends(cx); self.sync_events(cx); }
            if let Some(bracket) = bracket { self.set_bracket(cx, bracket); }
            // Preserve gesture order even when several steps arrive in one action batch.
            // Borrow the payload: moves carry no net Strings and need no temporary action Vec.
            for action in actions.iter().filter_map(|action| action.as_widget_action()) {
                if let Some(ScopeAction::Delay(gesture)) = action.action.downcast_ref::<ScopeAction>() {
                    let lane = self.cards.values().find(|entry| entry.widget.widget_uid() == action.widget_uid).and_then(|entry| entry.channel);
                    if let Some(lane) = lane { self.handle_delay(cx, lane, *gesture); }
                }
            }
            self.sync_shared(cx);
            if let Some((net, domain)) = domain { self.set_probe_domain(cx, &net, domain); }
            if let Some((net, abs)) = dragged { self.begin_tag_drag(cx, net, abs); }
            if let Some((net, abs, grab)) = trigger_dragged { self.begin_trigger_drag(cx, net, abs, grab); }
            self.redraw(cx);
            self.request_frame(cx);
        }
        if matches!(event, Event::Shutdown) {
            cx.stop_timer(self.source_timer);
            self.stop();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.asked_draw = None;
        self.follow_bench(cx);
        if cx.has_global::<ElecLive>() {
            let (record, ratio, t, playing) = {
                let sim = &cx.get_global::<ElecLive>().sim;
                // while the recording plays the strip follows the sound: it moves only while that does
                (sim.record, sim.ratio, sim.t, sim.heard.map_or(sim.playing, |(_, moving)| moving))
            };
            // the simulation's record ends at the running head, which playback follows
            let model = StripModel { span: record, ratio, ghost: self.ghost, live: true, ..Default::default() };
            if self.strip_model.as_ref() != Some(&model) {
                if let Some(mut strip) = self.strip_widget(cx).borrow_mut::<TimeStrip>() {
                    strip.set_model(cx, model.clone());
                }
                self.strip_model = Some(model);
            }
            if self.following && self.last_heard != Some((t, playing)) {
                if let Some(mut strip) = self.strip_widget(cx).borrow_mut::<TimeStrip>() {
                    strip.hear(cx, TimeCursor { t, playing, rate: self.rate, source: CursorSource::Sim, fw: None });
                }
                self.last_heard = Some((t, playing));
            }
        }
        self.update_ghosts(cx);
        if self.visible.iter().any(|probe| !self.cards.contains_key(&probe.net)) { self.sync_cards(cx); }
        self.sync_record(cx);
        self.push_delays(cx);
        let shown = std::mem::take(&mut self.visible);
        if self.spare.is_none() {
            if let Some(template) = &self.template {
                self.spare = Some(new_card(cx, template));
            }
        }
        if self.generator_spare.is_none() {
            self.generator_spare = Some(new_card(cx, &self.generator));
        }
        let light = base_theme(cx) == BaseTheme::Light;
        self.chip_cache.begin_frame(&self.draw_chip_text, base_theme(cx));
        let ink = channel_ink(!light);
        let rect = cx.walk_turtle(walk);
        let now = Instant::now();
        let order_changed = !shown.iter().map(|probe| &probe.net).eq(self.last_order.iter());
        let reflow = order_changed && self.resizing.is_none() && rect.size.x == self.last_width && !makepad_widgets_core::tween::tween_ticker_ref(cx).reduced_motion;
        if self.resizing.is_some() || rect.size.x != self.last_width {
            self.flips.clear();
        } else {
            self.flips.retain(|_, flip| (now - flip.start).as_secs_f64() < FLIP_TIME);
        }
        let canvas = Tokens::for_theme(!light).canvas;
        self.draw_bg.color = vec4(canvas[0], canvas[1], canvas[2], canvas[3]);
        self.draw_bg.draw_abs(cx, rect);
        cx.begin_turtle(Walk::abs_rect(rect), Layout::flow_down());
        if self.source_chip.is_none() { self.sync_source(cx, true); }
        let bar = self.source_bar_widget(cx);
        if let Some(mut source) = bar.borrow_mut::<SourceBar>() {
            let channel = self.trigger_net.as_ref().and_then(|net| self.stack.channels.iter().position(|n| n == net)).unwrap_or(0);
            source.set_look(cx, self.scope_settings.persistence, self.scope_settings.intensity, channel);
        }
        bar.draw_walk_all(cx, scope, Walk::new(Size::fill(), Size::Fixed(30.0)));
        // The one bar above the tabs is the run's only transport: the Bench draws no strip of its
        // own. The run board's controls sit under the cards, generated from its parts (a key per
        // switch with its LED, a knob per pot, the LEDs no control carries), in at most
        // `PANEL_SHARE` of the height, where they scroll.
        let panel = self.controls_panel_widget(cx);
        let panel_h = panel.borrow_mut::<ControlsPanel>().map_or(0.0, |mut panel| panel.height_for(cx, rect.size.x)).min(rect.size.y * PANEL_SHARE);
        let viewport = Walk::new(Size::fill(), Size::Fixed((rect.size.y - self.bottom_space - panel_h - 24.0 - 30.0).max(0.0)));
        self.scroll_bars.begin(cx, viewport, self.layout);
        let scroll_pos = self.scroll_bars.get_scroll_pos();
        let viewport = cx.turtle().rect_unscrolled();
        let dpi = cx.current_dpi_factor();
        let mut revealed = false;
        let card_width = cx.turtle().inner_width().max(1.0);
        // Borrowed Widget draws bypass semantic_draw. Only this stack is a canvas node;
        // the lanes' nested popovers continue drawing through their ordinary WidgetRefs.
        for stub in &self.generator_order {
            if let Some(widget) = self.generators.get_mut(stub) {
                if let Some(mut lane) = widget.borrow_mut::<DriveLane>() {
                    lane.draw_walk_all(cx, scope, Walk { width: Size::Fixed(card_width), height: Size::fit(), ..Walk::default() });
                }
            }
        }
        if shown.is_empty() && self.generators.is_empty() && self.fw_events.is_none() {
            cx.begin_turtle(
                Walk::fill(),
                Layout {
                    align: Align { x: 0.5, y: 0.5 },
                    spacing: 12.0,
                    ..Layout::flow_down()
                },
            );
            self.draw_empty
                .draw_walk(cx, Walk::fit(), Align::default(), "Drag the probe onto a net, or hold P and press one");
            self.draw_probe_chip(cx, ink);
            cx.end_turtle();
        } else {
            let new_height = new_lane_height(self.stack.channels.len());
            for probe in &shown {
                let mut offset = self.flips.get(&probe.net).map(|flip| {
                    let k = ((now - flip.start).as_secs_f64() / FLIP_TIME).clamp(0.0, 1.0);
                    flip.from * (1.0 - k).powi(3)
                });
                let Some(entry) = self.cards.get_mut(&probe.net) else { continue };
                let lane_height = entry.widget.borrow::<SignalLane>().map_or(DEFAULT_LANE_HEIGHT, |lane| {
                    if lane.collapsed() { 28.0 } else if entry.channel.is_none() { lane.height() }
                    else { self.heights.get(&probe.net).copied().unwrap_or(new_height) }
                });
                let walk = Walk { width: Size::Fixed(card_width), height: Size::Fixed(lane_height), ..Walk::default() };
                let measured = if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                    entry.setup = Some(lane.setup_for_size(dvec2(card_width, lane_height), dpi));
                    lane.layout_walk(cx, walk)
                } else { walk };
                let planned = cx.peek_walk_turtle(measured);
                let target = planned.pos - rect.pos + scroll_pos;
                if let Some(old_target) = self.placed.get_mut(&probe.net) {
                    if reflow && (target - *old_target).length() > 0.5 {
                        let from = *old_target + offset.unwrap_or_default() - target;
                        self.flips.insert(probe.net.clone(), Flip { from, start: now });
                        offset = Some(from);
                    }
                    *old_target = target;
                } else {
                    self.placed.insert(probe.net.clone(), target);
                }
                // Touching edges share a point. Dragged cards and every active FLIP stay drawn.
                let visible = offset.is_some() || self.tag_drag.as_ref().is_some_and(|drag| drag.net == probe.net)
                    || (planned.pos.x <= viewport.pos.x + viewport.size.x && planned.pos.x + planned.size.x >= viewport.pos.x
                        && planned.pos.y <= viewport.pos.y + viewport.size.y && planned.pos.y + planned.size.y >= viewport.pos.y);
                // A card below the fold is drawn until it holds a picture it will show at its current
                // size, then no more: a resize or a source change while off screen primes it again.
                let size = entry.setup.map(|setup| (setup.map.w, setup.map.h));
                let (primed, first_picture) = entry.widget.borrow_mut::<SignalLane>().map_or((false, false), |mut lane| {
                    let current = size.is_none_or(|size| lane.plot_px() == Some(size));
                    let primed = current && lane.holds_picture();
                    (primed, !primed && (!current || lane.picture_clear_pending() || lane.phosphor().wants_first_picture()))
                });
                entry.primed = primed;
                if visible != entry.visible {
                    revealed |= visible;
                    entry.shown = None;
                    entry.still_pending = false;
                    entry.spectrum_pending = false;
                    if visible {
                        if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                            // A card keeps its last picture off screen and shows it again on the way back in;
                            // its persistence carries on from there until the first new raster arrives.
                            lane.phosphor().resume_clock();
                            lane.spectrum_view().resume_clock();
                            // A card's very first draw is no reveal; every later return to the screen is.
                            if lane.plot_px().is_some() {
                                self.perf_revealed += 1;
                                entry.reveal_at = Some(cx.repaint_id());
                            }
                        }
                    }
                    entry.visible = visible;
                }
                if !visible && !first_picture {
                    let card_rect = cx.walk_turtle(measured);
                    self.placed.insert(probe.net.clone(), card_rect.pos - rect.pos + scroll_pos);
                    continue;
                }
                if let Some(bloom) = self.blooms.get(&probe.net) {
                    // The new screen flashes, its glow spreading on a dark screen, and settles to its own look.
                    let glow = (1.0 - bloom.start.elapsed().as_secs_f64() / BLOOM_GLOW).max(0.0) as f32;
                    if let Some(mut card) = entry.widget.borrow_mut::<SignalLane>() {
                        let mut look = card.settings().look(light);
                        look.gain *= 1.0 + (BLOOM_FLASH - 1.0) * glow * glow;
                        if !light {
                            look.bloom += (1.0 - look.bloom) * glow;
                        }
                        card.phosphor().set_look(cx, look);
                    }
                }
                let start = cx.align_list_len();
                if let Some(mut lane) = entry.widget.borrow_mut::<SignalLane>() {
                    // A reveal is empty when the frame that showed it painted no picture: none was there (a clear it
                    // applied included), or the step that made it had not painted. Judged once that frame is painted,
                    // before this drawing adds to it.
                    if entry.reveal_at.is_some_and(|at| cx.repaint_id() > at) {
                        entry.reveal_at = None;
                        self.perf_reveal_empty += u32::from(!lane.phosphor().picture_shown(cx));
                    }
                    lane.draw_walk_all(cx, scope, walk);
                }
                let mut card_rect = entry.widget.area().rect(cx);
                if let Some(offset) = offset {
                    cx.shift_align_range(&TurtleAlignRange { start, end: cx.align_list_len() }, offset);
                    card_rect.pos += offset;
                }
                if let Some(card) = entry.widget.borrow::<SignalLane>() {
                    if entry.channel.is_none() && card.settings().running {
                        let mut bracket_generation = None;
                        let result = entry.host.sync_plot(cx, &card, |cx, plot, settings| {
                            let node = probe_node(cx, &probe.net);
                            let feed = SimScopeFeed::show_spectrum(
                                cx,
                                NetProbe {
                                    pos: node,
                                    neg: None,
                                    scale: 1.0,
                                },
                                SimScopeConfig {
                                    plot,
                                    volts: volts(settings, plot),
                                    trigger: self.scope_settings.trigger(),
                                    qualifier: self.scope_settings.trigger.qualify,
                                    zone: None,
                                    window: settings.window(),
                                    auto: settings.auto_after(),
                                    frame_every: Duration::from_millis(8),
                                },
                                card.spectrum(),
                            )?;
                            bracket_generation = Some(feed.set_span(self.bracket));
                            Ok(feed)
                        });
                        if let Some(generation) = bracket_generation { entry.bracket_generation = generation; }
                        if let Err(error) = &result {
                            if !entry.start_failed {
                                log!("bench instruments: could not start {}: {error:?}", probe.net);
                            }
                        }
                        entry.start_failed = result.is_err();
                        if let Some(plot) = card.plot_px() {
                            let setup = (LaneSetup { map: card.settings().map(plot), spectrum: card.spectrum_setup(), drawn: true }, self.scope_settings.trigger(), self.scope_settings.window(), self.scope_settings.auto_after(), self.scope_settings.trigger.qualify);
                            if entry.preview_setup != Some(setup) {
                                entry.host.configure_lanes(setup.1, setup.4, 0, setup.2, vec![setup.0], setup.3, None);
                                entry.preview_setup = Some(setup);
                            }
                        }
                    } else {
                        entry.host.stop();
                    }
                }
                let ink = InstrumentLook::for_theme(!light).channel(self.stack.hue_of(&probe.net).unwrap_or_else(|| self.stack.next_hue()));
                self.draw_card_marks(cx, &probe.net, card_rect, ink);
                if self.tag_drag.as_ref().is_some_and(|drag| drag.over && drag.net == probe.net) {
                    let bg = Tokens::for_theme(!light).canvas;
                    self.quad(cx, card_rect, [bg[0], bg[1], bg[2], 0.65]);
                }
            }
            // The firmware lane sits beneath the scope cards, on the trigger card's time axis.
            if let Some(widget) = &self.fw_lane_widget {
                let window = self.scope_settings.window();
                let time_map = self.trigger_net.as_ref().and_then(|net| self.cards.get(net)).and_then(|entry| {
                    let instant = entry.shown.map(|key| key.at).or(self.latest_trigger_at)?;
                    let (x0, width) = entry.widget.borrow::<SignalLane>()?.time_plot_x()?;
                    Some(TimeMap { x0, width, t0: instant - window.pre, t1: instant + window.post })
                });
                if let Some(mut lane) = widget.borrow_mut::<FwLane>() {
                    lane.set_time_map(time_map);
                    lane.draw_walk_all(cx, scope, Walk { width: Size::Fixed(card_width), height: Size::fit(), ..Walk::default() });
                }
            }
            self.draw_probe_chip(cx, ink);
        }
        for net in &self.stack.channels {
            let visible = if let Some(host) = overlay_host(&self.stack, self.tag_drag.as_ref(), net) {
                self.cards.get(host).is_some_and(|entry| entry.visible)
            } else if self.tag_drag.as_ref().is_some_and(|drag| drag.net == *net) { true }
            else { shown.iter().any(|probe| probe.net == *net) && self.cards.get(net).is_some_and(|entry| entry.visible) };
            if let Some(entry) = self.cards.get_mut(net) {
                if entry.visible != visible { entry.shown = None; entry.still_pending = false; entry.spectrum_pending = false; }
                entry.visible = visible;
            }
        }
        if order_changed {
            self.last_order.clear();
            self.last_order.extend(shown.iter().map(|probe| probe.net.clone()));
        }
        self.visible = shown;
        self.last_width = rect.size.x;
        if order_changed {
            let last_order = &self.last_order;
            self.placed.retain(|net, _| last_order.contains(net));
            self.flips.retain(|net, _| last_order.contains(net));
        }
        self.scroll_bars.end(cx);
        if self.reveal.is_some() {
            self.reveal = self.reveal.take().filter(|net| self.probes.iter().any(|probe| &probe.net == net));
            self.scroll_to_reveal(cx);
        }
        self.update_blooms(cx);
        // A paused scrub has no live publication to wake it after a hidden still is invalidated.
        if revealed && !cx.next_frame_is_pending(self.next_frame) { self.next_frame = cx.new_next_frame(); }
        self.draw_delay_drag(cx);
        self.sync_feed(cx);
        self.sync_events(cx);
        let axis = cx.walk_turtle(Walk::new(Size::fill(), Size::Fixed(24.0)));
        if let Some(mut lane) = self.trigger_net.as_ref().and_then(|net| self.cards.get(overlay_host(&self.stack, self.tag_drag.as_ref(), net).unwrap_or(net))).and_then(|entry| entry.widget.borrow_mut::<SignalLane>()) {
            let axis = Rect { pos: axis.pos + dvec2(self.layout.padding.left, 0.0), size: dvec2((axis.size.x - self.layout.padding.left - self.layout.padding.right).max(1.0), axis.size.y) };
            lane.draw_shared_time_axis(cx, axis);
        }
        if panel_h > 0.0 {
            // through its WidgetRef, so its controls are in the semantic tree
            panel.draw_walk_all(cx, scope, Walk::new(Size::fill(), Size::Fixed(panel_h)));
        }
        cx.end_turtle();
        self.request_frame(cx);
        DrawStep::done()
    }
}

impl Drop for BenchInstruments {
    fn drop(&mut self) {
        self.stop();
    }
}

impl BenchInstrumentsRef {
    pub fn set_fw_events(&self, cx: &mut Cx, events: Option<Arc<dyn FwEvents>>) {
        if let Some(mut instruments) = self.borrow_mut() { instruments.set_fw_events(cx, events); }
    }

    pub fn set_bench(&self, cx: &mut Cx, sources: BenchView) {
        if let Some(mut instruments) = self.borrow_mut() {
            instruments.set_bench(cx, sources);
        }
    }

    pub fn set_refusal(&self, cx: &mut Cx, refusal: Option<Refusal>) {
        if let Some(mut instruments) = self.borrow_mut() {
            instruments.set_refusal(cx, refusal);
        }
    }

    pub fn set_expects(&self, cx: &mut Cx, expects: Vec<LaneExpect>) {
        if let Some(mut instruments) = self.borrow_mut() {
            instruments.set_expects(cx, expects);
        }
    }

    pub fn set_probes(&self, cx: &mut Cx, probes: Vec<BenchProbe>) {
        if let Some(mut instruments) = self.borrow_mut() {
            instruments.set_probes(cx, probes);
        }
    }
}
