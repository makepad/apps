//! The Scope shows one lane per probed net on a shared time ruler.
//!
//! In recording view the ruler scrubs the take. Drag a trace to pan, click to move the
//! playhead, or use the wheel to zoom. In trigger sweep the lanes follow the timebase.
//! The ruler menu holds time model, trigger, cursor, decoder and audio commands.
//!
//! A lane shows its name, value, display kind and monitoring level. Drag the name to
//! reorder, or use the lane menu to rename, trigger or remove its probe. The speaker
//! and meter control the shared listen mix. Decoders draw annotations below their lanes.
//!
//! Only visible lanes reduce the record, once per changed record, window or width.
//! The column cache reuses completed columns as the record grows. Analog drawing costs
//! O(columns), digital drawing O(runs). Logic reduction costs O(min(visible edges,
//! columns) × log(edges)). Trigger searches use the trace pyramid or indexed edges. Decoder rows visit
//! visible chunks and annotations. The worker receives the record by shared reference.
//! Each visible header draws a fixed set of controls and looks up its monitored voice.
//! Coverage and the playhead use constant geometry independent of the take length.
//! An unchanged window and record reuse their reductions.

use super::{
    logic::{cursor_for_click, cursor_readout, edge_runs, find_logic_trigger, find_trigger, follow_trigger, hold, logic_runs, move_to_gap, nearest_edge, nearest_logic_edge, next_trigger, net_facts, step_125, swings_like_logic, Anchor, LaneKind, LaneOf, LaneSource, Level, NetFacts, Run, Thresholds, Timebase, Trigger, TrigMode, Which, DIVS},
    look::InstrumentLook,
};
use super::semantics;
use super::decoders::{Decoded, DecodeWorker, Piece, Protocol, Setup};
use fab_elec_bench::decode::flags as ann_flags;
use crate::{
    live::{
        listen::{self, level_db, listen_status, listening, master_db, muted, set_level_db, set_master_db, ElecAudio},
        overview::{PanScopeWindow, ScopeWindowShown},
        peek::PeekGesture,
        probe::{click_probe_op, host_bench, remove_probe_op, rename_keeps, rename_probe_op},
        sim::{EnvSlot, SimAction, SinkSlot},
        transport::TraceAction,
    },
    snapshot::{ElecLive, ViewAction},
    trace::{colcache::ColumnCache, columns::Columns, cursor::{eng, Cursors}, draw::{draw_columns, DrawTraceSeg, TraceStyle}, take_window::{ruler_ticks, scroll_pans, TakeWindow}, view::{StoreSource, TraceSource}},
};
use fab_elec_bench::{source::Side, trigger::Slope};
use fab_elec_core::{ids::NetRef, Sel};
use fab_elec_design::{CursorSource, ProbeOf, ProbeQuantity, TimeCursor};
use fab_elec_sim::listen::TapMix;
use fab_elec_sim::trace::{LogicTrace, SignalId, TraceStore};
use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::*;
use std::{borrow::Cow, collections::HashMap, fmt::Write as _, sync::Arc};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    // Hatch undefined logic, decoder gaps and the unrecorded part of a take.
    set_type_default() do #(DrawLogicHatch::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        radius: 0.0
        pixel: fn() {
            let p = self.pos * self.rect_size
            let d = (p.x + p.y) / 5.0
            let stripe = clamp((abs(d - floor(d) - 0.5) - 0.25) * 5.0 * self.draw_pass.dpi_factor + 0.5, 0.0, 1.0)
            let a = (0.12 + 0.4 * stripe) * self.color.a
            let sdf = Sdf2d.viewport(p)
            sdf.box(0., 0., self.rect_size.x, self.rect_size.y, min(self.radius, min(self.rect_size.x, self.rect_size.y) * 0.5))
            sdf.fill(vec4(self.color.rgb, a))
            return sdf.result
        }
    }

    // A control's, a card's or a menu's face, no outline: its corner (`radius`, in `Sdf2d.box`
    // units) is the sheet's, set by the Scope per draw from `LogicScope::radius` / `radius_lg`.
    set_type_default() do #(DrawScopeFace::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        radius: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0., 0., self.rect_size.x, self.rect_size.y, min(self.radius, min(self.rect_size.x, self.rect_size.y) * 0.5))
            sdf.fill(self.color)
            return sdf.result
        }
    }

    mod.widgets.LogicScopeBase = #(LogicScope::register_widget(vm))
    mod.widgets.LogicScope = set_type_default() do mod.widgets.LogicScopeBase {
        width: Fill height: Fill
        // the sheet's corners; Fab sets its tokens on its instance (fab.radius / fab.radius_lg)
        radius: theme.corner_radius
        radius_lg: theme.container_corner_radius
        // the chrome's ground: the ruler row's left cell, the area's notch's (Fab sets its
        // fab.color_editor on its instance)
        color_chrome: theme.color_surface_container
        draw_head +: {
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.move_to(0., 0.)
                sdf.line_to(self.rect_size.x, 0.)
                sdf.line_to(0.5 * self.rect_size.x, self.rect_size.y)
                sdf.close_path()
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_name +: {
            text_style: theme.font_regular {font_size: theme.prop_font_size}
            max_lines: 1
            text_overflow: Ellipsis
        }
        draw_text +: {text_style: theme.font_regular {font_size: 7.5}}
        draw_code +: {text_style: theme.font_code {font_size: 7.5}}
        name_field +: {
            is_multiline: false
            keep_focus_on_submit: true
            margin: 0.
            padding: Inset{left: 4., right: 4., top: 2., bottom: 2.}
            draw_text +: {text_style: theme.font_regular{font_size: theme.prop_font_size}}
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawLogicHatch {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    /// The corner, in `Sdf2d.box` units: a card's, where the hatch fills one; 0 square.
    #[live]
    radius: f32,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawScopeFace {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    /// The corner, in `Sdf2d.box` units (at most half the face's height: a pill).
    #[live]
    radius: f32,
}

const MENU_ROW: f64 = 20.0;

/// The ruler row's card: the view's width less the gap at each side, the gap under it.
fn ruler_card(view: Rect) -> Rect {
    Rect { pos: dvec2(view.pos.x + LANE_GAP, view.pos.y), size: dvec2((view.size.x - 2.0 * LANE_GAP).max(0.0), RULER_H - LANE_GAP) }
}

/// A lane's card over its block (the lane and its decoders): the gap at each side and under it.
fn lane_card(view: Rect, block: Rect) -> Rect {
    Rect { pos: dvec2(view.pos.x + LANE_GAP, block.pos.y), size: dvec2((view.size.x - 2.0 * LANE_GAP).max(0.0), (block.size.y - LANE_GAP).max(0.0)) }
}
/// The panel's gap between two cards (the ruler's and each lane's), points.
const LANE_GAP: f64 = 2.0;
const ADD_ROW: f64 = 28.0;
/// The ruler row: the area's corner chip's height, its left cell the chip's.
const RULER_H: f64 = 24.0;
const DIGITAL_H: f64 = 46.0;
const ANALOG_H: f64 = 66.0;
const CHIP: f64 = 18.0;
/// A decoder's title row and each of its annotation rows.
const ROW_H: f64 = 18.0;
const SNAP: f64 = 6.0;
const FLASH: f64 = 1.4;
/// A monitored lane's level and the master's range in dB, and the meter's scale (its fader's).
const LEVEL_MIN_DB: f32 = -48.0;
const LEVEL_MAX_DB: f32 = 12.0;
/// The Scope's id for its probes ask (`SimAction::Probes`).
const SCOPE_RECORD: u64 = 0x5343_4f50_4552_4543;
/// The Scope's id as a view of the bench (`SimAction::ShowScope`, no sink of its own): while it is
/// drawn the bench runs, as it does for a visible Bench scope.
const SCOPE_VIEW: u64 = 0x5343_4f50_4556_4945;
/// The Scope's id for its ask of spilled logic in detail (`SimAction::LogicDetail`).
const SCOPE_DETAIL: u64 = 0x5343_4f50_4544_4554;
/// This Scope's request for the selected net's voltage envelope.
const SCOPE_ENVELOPE: u64 = 0x5343_4f50_4545_4e56;
/// A meter's held peak falls this fast, in dB a second (a peak meter's usual return).
const PEAK_FALL_DB: f32 = 20.0;

/// The height fraction used by Bench.
pub const PANEL_SHARE: f64 = 0.42;

/// The Take model (L3, user 11-18: "the proper recordable/scrubbable scope view"): while the
/// run has a take, every lane shows it on one fixed axis over [0, L), the play head moves over a
/// still signal, the wheel zooms about the pointer, a sideways scroll (or Shift and the wheel)
/// pans, a double-click on the ruler fits, and a press or drag on the ruler or a lane scrubs (a
/// review of that instant, silent). Cursors A and B take Alt-click and Alt-Shift-click there.
/// Each Scope keeps its own (two Scopes may show two zooms); a new one opens on the window last
/// used ([`SharedTake`]).
#[derive(Clone, Default, PartialEq)]
struct TakeView {
    /// The person chose the scope's timebase over the take (`Chip::ScopeModel`).
    scope: bool,
    /// A take was there at the last draw: one that appears opens on the Take model.
    seen: bool,
    /// The window shown and the take's end it was fitted against, once a take was seen.
    window: Option<TakeWindow>,
    end: f64,
    /// Where the axis begins: 0 for a take, the oldest instant the ring keeps while Continuous.
    origin: f64,
    /// Continuous: the window rides the front (off once the person pans or zooms away from it).
    held: bool,
    /// The window shows the whole axis (off once zoomed): it grows with a growing record.
    zoomed: bool,
    /// Follow playhead (the Scope menu, off by default): while the take plays, a zoomed window
    /// turns a page when the play head leaves it.
    follow: bool,
}

impl TakeView {
    /// The window as the axis `[origin, end]` shows it: kept, and while Continuous riding the
    /// front unless held; fitted the first time.
    fn window_for(&mut self, origin: f64, end: f64, continuous: bool) -> TakeWindow {
        let len = end - origin;
        let w = match self.window {
            None => fit_from(origin, len),
            Some(_) if !self.zoomed => fit_from(origin, len),
            Some(w) if continuous && !self.held => {
                let span = w.span().min(len);
                TakeWindow { t0: end - span, t1: end }
            }
            Some(w) if !continuous && self.end != end => w.rebound(self.end, end),
            Some(w) => shift(shift(w, -origin).clamp(len), origin),
        };
        (self.window, self.end, self.origin) = (Some(w), end, origin);
        w
    }

    /// The window turned to the page that holds `head`, its head a twentieth in, when Follow
    /// playhead is on, the window zoomed and `head` outside it; else the window as it is.
    fn page_to(&mut self, w: TakeWindow, head: f64) -> TakeWindow {
        if !self.follow || !self.zoomed || (w.t0..w.t1).contains(&head) {
            return w;
        }
        let span = w.span();
        let t0 = (head - span * 0.05).clamp(self.origin, (self.end - span).max(self.origin));
        self.place(TakeWindow { t0, t1: t0 + span });
        self.window.unwrap_or(w)
    }

    fn fit(&mut self) {
        self.window = Some(fit_from(self.origin, self.end - self.origin));
        (self.zoomed, self.held) = (false, false);
    }

    /// Zooms about `at` by `factor`; the window holds unless it still ends at the front.
    fn zoom(&mut self, at: f64, factor: f64) {
        let Some(w) = self.window else { return };
        let len = self.end - self.origin;
        let z = shift(shift(w, -self.origin).zoom(at - self.origin, factor, len), self.origin);
        self.place(z);
    }

    /// The window `from` grabbed and dragged `dx` points across a plot `width` wide.
    fn drag(&mut self, from: TakeWindow, dx: f64, width: f64) {
        let len = self.end - self.origin;
        let p = shift(shift(from, -self.origin).dragged(dx, width, len), self.origin);
        self.place(p);
    }

    fn place(&mut self, w: TakeWindow) {
        let len = self.end - self.origin;
        self.zoomed = w.span() < len * (1.0 - 1e-9);
        self.held = w.t1 < self.end - w.span() * 1e-6;
        self.window = Some(w);
    }

    /// The instant under `x` on a plot from `x0`, `width` wide, inside the axis.
    fn scrub_t(&self, x: f64, x0: f64, width: f64) -> f64 {
        let Some(w) = self.window else { return self.origin };
        w.t_of(x, x0, width).clamp(self.origin, self.end)
    }
}

/// The window on the take last used by any Scope: a Scope placed in a layout, or opened again,
/// starts there ([`LogicScope::pull_take`]) and goes its own way once it shows a take; every change
/// a Scope makes is written back ([`LogicScope::push_take`]). O(1) per event.
#[derive(Default)]
struct SharedTake {
    view: TakeView,
    generation: u64,
}

/// The whole axis `[origin, origin + len]`.
fn fit_from(origin: f64, len: f64) -> TakeWindow {
    shift(TakeWindow::fit(len), origin)
}

fn shift(w: TakeWindow, by: f64) -> TakeWindow {
    TakeWindow { t0: w.t0 + by, t1: w.t1 + by }
}

/// One wheel notch (40 points) zooms the take by this factor.
const TAKE_NOTCH: f64 = 1.25;
/// The person asks for the Scope tab at a net's lane (an explicit Scope button, as the Bench's
/// probe rows have): the shell brings the Scope forward. A plain click on a wire posts
/// [`crate::live::flags::OpenInScope`] instead, which never changes the tab in front.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowInScope(pub String);

/// A kept probe or the current selection.
struct Lane {
    net: String,
    label: String,
    selection: bool,
    net_ref: Option<NetRef>,
    of: ProbeOf,
    source: LaneSource,
    hue: usize,
    facts: NetFacts,
    /// The record's signal of the net under the numbering `row.0`.
    row: Option<(u64, Option<usize>)>,
    /// Switched to digital by its swing (kept for the lane's life).
    swing: bool,
    cols: Columns,
    cols_key: Option<(usize, usize, u64, u64, u32)>,
    /// The window's columns kept between publications: a record that grew reduces only its new
    /// columns, so a take rendering on a fixed axis costs O(new columns) a publication.
    cache: ColumnCache,
    /// Parts of the reduced window without recorded voltage: the lane's coarse envelope fills
    /// them (a lane opened mid-run, a net Realtime's trace left out). None for a digital lane drawn
    /// from its pad's edges, which cover the whole run.
    gaps: Vec<(f64, f64)>,
    runs: Vec<Run>,
    /// The spilled logic chunks the window shows in detail, by first time (`LogicTrace::spilled_in`).
    detail: Vec<f64>,
    range: (f64, f64),
}

impl Lane {
    /// A lane of a saved trace, not of the bench.
    fn replay(&self) -> bool {
        self.source.side == Side::Replay
    }

    fn kind(&self, overrides: &HashMap<String, LaneKind>) -> LaneKind {
        overrides.get(&self.net).copied().unwrap_or(if self.swing { LaneKind::Digital } else { self.facts.kind() })
    }

    fn thresholds(&self) -> Thresholds {
        self.facts.thresholds(self.range.1)
    }

    fn update_gaps(&mut self) {
        self.gaps.clear();
        let cols = &self.cols;
        let time = |x: f32| cols.t0 + f64::from(x) / f64::from(cols.cols) * (cols.t1 - cols.t0);
        let mut end = cols.t0;
        let mut connected = cols.before.is_some_and(|(_, v)| v.is_finite());
        for c in &cols.cols_data {
            if c.lo.is_finite() && c.hi.is_finite() {
                let start = time(c.x0);
                if (!connected || !c.y0.is_finite()) && start > end {
                    self.gaps.push((end, start));
                }
                end = time(c.x1);
            }
            connected = c.y1.is_finite();
        }
        if !(connected && cols.after.is_some_and(|(_, v)| v.is_finite())) && end < cols.t1 {
            self.gaps.push((end, cols.t1));
        }
    }
}

/// Visit only the parts of coarse slots that fill a gap. Both lists are in time order.
fn visit_envelope(slots: &[EnvSlot], gaps: &[(f64, f64)], mut visit: impl FnMut(&EnvSlot, f64, f64)) {
    let Some(&(first, _)) = gaps.first() else { return };
    let mut i = slots.partition_point(|slot| slot.t1 <= first);
    for &(start, end) in gaps {
        while let Some(slot) = slots.get(i).filter(|slot| slot.t0 < end) {
            let (left, right) = (slot.t0.max(start), slot.t1.min(end));
            if right > left && slot.min.is_finite() && slot.max.is_finite() && slot.mean.is_finite() {
                visit(slot, left, right);
            }
            if slot.t1 > end { break }
            i += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Chip {
    More,
    Mode,
    Follow,
    /// The take's window keeps the play head in view while it plays.
    FollowHead,
    Clear,
    Master,
    /// Saves the recording of what is heard as a WAV file.
    Save,
    /// Opens the decoder menu: the buses the lanes' names offer, then a protocol to pick lanes for.
    Decode,
    Offer(usize),
    Pick(u8),
    /// While picking a decoder's lanes: leave an optional role empty, or stop.
    Skip,
    Cancel,
    /// The time model: the take on a fixed axis, or the scope's rolling or triggered timebase.
    TakeModel,
    ScopeModel,
    /// The take's window fitted to the whole take.
    Fit,
    /// `+ add lane`: says how a lane is added.
    AddLane,
    /// The Scope menu's saved trace at this index of `ElecLive::traces`: shown in place of the run.
    Open(usize),
    /// Back to the live run from a saved trace.
    Live,
}

/// A decoder's chips: its main option (UART baud, SPI bits), SPI's mode, and remove.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DecChip {
    Option,
    Mode,
    Close,
}

/// A protocol decoder under the lanes (view state): its protocol and options, and its lines by
/// role, as nets (None: an optional role left empty).
#[derive(Clone, Debug, PartialEq)]
struct ScopeDecoder {
    id: u64,
    protocol: Protocol,
    nets: Vec<Option<String>>,
}

/// A decoder whose lanes the person is clicking, role by role.
#[derive(Clone, Debug)]
struct Picking {
    protocol: Protocol,
    nets: Vec<Option<String>>,
}

/// The UART rates and SPI word sizes a decoder's option chip steps through (None: found).
const BAUDS: [Option<f64>; 10] = [None, Some(9600.0), Some(19200.0), Some(38400.0), Some(57600.0), Some(115200.0), Some(230400.0), Some(460800.0), Some(921600.0), Some(1e6)];
const WORD_BITS: [Option<u8>; 5] = [None, Some(8), Some(16), Some(24), Some(32)];

/// A lane's height, from its menu: Small is a digital lane's own, Medium an analog lane's, Large
/// twice that. Without a choice the lane's kind picks Small or Medium. Each also takes its share
/// of the room the lanes leave (1, 3, 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerRon, DeRon)]
pub enum LaneSize {
    Small,
    Medium,
    Large,
}

impl LaneSize {
    fn of_kind(kind: LaneKind) -> Self {
        match kind {
            LaneKind::Digital => LaneSize::Small,
            LaneKind::Analog => LaneSize::Medium,
        }
    }

    fn base(self) -> f64 {
        match self {
            LaneSize::Small => DIGITAL_H,
            LaneSize::Medium => ANALOG_H,
            LaneSize::Large => 2.0 * ANALOG_H,
        }
    }

    fn share(self) -> f64 {
        match self {
            LaneSize::Small => 1.0,
            LaneSize::Medium => 3.0,
            LaneSize::Large => 6.0,
        }
    }
}

/// The lane choices and time per division a Scope instance keeps on a plate.
#[derive(Clone, Debug, Default, PartialEq, SerRon, DeRon)]
pub struct ScopeView {
    pub per_div: Option<f64>,
    pub order: Vec<String>,
    pub sizes: Vec<(String, LaneSize)>,
    pub kinds: Vec<(String, LaneKind)>,
    /// The take's window the person zoomed to and the take's end it was shown against
    /// (`[t0, t1, end]`); None shows the whole take.
    pub take: Option<[f64; 3]>,
}

/// The electronics host still shows this snapshot while it loads the opening plate.
#[derive(Default)]
struct LeavingSnapshot(std::sync::Weak<crate::DesignSnapshot>);

impl LeavingSnapshot {
    fn is_leaving(cx: &Cx, live: &ElecLive) -> bool {
        cx.get_global_ref::<Self>().is_some_and(|old| old.0.as_ptr() == Arc::as_ptr(&live.snap))
    }
}

fn bench_timebase(bench: Option<&fab_elec_lang::BenchView>) -> Timebase {
    Timebase::new(super::bench::bench_time_per_div(bench.and_then(|bench| bench.out.stop)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LaneChip {
    Kind,
    SetKind(Option<LaneKind>),
    Size(LaneSize),
    More,
    Rename,
    Trig,
    Pin,
    Listen,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
enum Menu {
    Scope,
    Decode,
    Lane(String),
    Kind(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Scope(Chip),
    Lane(usize, LaneChip),
    Level(usize),
    Decoder(u64, DecChip),
}

struct Button {
    control: Control,
    rect: Rect,
    label: Cow<'static, str>,
    selected: bool,
    enabled: bool,
}

#[derive(Clone, Debug)]
enum Press {
    Button(Control),
    Name { net: String, start: DVec2, moved: bool },
    Plot { start: DVec2, lag: f64, moved: bool, shift: bool },
    /// A press on the take's ruler: it moves the play head while held, heard as a scratch.
    Scrub,
    /// A press on the take's traces: a drag pans the window (the grabbed instant stays under the
    /// pointer), a click without a drag moves the play head there.
    Pan { start: DVec2, from: TakeWindow, moved: bool },
    Cursor(Which),
    /// A monitored lane's meter dragged as its fader.
    Level(usize),
}

/// Where things were drawn, for input until the next draw.
#[derive(Default)]
struct Placed {
    buttons: Vec<Button>,
    menu: Option<Rect>,
    ruler: Rect,
    names: Rect,
    plot: Rect,
    /// Per shown lane: lane index, its row (full width).
    rows: Vec<(usize, Rect)>,
    window: (f64, f64),
    dpi: f64,
}

impl Placed {
    fn hit(&self, pos: DVec2) -> Option<Control> {
        self.buttons.iter().rev().find(|b| b.enabled && b.rect.contains(pos)).map(|b| b.control)
    }

    fn button(&mut self, control: Control, rect: Rect, label: impl Into<Cow<'static, str>>, selected: bool) {
        self.buttons.push(Button { control, rect, label: label.into(), selected, enabled: true });
    }

    fn x_of(&self, t: f64) -> f64 {
        let (t0, t1) = self.window;
        self.plot.pos.x + (t - t0) / (t1 - t0) * self.plot.size.x
    }

    fn t_of(&self, x: f64) -> f64 {
        let (t0, t1) = self.window;
        t0 + (x - self.plot.pos.x) / self.plot.size.x.max(1.0) * (t1 - t0)
    }
}

/// What the frame's picture depends on in the live bench, drawn again when it moves: the record
/// and its length, the scrub, the numbering, the evaluated benches, whether the run plays and the
/// instant heard while the recording plays (the play head), the published envelopes, the saved
/// trace shown and the saved traces listed.
type LiveKey = (usize, usize, Option<u64>, u64, usize, bool, u64, usize, usize, usize);

fn live_key(cx: &Cx) -> LiveKey {
    let Some(live) = cx.get_global_ref::<ElecLive>() else { return LiveKey::default() };
    let sim = &live.sim;
    (
        sim.store.as_ref().map_or(0, |s| Arc::as_ptr(s) as usize),
        sim.store.as_ref().map_or(0, |s| s.len()),
        sim.scrub.map(f64::to_bits),
        sim.index.generation,
        Arc::as_ptr(&live.snap.benches) as usize,
        sim.playing,
        sim.heard.map_or(sim.t, |(t, _)| t).to_bits(),
        Arc::as_ptr(&sim.envelopes) as usize,
        live.opened.as_ref().map_or(0, |opened| Arc::as_ptr(opened) as usize),
        Arc::as_ptr(&live.traces) as usize,
    )
}

/// The logic-analyser Scope tab, registered as `mod.widgets.LogicScope`.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct LogicScope {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    /// The chrome's ground: the ruler row's left cell, under the area's notch.
    #[live]
    color_chrome: Vec4f,
    #[live]
    draw_quad: DrawColor,
    #[live]
    draw_face: DrawScopeFace,
    #[live]
    draw_head: DrawColor,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_hatch: DrawLogicHatch,
    /// A control's corner, and a card's or a menu's, in `Sdf2d.box` units: the sheet's tokens.
    #[live]
    radius: f64,
    #[live]
    radius_lg: f64,
    #[live]
    draw_seg: DrawTraceSeg,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_code: DrawText,
    #[live]
    name_field: TextInput,
    #[rust]
    area: Area,
    #[rust]
    lanes: Vec<Lane>,
    /// The lanes' order by net, as the person dragged them (view state).
    #[rust]
    order: Vec<String>,
    #[rust]
    overrides: HashMap<String, LaneKind>,
    /// The heights chosen in the lane menu, by net (view state); a lane without one follows its kind.
    #[rust]
    sizes: HashMap<String, LaneSize>,
    #[rust]
    bench: Option<String>,
    #[rust]
    probes: Vec<fab_elec_lang::BenchProbe>,
    #[rust]
    probes_key: Option<(std::sync::Weak<crate::DesignSnapshot>, Option<Arc<str>>)>,
    /// The next sync starts fresh lanes, including their inferred signal kinds, on this plate.
    #[rust]
    reset_lanes: bool,
    /// The saved trace the lanes are of (`ElecLive::opened`, by address); None: the live run's.
    #[rust]
    trace_key: Option<usize>,
    #[rust]
    source: Option<StoreSource>,
    #[rust]
    timebase: Option<Timebase>,
    #[rust]
    trigger: Option<Trigger>,
    /// The triggered capture shown: the record its event was found in, and the event. Held as an
    /// instrument holds its last acquisition (`logic::hold`).
    #[rust]
    capture: Option<(Arc<TraceStore>, f64)>,
    #[rust]
    trigger_key: Option<(usize, usize, u64, u64, bool)>,
    #[rust]
    cursors: Cursors,
    /// The lane a shift-drag measured on (its net): the readout adds its ΔV between the cursors.
    #[rust]
    measure_lane: Option<String>,
    /// The trigger event the cursors were last placed against (they follow it).
    #[rust]
    cursor_trigger: Option<f64>,
    #[rust]
    hover_t: Option<f64>,
    #[rust]
    press: Option<Press>,
    /// The instant the hand holds the line at, while it scrubs.
    #[rust]
    scrubbed: Option<f64>,
    #[rust]
    insert_at: Option<usize>,
    /// Where a lane dragged by its name is: its chip rides the pointer there.
    #[rust]
    drag_at: Option<DVec2>,
    #[rust]
    wheel: f64,
    #[rust]
    scroll_y: f64,
    #[rust]
    reveal: Option<String>,
    #[rust]
    selected: Option<String>,
    #[rust]
    flash: Option<(String, f64)>,
    #[rust]
    /// The lane being renamed, by net (lanes may re-sync meanwhile), and its field's place.
    rename: Option<(String, Rect)>,
    #[rust]
    rename_focus: bool,
    #[rust]
    placed: Placed,
    #[rust]
    live_key: LiveKey,
    #[rust]
    next_frame: NextFrame,
    /// The app's installed tap as last seen (`listen::ElecAudio::tap`): its count, its voices'
    /// nets (its voice order) and its shared controls and meters; each voice's meter as shown
    /// (RMS, held peak).
    #[rust]
    tap_gen: u64,
    #[rust]
    tap_nets: Vec<String>,
    #[rust]
    mix: Option<Arc<TapMix>>,
    #[rust]
    meters: Vec<(f32, f32)>,
    /// The meters move: draw every frame.
    #[rust]
    metering: bool,
    /// When the meters were last read (app seconds): the held peak falls by the time since.
    #[rust]
    meter_at: f64,
    /// The lanes' rows last asked of the bench to keep whole (`SimAction::Probes`).
    #[rust]
    asked_probes: Vec<usize>,
    /// The nets whose coarse envelope this Scope asked of the bench (`SimAction::Envelope`), one a
    /// lane: drawn where the record has no point of them (a lane opened mid-run, a net Realtime's
    /// trace left out). Sorted.
    #[rust]
    asked_envelopes: Vec<NetRef>,
    /// The spilled logic chunks the drawn digital lanes show in detail (their lanes' `detail`), by
    /// signal and first time, and those last asked of the bench (`SimAction::LogicDetail`).
    #[rust]
    detail: Vec<(SignalId, f64)>,
    #[rust]
    asked_detail: Vec<(SignalId, f64)>,
    /// Whether the bench was told this Scope is drawn (`SimAction::ShowScope`).
    #[rust]
    shown: bool,
    /// The drawing id when a redraw was asked and not yet walked: two draw events later the Scope
    /// is hidden (another tab) and the bench is told.
    #[rust]
    asked_draw: Option<u64>,
    /// The protocol decoders under the lanes (view state), in the order they were added, and the
    /// next one's id.
    #[rust]
    decoders: Vec<ScopeDecoder>,
    #[rust]
    next_decoder: u64,
    /// The decode worker, while there are decoders; its newest rows and their publication number.
    #[rust]
    decode_worker: Option<DecodeWorker>,
    #[rust]
    decoded: Option<(u64, Arc<Decoded>)>,
    /// What the worker was last handed: the record (pointer, length) and the decoders resolved to
    /// its signals (`decoders` order).
    #[rust]
    asked_decode: Option<(usize, usize, Arc<[Setup]>)>,
    #[rust]
    menu: Option<Menu>,
    #[rust]
    menu_scroll: f64,
    #[rust]
    pointer: Option<DVec2>,
    #[rust]
    focused_lane: Option<String>,
    /// The buses the decoder menu offers (by its `Offer` chips).
    #[rust]
    offers: Vec<(Protocol, Vec<Option<String>>)>,
    #[rust]
    picking: Option<Picking>,
    /// A row's pieces of the window, reused frame to frame.
    #[rust]
    pieces: Vec<Piece>,
    #[rust]
    take: TakeView,
    /// The [`SharedTake`] generation `take` was last taken from or written as.
    #[rust]
    take_generation: u64,
    /// Extra lane height after reserving the footer and decoder rows.
    #[rust]
    stretch: Vec<f64>,
    /// The nets the run bench's output jacks carry: their lanes are the output, drawn white.
    #[rust]
    outs: Vec<String>,
    /// `+ add lane` was pressed: its probing hint replaces the gesture hint.
    #[rust]
    add_hint: bool,
}

fn with_alpha(c: Vec4f, a: f32) -> Vec4f {
    Vec4f { w: a, ..c }
}

/// Whether a lane's name plays a role of a protocol: it ends with one of the role's usual names,
/// in upper case letters and digits (`I2C0_SCL`, `SPI_MOSI`, `SRCLK`, `LATCH`, `TXD`).
fn plays_role(name: &str, protocol: &Protocol, role: usize) -> bool {
    let names: &[&str] = match (protocol, role) {
        (Protocol::Uart { .. }, _) => &["TX", "RX", "TXD", "RXD", "UART"],
        (Protocol::I2c, 0) => &["SCL"],
        (Protocol::I2c, _) => &["SDA"],
        (Protocol::Spi { .. }, 0) => &["SCLK", "SCK", "CLK", "SPC"],
        (Protocol::Spi { .. }, 1) => &["MOSI", "COPI", "SDO", "SER", "DATA", "DIN", "DS"],
        (Protocol::Spi { .. }, 2) => &["MISO", "CIPO", "DOUT"],
        (Protocol::Spi { .. }, _) => &["CS", "SS", "NSS", "CSN", "NCS", "LATCH", "RCLK", "STB", "STROBE", "LOAD"],
    };
    let name: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect();
    names.iter().any(|role| name.ends_with(role))
}

/// Where a level or a reading sits on a meter, 0 to 1 over [LEVEL_MIN_DB, LEVEL_MAX_DB].
fn meter_x(db: f32) -> f64 {
    f64::from(((db - LEVEL_MIN_DB) / (LEVEL_MAX_DB - LEVEL_MIN_DB)).clamp(0.0, 1.0))
}

fn db_label(db: f32) -> String {
    if db.abs() < 0.05 { "0 dB".into() } else { format!("{db:+.0} dB") }
}

fn rgba(c: [f32; 4], a: f32) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * a)
}

/// The window the Scope shows on the take, for the bar's overview (`ScopeWindowShown`).
fn show_window(cx: &mut Cx, window: Option<(f64, f64)>) {
    if !cx.has_global::<ScopeWindowShown>() {
        cx.set_global(ScopeWindowShown::default());
    }
    let shown = cx.get_global::<ScopeWindowShown>();
    if shown.0 != window {
        shown.0 = window;
    }
}

/// `a` moved toward `b` by `t` (0 is `a`).
fn mix_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn per_div_label(per_div: f64) -> String {
    format!("{}/div", eng(per_div, "s", 1))
}

fn slope_word(slope: Slope) -> &'static str {
    match slope {
        Slope::Rising => "rising",
        Slope::Falling => "falling",
        Slope::Either => "either edge",
    }
}

fn slope_mark(slope: Slope) -> &'static str {
    match slope {
        Slope::Rising => "↑",
        Slope::Falling => "↓",
        Slope::Either => "↕",
    }
}

impl LogicScope {
    /// Called once at a plate switch, before the electronics host starts its load.
    pub fn leaving_plate(cx: &mut Cx) {
        if let Some(snap) = cx.get_global_ref::<ElecLive>().map(|live| Arc::downgrade(&live.snap)) {
            cx.global::<LeavingSnapshot>().0 = snap;
        }
    }

    /// Only choices that differ from the bench defaults need a place in the view file.
    pub fn held_view(&self, cx: &Cx) -> Option<ScopeView> {
        let per_div = self.timebase.map(|tb| tb.per_div).filter(|per_div| {
            let Some(live) = cx.get_global_ref::<ElecLive>() else { return true };
            LeavingSnapshot::is_leaving(cx, live) || *per_div != bench_timebase(live.run_bench()).per_div
        });
        let take = self.take.window.filter(|_| self.take.zoomed).map(|w| [w.t0, w.t1, self.take.end]);
        if per_div.is_none() && self.order.is_empty() && self.sizes.is_empty() && self.overrides.is_empty() && take.is_none() {
            return None;
        }
        let mut sizes: Vec<_> = self.sizes.iter().map(|(net, &size)| (net.clone(), size)).collect();
        sizes.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut kinds: Vec<_> = self.overrides.iter().map(|(net, &kind)| (net.clone(), kind)).collect();
        kinds.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        Some(ScopeView { per_div, order: self.order.clone(), sizes, kinds, take })
    }

    pub fn restore_view(&mut self, cx: &mut Cx, view: Option<ScopeView>) {
        let view = view.unwrap_or_default();
        self.order = view.order;
        self.sizes = view.sizes.into_iter().collect();
        self.overrides = view.kinds.into_iter().collect();
        self.timebase = view.per_div.map(Timebase::new);
        // The take's window the person left, held where they put it; else the whole take.
        // Their choice of model and Follow playhead stay the Scope's own.
        match view.take {
            Some([t0, t1, end]) => {
                (self.take.window, self.take.end) = (Some(TakeWindow { t0, t1 }), end);
                (self.take.zoomed, self.take.held) = (true, true);
            }
            None => {
                self.take.window = None;
                (self.take.zoomed, self.take.held) = (false, false);
            }
        }
        self.reset_lanes = true;
        self.probes_key = None;
        self.area.redraw(cx);
    }

    /// Reveals and flashes the net's probe, creating a temporary lane when it has none.
    pub fn show_lane(&mut self, cx: &mut Cx, net: &str) -> bool {
        self.sync_probes(cx);
        let lane = self.lanes.iter().find(|lane| lane.net == net);
        let there = lane.is_some();
        let selected = lane.is_none_or(|lane| lane.selection).then_some(net);
        if self.selected.as_deref() != selected {
            self.selected = selected.map(str::to_owned);
            self.probes_key = None;
            // Start keeping the selected signal even when the Scope is behind another tab.
            self.sync_probes(cx);
        }
        self.focused_lane = Some(net.to_owned());
        self.reveal = Some(net.to_owned());
        // A lane still to come flashes from when it arrives (NaN until then).
        self.flash = Some((net.to_owned(), if there { cx.seconds_since_app_start() } else { f64::NAN }));
        self.area.redraw(cx);
        there || self.lanes.first().is_some_and(|lane| lane.net == net)
    }

    /// The views' selection moved to `sel` (V8.3): a net, or a pin's net, gets the selection lane
    /// (or lights its kept probe's lane, [`Self::show_lane`]); anything else, or none, takes the
    /// selection lane away. O(nets of the unit) for a net's name.
    pub fn follow_selection(&mut self, cx: &mut Cx, sel: Option<&Sel>) {
        let net = sel.and_then(|sel| {
            let design = cx.get_global_ref::<ElecLive>()?.snap.live();
            let (board, id) = match sel {
                Sel::Net(net) => (net.board, net.net),
                Sel::Pin(pin) => (pin.board, design.unit(pin.board)?.circuit.net_of(pin.comp, pin.pin)?),
                _ => return None,
            };
            design.unit(board)?.circuit.nets.get(id.ix()).map(|net| net.name.clone())
        });
        match net {
            Some(net) => {
                self.show_lane(cx, &net);
            }
            None => {
                if self.selected.take().is_some() {
                    self.probes_key = None;
                    self.sync_probes(cx);
                }
            }
        }
    }

    /// The lanes' names in display order.
    pub fn lane_names(&self) -> Vec<String> {
        self.lanes.iter().map(|lane| lane.label.clone()).collect()
    }

    /// The shown time window, seconds.
    pub fn window(&self) -> (f64, f64) {
        self.placed.window
    }

    /// Rebuilds the lane list when the design, bench or selection changes.
    fn sync_lanes(&mut self, cx: &Cx) -> bool {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return false };
        let trace = live.opened.as_ref().map(|opened| Arc::as_ptr(opened) as usize);
        if self.trace_key == trace && self.probes_key.as_ref().is_some_and(|(snap, choice)| snap.as_ptr() == Arc::as_ptr(&live.snap) && *choice == live.chosen_bench) {
            return false;
        }
        self.probes_key = Some((Arc::downgrade(&live.snap), live.chosen_bench.clone()));
        if LeavingSnapshot::is_leaving(cx, live) {
            return false;
        }
        if std::mem::take(&mut self.reset_lanes) || self.trace_key != trace {
            // Another plate or trace can use the same net name for a different signal.
            (self.trace_key, self.scrubbed) = (trace, None);
            self.lanes.clear();
        }
        if let Some(opened) = &live.opened {
            // A saved trace: a lane per channel, its row the channel's; nothing of the bench.
            (self.bench, self.probes, self.outs) = (None, Vec::new(), Vec::new());
            // a lane is its channel's net, shown by the label it had in the Scope when saved
            let label = |i: usize, net: &str| opened.labels.get(i).filter(|label| !label.is_empty()).map_or_else(|| net.to_owned(), String::clone);
            self.lanes = opened.layout.channels.iter().enumerate().map(|(i, channel)| Lane {
                source: LaneSource { side: Side::Replay, source: None, of: LaneOf::Channel(channel.id) },
                net: channel.name.clone(), label: label(i, &channel.name), of: ProbeOf::v(channel.name.clone()), selection: false, net_ref: None, hue: i,
                facts: Default::default(), row: Some((opened.store.generation, Some(i))), swing: false, cols: Columns::default(), cols_key: None,
                cache: ColumnCache::new(), gaps: Vec::new(), runs: Vec::new(), detail: Vec::new(), range: (0.0, 0.0),
            }).collect();
            self.settle_lanes();
            return true;
        }
        let bench = live.run_bench();
        self.bench = bench.map(|bench| bench.out.name.clone());
        self.probes = bench.map_or_else(Vec::new, |bench| bench.out.probes.clone());
        self.outs = bench.and_then(|bench| bench.out.circuit(live.snap.live()).map(|circuit| listen::jack_tap_voices(circuit, bench.out.scope_id())))
            .map_or_else(Vec::new, |voices| voices.into_iter().map(|voice| voice.net).collect());
        let gesture = cx.get_global_ref::<PeekGesture>();
        let design = live.snap.live();
        let circuit = bench.and_then(|b| b.out.circuit(design));
        let mut old: HashMap<String, Lane> = self.lanes.drain(..).map(|lane| (lane.net.clone(), lane)).collect();
        let mut lane_for = |of: &ProbeOf, label: &str, i: usize, selection: bool| {
            let net = &of.target;
            // Different boards can have identically named nets. A bench probe belongs to its scope.
            let signal = |c: &fab_elec_circuit::Circuit| {
                let id = c.net(net)?.id;
                Some((Some(NetRef { board: c.unit, net: id }), net_facts(c, id)))
            };
            let (net_ref, facts) = circuit.and_then(signal).or_else(|| {
                if selection { design.units.iter().find_map(|unit| signal(&unit.circuit)) } else { None }
            }).unwrap_or_default();
            let hue = gesture.map_or(i, |gesture| gesture.hue_of(net, i));
            match old.remove(net) {
                Some(mut lane) => {
                    label.clone_into(&mut lane.label);
                    lane.of.clone_from(of);
                    if lane.net_ref != net_ref {
                        lane.row = None;
                    }
                    if lane.selection != selection || lane.facts != facts {
                        lane.cols_key = None;
                    }
                    lane.selection = selection;
                    lane.net_ref = net_ref;
                    lane.facts = facts;
                    lane.hue = hue;
                    lane
                }
                None => Lane {
                    source: LaneSource::sim(net), net: net.clone(), label: label.to_owned(), of: of.clone(), selection, net_ref, hue, facts,
                    row: None, swing: false, cols: Columns::default(), cols_key: None, cache: ColumnCache::new(), gaps: Vec::new(), runs: Vec::new(), detail: Vec::new(), range: (0.0, 0.0),
                },
            }
        };
        let mut lanes = Vec::new();
        for (i, probe) in self.probes.iter().enumerate() {
            if probe.of.quantity == ProbeQuantity::V && probe.of.reference.is_none() {
                lanes.push(lane_for(&probe.of, &probe.label, i, false));
            }
        }
        if let Some(net) = &self.selected {
            // a kept probe shows it (its lane lights instead), or the design has no such net (another plate)
            if lanes.iter().any(|lane| &lane.net == net) || !design.units.iter().any(|unit| unit.circuit.net(net).is_some()) {
                self.selected = None;
            } else {
                self.order.retain(|ordered| ordered != net);
                lanes.insert(0, lane_for(&ProbeOf::v(net.clone()), &format!("Selection · {net}"), self.probes.len(), true));
            }
        }
        self.lanes = lanes;
        self.settle_lanes();
        if self.timebase.is_none() {
            self.timebase = Some(bench_timebase(bench));
        }
        true
    }

    /// What a new lane list drops: the places drawn for the old one, a press on one of its lanes
    /// and a trigger on a lane it no longer has.
    fn settle_lanes(&mut self) {
        // Keep the selection first, then the person's order, then the bench or trace's order.
        if !self.order.is_empty() {
            self.lanes.sort_by_key(|lane| {
                (!lane.selection, self.order.iter().position(|net| net == &lane.net).unwrap_or(usize::MAX))
            });
        }
        // Input may arrive before the next draw places the new lane indices.
        self.placed.rows.clear();
        self.placed.buttons.clear();
        if matches!(self.press, Some(Press::Level(_) | Press::Button(Control::Lane(..) | Control::Level(_)))) {
            self.press = None;
        }
        if self.trigger.as_ref().is_some_and(|t| !self.lanes.iter().any(|lane| lane.net == t.net)) {
            self.trigger = None;
            self.capture = None;
        }
    }

    /// Each lane's record signal under the live numbering (a saved trace's lanes keep theirs).
    fn sync_rows(&mut self, cx: &Cx) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        if live.opened.is_some() {
            return;
        }
        let index = &live.sim.index;
        for lane in &mut self.lanes {
            if lane.row.is_some_and(|(g, _)| g == index.generation) {
                continue;
            }
            let row = lane.net_ref.and_then(|net| index.net(net));
            lane.row = Some((index.generation, row));
            lane.cols_key = None;
        }
    }

    fn record(cx: &Cx) -> Option<(Arc<TraceStore>, Option<f64>, bool)> {
        let live = cx.get_global_ref::<ElecLive>()?;
        if let Some(opened) = &live.opened {
            return (!opened.store.is_empty()).then(|| (opened.store.clone(), None, false));
        }
        let store = live.sim.store.clone().filter(|s| s.generation == live.sim.index.generation && !s.is_empty())?;
        Some((store, live.sim.scrub.or(live.sim.heard.map(|(t, _)| t)), live.sim.playing))
    }

    /// Whether the views review an instant of the take (the shared cursor parked on it).
    fn reviewing(cx: &Cx) -> bool {
        cx.get_global_ref::<ElecLive>().is_some_and(|live| live.opened.is_none() && live.sim.scrub.is_some())
    }

    /// The window's anchor in a (non-empty) record: its newest instant, or the instant reviewed or
    /// heard (`head`: the window follows the play head), and its first.
    fn anchor(store: &TraceStore, head: Option<f64>, trigger: Option<f64>) -> Anchor {
        let start = store.time.at(0);
        let end = store.time.at(store.len() - 1);
        Anchor { head: head.map_or(end, |t| t.clamp(start, end)), start, trigger }
    }

    /// The trigger event shown.
    fn shown_trigger(&self) -> Option<f64> {
        self.capture.as_ref().map(|(_, t)| *t)
    }

    /// The record the lanes draw: the trigger's capture while it holds one, else the newest.
    fn shown(&self, newest: Option<(Arc<TraceStore>, Option<f64>, bool)>) -> Option<(Arc<TraceStore>, Option<f64>, bool)> {
        let (store, scrub, playing) = newest?;
        Some((self.capture.as_ref().map_or(store, |(capture, _)| capture.clone()), scrub, playing))
    }

    fn lane_index(&self, net: &str) -> Option<usize> {
        self.lanes.iter().position(|lane| lane.net == net)
    }

    /// The capture to show: the newest record searched again for the trigger's event when it, the
    /// trigger, the span or the run's state changed; without a new event the last capture holds.
    fn sync_trigger(&mut self, store: &Arc<TraceStore>, scrub: Option<f64>, playing: bool) {
        let Some(trigger) = self.trigger.clone() else {
            self.capture = None;
            return;
        };
        let Some(tb) = self.timebase else { return };
        let key = (Arc::as_ptr(store) as usize, store.len(), scrub.map_or(0, f64::to_bits), tb.per_div.to_bits(), playing);
        if self.trigger_key == Some(key) {
            return;
        }
        self.trigger_key = Some(key);
        let Some(lane) = self.lane_index(&trigger.net).map(|i| &self.lanes[i]) else { return };
        let Some((_, Some(k))) = lane.row else { return };
        let level = match lane.kind(&self.overrides) {
            LaneKind::Digital => lane.thresholds().middle(),
            LaneKind::Analog => 0.5 * (lane.range.0 + lane.range.1),
        };
        let anchor = Self::anchor(store, scrub, None);
        let found = match logic_of(store, k).filter(|_| lane.kind(&self.overrides) == LaneKind::Digital) {
            Some(trace) => find_logic_trigger(trace, &anchor, tb.span(), trigger.slope, trigger.mode, playing),
            None => find_trigger(&StoreSource { store: store.clone() }, k, &anchor, tb.span(), level, trigger.slope, trigger.mode, playing),
        };
        let found = found.map(|t| (store.clone(), t));
        self.capture = hold(self.capture.take(), found, trigger.mode, anchor.head, tb.span());
    }

    fn redraw_soon(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }

    /// The end of the Take model's axis (`take_axis`): None while the scope's timebase shows.
    fn take_end(&self, cx: &Cx) -> Option<f64> {
        self.take_axis(cx).map(|(_, end, _)| end)
    }

    /// The Take model's axis: `(origin, end, continuous)`, the take `[0, L)`, or while the run is
    /// Continuous the record the ring keeps, ending at its newest point.
    fn take_axis(&self, cx: &Cx) -> Option<(f64, f64, bool)> {
        let live = cx.get_global_ref::<ElecLive>()?;
        let sim = &live.sim;
        if self.take.scope {
            return None;
        }
        // a saved trace: its record whole, from its first sample to its last
        if let Some(store) = live.opened.as_ref().map(|opened| &opened.store) {
            let (first, last) = (store.time.at(0), store.time.at(store.len().saturating_sub(1)));
            return (store.len() > 1 && first.is_finite() && last > first).then_some((first, last, false));
        }
        if sim.status.continuous {
            let (first, last) = sim.record;
            return (last.is_finite() && first.is_finite() && last > first).then_some((first, last, true));
        }
        let end = sim.status.take?;
        (end.is_finite() && end > 0.0).then_some((0.0, end, false))
    }



    /// The take at the instant under `x`: the shared cursor goes there (every view shows it, the
    /// run goes on) and the sound scratches there, at the hand's speed and direction, paused too.
    fn scrub_at(&mut self, cx: &mut Cx, x: f64) {
        if self.take.window.is_none() { return }
        let plot = self.placed.plot;
        let t = self.take.scrub_t(x, plot.pos.x, plot.size.x);
        if self.trace_key.is_some() {
            // a saved trace's play head is the Scope's own: the run and the sound stay where they are
            self.scrubbed = Some(t);
            self.redraw_soon(cx);
            return;
        }
        cx.action(ViewAction::Cursor(TimeCursor { t, playing: false, rate: 1.0, source: CursorSource::Sim, fw: None }));
        crate::live::listen::scrub_audio(cx, Some(t));
        self.scrubbed = Some(t);
        self.redraw_soon(cx);
    }

    /// The hand lets go of the line: playing, the take goes on from there (heard, or by the host's
    /// silent clock) and the line follows it again; paused or stopped, both hold there until Play
    /// (the line on the instant played back, held there, while the take plays back; else the
    /// review stays).
    fn end_scrub(&mut self, cx: &mut Cx) {
        if self.trace_key.is_some() {
            // the line stays where the hand left it in the trace
            self.redraw_soon(cx);
            return;
        }
        crate::live::listen::scrub_audio(cx, None);
        if let Some(t) = self.scrubbed.take() {
            if cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.playing) || crate::live::listen::plays_back(cx) {
                crate::live::listen::seek_clock(cx, t);
                cx.action(ViewAction::Cursor(TimeCursor { t, playing: true, rate: 1.0, source: CursorSource::Sim, fw: None }));
            }
        }
        self.redraw_soon(cx);
    }

    fn zoom(&mut self, cx: &mut Cx, steps: i32, fraction: f64) {
        if let (Some(w), Some(_)) = (self.take.window, self.take_end(cx)) {
            let at = w.t0 + fraction * w.span();
            self.take.zoom(at, TAKE_NOTCH.powi(-steps));
            self.redraw_soon(cx);
            return;
        }
        let playing = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.playing);
        let (shown, trigger) = (self.shown(Self::record(cx)), self.shown_trigger());
        let Some(tb) = self.timebase.as_mut() else { return };
        match shown {
            Some((store, scrub, _)) => tb.zoom(&Self::anchor(&store, scrub, trigger), steps, fraction, playing),
            None => tb.per_div = step_125(tb.per_div, steps).clamp(super::logic::MIN_DIV, super::logic::MAX_DIV),
        }
        self.trigger_key = None;
        self.redraw_soon(cx);
    }

    /// The instant under `x`, snapped to the nearest logic edge of any digital lane within
    /// [`SNAP`] points: a logic trace's edge at its exact time.
    fn snapped(&self, x: f64) -> f64 {
        let p = &self.placed;
        let cols = (p.plot.size.x * p.dpi).ceil().max(1.0);
        let per_col = p.plot.size.x.max(1.0) / cols;
        let digital = || self.lanes.iter().filter(|lane| lane.kind(&self.overrides) == LaneKind::Digital);
        let Some(edge) = nearest_edge(digital().map(|lane| &lane.runs[..]), ((x - p.plot.pos.x) / per_col) as f32, (SNAP / per_col) as f32) else {
            return p.t_of(x);
        };
        let t = p.t_of(p.plot.pos.x + f64::from(edge) * per_col);
        let col_t = (p.window.1 - p.window.0) / cols;
        let exact = self.source.as_ref().into_iter().flat_map(|source| {
            digital().filter_map(|lane| logic_of(&source.store, lane.row?.1?)).filter_map(|trace| nearest_logic_edge(trace, t, col_t))
        });
        exact.min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or(t)
    }

    /// The measured lane's ΔV between the cursors, as `ΔV Mix 12.3 mV`, while both are set and the
    /// shown record holds the lane's values at both (during a draw). O(log n) per cursor.
    fn measured_dv(&self) -> Option<String> {
        let (Some(a), Some(b)) = (self.cursors.a, self.cursors.b) else { return None };
        let lane = self.lanes.iter().find(|lane| Some(&lane.net) == self.measure_lane.as_ref())?;
        let (Some((_, Some(k))), Some(source)) = (lane.row, self.source.as_ref()) else { return None };
        let at = |t: f64| source.value_at(k, t).map(f64::from).filter(|v| v.is_finite());
        let dv = at(b)? - at(a)?;
        Some(format!("ΔV {} {}", lane.label, eng(dv, "V", 4)))
    }

    fn set_cursor(&mut self, which: Which, t: f64) {
        match which {
            Which::A => self.cursors.a = Some(t),
            Which::B => self.cursors.b = Some(t),
        }
    }

    fn cursor_near(&self, x: f64) -> Option<Which> {
        let p = &self.placed;
        let near = |t: Option<f64>| t.map(|t| (p.x_of(t) - x).abs()).filter(|d| *d <= 4.0);
        match (near(self.cursors.a), near(self.cursors.b)) {
            (Some(a), Some(b)) => Some(if a <= b { Which::A } else { Which::B }),
            (Some(_), None) => Some(Which::A),
            (None, Some(_)) => Some(Which::B),
            _ => None,
        }
    }

    fn lane_at_y(&self, y: f64) -> Option<usize> {
        self.placed.rows.iter().find(|(_, r)| y >= r.pos.y && y < r.pos.y + r.size.y).map(|(i, _)| *i)
    }

    /// Where a dragged lane would land: the gap between rows nearest the pointer.
    fn insertion(&self, y: f64) -> usize {
        let rows = &self.placed.rows;
        let mut at = rows.iter().filter(|(_, r)| y > r.pos.y + r.size.y * 0.5).count();
        if let Some(first) = rows.first() {
            at += first.0;
        }
        at.clamp(usize::from(self.lanes.first().is_some_and(|lane| lane.selection)), self.lanes.len())
    }

    fn commit_reorder(&mut self, from: usize, to: usize) {
        move_to_gap(&mut self.lanes, from, to);
        self.order = self.lanes.iter().filter(|lane| !lane.selection).map(|lane| lane.net.clone()).collect();
    }

    fn lane_chip(&mut self, cx: &mut Cx, lane: usize, chip: LaneChip) {
        let Some(l) = self.lanes.get(lane) else { return };
        let net = l.net.clone();
        match chip {
            LaneChip::Kind => self.open_menu(Menu::Kind(net)),
            LaneChip::More => self.open_menu(Menu::Lane(net)),
            LaneChip::Rename => self.begin_rename(cx, lane),
            LaneChip::Pin => {
                if let Some(action) = host_bench(cx).and_then(|bench| click_probe_op(&bench, &self.probes, &net)) {
                    cx.widget_action(self.uid, action);
                }
            }
            LaneChip::Size(size) => {
                // the kind's own height is no choice: the lane follows its kind again
                if size == LaneSize::of_kind(self.lanes[lane].kind(&self.overrides)) {
                    self.sizes.remove(&net);
                } else {
                    self.sizes.insert(net, size);
                }
            }
            LaneChip::SetKind(kind) => {
                match kind {
                    Some(kind) => self.overrides.insert(net, kind),
                    None => self.overrides.remove(&net),
                };
                self.lanes[lane].cols_key = None;
            }
            LaneChip::Trig => {
                self.trigger = next_trigger(self.trigger.take(), &net);
                self.capture = None;
                self.trigger_key = None;
            }
            LaneChip::Listen => {
                let on = !listening(cx, &net);
                listen::listen(cx, &net, on);
                self.follow_listen(cx);
            }
            LaneChip::Close => {
                // a lane closed is no longer heard, whichever view switched it on
                listen::listen(cx, &net, false);
                if l.selection {
                    self.selected = None;
                    self.probes_key = None;
                    self.sync_probes(cx);
                } else if let Some(action) = self.bench.as_ref().and_then(|bench| remove_probe_op(bench, &self.probes, &net)) {
                    cx.widget_action(self.uid, action);
                }
            }
        }
        self.redraw_soon(cx);
    }

    fn chip(&mut self, cx: &mut Cx, chip: Chip) {
        match chip {
            Chip::More => {
                self.open_menu(Menu::Scope);
                // the saved traces it lists, read again from the project's folder
                cx.action(TraceAction::List);
            }
            Chip::Open(i) => {
                if let Some(path) = cx.get_global_ref::<ElecLive>().and_then(|live| live.traces.get(i).cloned()) {
                    cx.action(TraceAction::Open(path));
                }
            }
            Chip::Live => cx.action(TraceAction::Close),
            Chip::Mode => {
                if let Some(t) = &mut self.trigger {
                    t.mode = if t.mode == TrigMode::Auto { TrigMode::Normal } else { TrigMode::Auto };
                    self.trigger_key = None;
                }
            }
            Chip::Follow => {
                if let Some(tb) = &mut self.timebase {
                    tb.lag = 0.0;
                }
                // Continuous: the window rides the front again
                self.take.held = false;
            }
            Chip::Clear => (self.cursors, self.measure_lane) = (Cursors::default(), None),
            Chip::Master => {}
            // a refusal shows in the status too
            Chip::Save => drop(listen::save_recording(cx, listen::recording_path())),
            Chip::Decode => self.open_menu(Menu::Decode),
            Chip::Offer(i) => {
                if let Some((protocol, nets)) = self.offers.get(i).cloned() {
                    self.add_decoder(cx, protocol, nets);
                }
            }
            Chip::Pick(i) => {
                self.picking = Protocol::ALL.get(usize::from(i)).map(|&protocol| Picking { protocol, nets: Vec::new() });
            }
            Chip::Skip => self.pick(cx, None),
            Chip::Cancel => self.picking = None,
            Chip::TakeModel => self.take.scope = false,
            Chip::FollowHead => self.take.follow = !self.take.follow,
            Chip::ScopeModel => self.take.scope = true,
            Chip::Fit => self.take.fit(),
            Chip::AddLane => self.add_hint = !self.add_hint,
        }
        self.redraw_soon(cx);
    }

    /// The next role of the decoder being picked takes `net` (None: left empty, when optional);
    /// the last one adds the decoder.
    fn pick(&mut self, cx: &mut Cx, net: Option<String>) {
        let Some(picking) = &mut self.picking else { return };
        let spec = picking.protocol.spec();
        if net.is_none() && !spec.inputs[picking.nets.len()].optional {
            return;
        }
        picking.nets.push(net);
        if picking.nets.len() == spec.inputs.len() {
            if let Some(Picking { protocol, nets }) = self.picking.take() {
                self.add_decoder(cx, protocol, nets);
            }
        }
        self.redraw_soon(cx);
    }

    /// Adds a decoder; the first starts the decode worker.
    fn add_decoder(&mut self, cx: &mut Cx, protocol: Protocol, nets: Vec<Option<String>>) {
        self.next_decoder += 1;
        self.decoders.push(ScopeDecoder { id: self.next_decoder, protocol, nets });
        if self.decode_worker.is_none() {
            self.decode_worker = DecodeWorker::start(cx);
            (self.decoded, self.asked_decode) = (None, None);
        }
    }

    /// A decoder's chip: step its main option or SPI's mode (it decodes again), or remove it; the
    /// last one removed stops the worker.
    fn decoder_chip(&mut self, cx: &mut Cx, id: u64, chip: DecChip) {
        fn next_of<T: Copy + PartialEq>(all: &[T], now: T) -> T {
            all[all.iter().position(|v| *v == now).map_or(0, |i| i + 1) % all.len()]
        }
        let Some(at) = self.decoders.iter().position(|d| d.id == id) else { return };
        match (chip, &mut self.decoders[at].protocol) {
            (DecChip::Option, Protocol::Uart { baud }) => *baud = next_of(&BAUDS, *baud),
            (DecChip::Option, Protocol::Spi { bits, .. }) => *bits = next_of(&WORD_BITS, *bits),
            (DecChip::Mode, Protocol::Spi { mode, .. }) => *mode = (*mode + 1) % 4,
            (DecChip::Close, _) => {
                self.decoders.remove(at);
                if self.decoders.is_empty() {
                    (self.decode_worker, self.decoded, self.asked_decode) = (None, None, None);
                }
            }
            _ => {}
        }
        self.redraw_soon(cx);
    }

    /// The buses the lanes' names make (I2C SCL and SDA; SPI's clock and data, then MISO and a
    /// chip select or latch when there; a UART per TX or RX lane), less those already decoded.
    /// O(lanes x roles).
    fn offers(&self) -> Vec<(Protocol, Vec<Option<String>>)> {
        let plays = |lane: &Lane, protocol: &Protocol, role: usize| plays_role(&lane.net, protocol, role) || plays_role(&lane.label, protocol, role);
        let mut out = Vec::new();
        for protocol in Protocol::ALL.into_iter().filter(|p| !matches!(p, Protocol::Uart { .. })) {
            let mut nets: Vec<Option<String>> = Vec::new();
            for (role, input) in protocol.spec().inputs.iter().enumerate() {
                let net = self.lanes.iter().find(|lane| plays(lane, &protocol, role) && !nets.contains(&Some(lane.net.clone()))).map(|lane| lane.net.clone());
                if net.is_none() && !input.optional {
                    break;
                }
                nets.push(net);
            }
            if nets.len() == protocol.spec().inputs.len() {
                out.push((protocol, nets));
            }
        }
        let uart = Protocol::Uart { baud: None };
        out.extend(self.lanes.iter().filter(|lane| plays(lane, &uart, 0)).map(|lane| (uart, vec![Some(lane.net.clone())])));
        out.retain(|(protocol, nets)| !self.decoders.iter().any(|d| d.protocol.name() == protocol.name() && d.nets == *nets));
        out
    }

    /// Resolves the decoders to the record's signals (their nets' lanes) and hands them and the
    /// newest record to the worker when either changed; takes the worker's newest rows. O(decoders
    /// x roles x lanes).
    fn sync_decoders(&mut self, store: Option<&Arc<TraceStore>>) {
        let Some(worker) = &self.decode_worker else { return };
        let published = worker.out.published();
        if self.decoded.as_ref().is_none_or(|(seen, _)| *seen != published) {
            self.decoded = worker.out.latest();
        }
        let Some(store) = store else { return };
        let lanes = &self.lanes;
        let signal = |net: &Option<String>| {
            let lane = lanes.iter().find(|lane| Some(&lane.net) == net.as_ref())?;
            match lane.row {
                Some((_, Some(k))) => u32::try_from(k).ok().map(SignalId),
                _ => None,
            }
        };
        let setups: Vec<Setup> = self.decoders.iter().map(|d| Setup { id: d.id, protocol: d.protocol, lines: d.nets.iter().map(signal).collect() }).collect();
        let key = (Arc::as_ptr(store) as usize, store.len());
        if self.asked_decode.as_ref().is_some_and(|(ptr, len, asked)| (*ptr, *len) == key && **asked == *setups) {
            return;
        }
        let setups: Arc<[Setup]> = setups.into();
        worker.ask(store.clone(), setups.clone());
        self.asked_decode = Some((key.0, key.1, setups));
    }

    /// Per lane, the decoders drawn under it (each under the last of its lanes in display order),
    /// in the order they were added.
    fn decoders_under(&self) -> Vec<Vec<usize>> {
        let mut under = vec![Vec::new(); self.lanes.len()];
        for (d, decoder) in self.decoders.iter().enumerate() {
            if let Some(i) = decoder.nets.iter().flatten().filter_map(|net| self.lane_index(net)).max() {
                under[i].push(d);
            }
        }
        under
    }

    /// A decoder's published rows, when they are of its current setup.
    fn decoded_of(&self, d: usize) -> Option<(Arc<Decoded>, usize)> {
        let decoder = &self.decoders[d];
        let (_, decoded) = self.decoded.as_ref()?;
        let at = decoded.decoders.iter().position(|one| one.setup.id == decoder.id && one.setup.protocol == decoder.protocol)?;
        Some((decoded.clone(), at))
    }

    /// The rows a decoder draws: its published ones, else those its roles would fill.
    fn decoder_row_ids(&self, d: usize) -> Vec<u8> {
        match self.decoded_of(d) {
            Some((decoded, at)) => decoded.decoders[at].rows.iter().map(|row| row.row).collect(),
            None => {
                let decoder = &self.decoders[d];
                let lines: Vec<Option<SignalId>> = decoder.nets.iter().map(|net| net.as_ref().map(|_| SignalId(0))).collect();
                decoder.protocol.rows(&lines)
            }
        }
    }

    /// A lane's height with the decoders under it: their title and annotation rows.
    /// Lane `i`'s height: the one chosen in its menu, else its kind's.
    fn lane_size(&self, i: usize) -> LaneSize {
        let lane = &self.lanes[i];
        self.sizes.get(&lane.net).copied().unwrap_or_else(|| LaneSize::of_kind(lane.kind(&self.overrides)))
    }

    fn block_h(&self, i: usize, under: &[Vec<usize>]) -> f64 {
        let base = self.lane_size(i).base();
        base + under[i].iter().map(|&d| ROW_H * (1 + self.decoder_row_ids(d).len()) as f64).sum::<f64>()
    }

    /// A decoder's title row (its title and chips, or why it has nothing yet) and its annotation
    /// rows from `top`, hatched where the record runs past what it decoded (up to `record_end`);
    /// returns their height. Per row O(chunks and boxes in the window).
    fn draw_decoder(&mut self, cx: &mut Cx2d, look: &InstrumentLook, d: usize, top: f64, hue: Vec4f, record_end: Option<f64>) -> f64 {
        let (left, name_w) = (self.placed.names.pos.x, self.placed.names.size.x);
        let decoder = self.decoders[d].clone();
        let published = self.decoded_of(d);
        let one = published.as_ref().map(|(decoded, at)| &decoded.decoders[*at]);
        let found = one.and_then(|one| one.found);
        let ids = self.decoder_row_ids(d);
        let height = ROW_H * (1 + ids.len()) as f64;
        let plot = self.placed.plot;
        self.quad(cx, Rect { pos: dvec2(left + 2.0, top + 2.0), size: dvec2(3.0, height - 4.0) }, with_alpha(hue, 0.5));
        let chips: &[(DecChip, &str)] = match decoder.protocol {
            Protocol::Uart { .. } => &[(DecChip::Option, "B"), (DecChip::Close, "×")],
            Protocol::I2c => &[(DecChip::Close, "×")],
            Protocol::Spi { .. } => &[(DecChip::Option, "N"), (DecChip::Mode, "M"), (DecChip::Close, "×")],
        };
        let chips_x = left + name_w - 8.0 - chips.len() as f64 * CHIP;
        let title = fit_text(&decoder.protocol.title(found), (chips_x - left - 14.0).max(0.0), |s| self.text_width_estimate(s));
        self.draw_text.color = rgba(look.text, 1.0);
        self.draw_text.draw_abs(cx, dvec2(left + 10.0, top + 1.0), &title);
        for (j, &(chip, text)) in chips.iter().enumerate() {
            let rect = Rect { pos: dvec2(chips_x + j as f64 * CHIP, top + 1.0), size: dvec2(CHIP - 2.0, ROW_H - 2.0) };
            self.control_face(cx, look, rect, Some(look.lane_control));
            self.draw_text.color = rgba(look.text_secondary, 1.0);
            self.draw_text.draw_abs(cx, rect.pos + dvec2(4.0, 0.0), text);
            if let Some(hit) = semantics::intersect(rect, self.placed.names) {
                self.placed.button(Control::Decoder(decoder.id, chip), hit, text, false);
            }
        }
        if let Some(note) = one.and_then(|one| one.note.as_deref()) {
            self.draw_text.color = rgba(look.text_tertiary, 1.0);
            self.draw_text.draw_abs(cx, dvec2(plot.pos.x + 8.0, top + 1.0), note);
        }
        let names = decoder.protocol.spec().rows;
        let labeler = decoder.protocol.decoder(found.unwrap_or_default());
        let (t0, t1) = self.placed.window;
        for (r, &id) in ids.iter().enumerate() {
            let y = top + ROW_H * (r + 1) as f64;
            self.draw_text.color = rgba(look.text_tertiary, 1.0);
            self.draw_text.draw_abs(cx, dvec2(left + 18.0, y + 1.0), names.get(usize::from(id)).copied().unwrap_or(""));
            let rect = Rect { pos: dvec2(plot.pos.x, y), size: dvec2(plot.size.x, ROW_H) };
            cx.push_clip_rect(rect);
            if let Some(row) = one.and_then(|one| one.rows.iter().find(|row| row.row == id)) {
                self.draw_pieces(cx, look, row, rect, hue, labeler.as_ref());
            }
            // the record past what was decoded: not decoded yet
            let head = one.map_or(f64::NEG_INFINITY, |one| one.head);
            if let Some(end) = record_end.filter(|end| head < *end && *end > t0 && head < t1) {
                let (x0, x1) = (self.placed.x_of(head.max(t0)), self.placed.x_of(end.min(t1)));
                if x1 - x0 >= 1.5 {
                    self.draw_hatch.color = with_alpha(hue, 0.35);
                    self.draw_hatch.draw_abs(cx, Rect { pos: dvec2(x0, y + 2.0), size: dvec2(x1 - x0, ROW_H - 4.0) });
                }
            }
            cx.pop_clip_rect();
        }
        height
    }

    /// One annotation row's pieces of the window: boxes with their text when it fits, marks with
    /// theirs when the room to the next allows, boxes under two points and dense chunks merged
    /// into runs, errors marked over everything (one per chunk when dense), gaps hatched.
    fn draw_pieces(&mut self, cx: &mut Cx2d, look: &InstrumentLook, row: &super::decoders::Row, rect: Rect, hue: Vec4f, labeler: &dyn fab_elec_bench::decode::Decoder) {
        let (t0, t1) = self.placed.window;
        let per_point = (t1 - t0) / rect.size.x.max(1.0);
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        row.pieces(t0, t1, 2.0 * per_point, &mut pieces);
        let (y0, h) = (rect.pos.y + 2.0, ROW_H - 4.0);
        let (error, warn) = (rgba(look.canvas.violation, 1.0), rgba(look.canvas.warning, 1.0));
        let mut run: Option<(f64, f64)> = None;
        let mut errors: Vec<(f64, f64)> = Vec::new();
        let start = |piece: &Piece| match piece {
            Piece::Box(ann) => ann.t0,
            Piece::Dense(sum) => sum.t0,
        };
        for (n, piece) in pieces.iter().enumerate() {
            let narrow = match *piece {
                Piece::Dense(sum) => {
                    if sum.errors > 0 {
                        errors.push((self.placed.x_of(sum.err0), self.placed.x_of(sum.err1)));
                    }
                    Some((self.placed.x_of(sum.t0), self.placed.x_of(sum.t1)))
                }
                Piece::Box(ann) => {
                    let (x0, x1) = (self.placed.x_of(ann.t0), self.placed.x_of(ann.t1));
                    let bad = ann.flags & ann_flags::ERROR != 0;
                    if bad {
                        errors.push((x0, x1));
                    }
                    (ann.t0 != ann.t1 && ann.flags & ann_flags::GAP == 0 && x1 - x0 < 2.0).then_some((x0, x1))
                }
            };
            if let Some((x0, x1)) = narrow {
                run = Some(run.map_or((x0, x1), |(a, b)| (a.min(x0), b.max(x1))));
                continue;
            }
            if let Some((a, b)) = run.take() {
                self.quad(cx, Rect { pos: dvec2(a, y0), size: dvec2((b - a).max(1.0), h) }, with_alpha(hue, 0.45));
            }
            let Piece::Box(ann) = *piece else { continue };
            let (x0, x1) = (self.placed.x_of(ann.t0), self.placed.x_of(ann.t1));
            let color = if ann.flags & ann_flags::ERROR != 0 { error } else if ann.flags & ann_flags::WARN != 0 { warn } else { hue };
            if ann.flags & ann_flags::GAP != 0 {
                self.draw_hatch.color = rgba(look.text_tertiary, 0.6);
                self.draw_hatch.draw_abs(cx, Rect { pos: dvec2(x0, y0), size: dvec2((x1 - x0).max(1.5), h) });
                continue;
            }
            let label = |room: f64, this: &Self| {
                let text = labeler.label(&ann);
                (this.text_width_estimate(&text) <= room).then_some(text)
            };
            if ann.t0 == ann.t1 {
                self.quad(cx, Rect { pos: dvec2(x0 - 0.75, y0), size: dvec2(1.5, h) }, color);
                let room = pieces.get(n + 1).map_or(rect.pos.x + rect.size.x, |next| self.placed.x_of(start(next))) - x0 - 5.0;
                if let Some(text) = label(room, self) {
                    self.draw_code.color = color;
                    self.draw_code.draw_abs(cx, dvec2(x0 + 2.5, y0 - 1.0), &text);
                }
                continue;
            }
            self.quad(cx, Rect { pos: dvec2(x0, y0), size: dvec2(x1 - x0, h) }, with_alpha(color, 0.22));
            self.quad(cx, Rect { pos: dvec2(x0, y0), size: dvec2(1.0, h) }, color);
            if let Some(text) = label(x1 - x0 - 6.0, self) {
                self.draw_code.color = rgba(look.text, 1.0);
                self.draw_code.draw_abs(cx, dvec2(x0 + 3.0, y0 - 1.0), &text);
            }
        }
        if let Some((a, b)) = run {
            self.quad(cx, Rect { pos: dvec2(a, y0), size: dvec2((b - a).max(1.0), h) }, with_alpha(hue, 0.45));
        }
        for (a, b) in errors {
            self.quad(cx, Rect { pos: dvec2(a - 0.75, y0), size: dvec2((b - a).max(1.5), h) }, error);
        }
        self.pieces = pieces;
    }

    /// The lanes' rows, asked of the bench to keep whole over the run when they change, so a
    /// lane shows any instant of the take at the run's own steps (the rest keep the live front).
    /// O(1) with unchanged lanes and numbering.
    fn sync_probes(&mut self, cx: &mut Cx) {
        let rebuilt = self.sync_lanes(cx);
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return };
        if live.opened.is_some() {
            // a saved trace's rows are its own: the run keeps what the Scope last asked of it
            if rebuilt {
                self.area.redraw(cx);
            }
            return;
        }
        if !rebuilt && self.lanes.first().is_none_or(|lane| lane.row.is_some_and(|(g, _)| g == live.sim.index.generation)) {
            return;
        }
        self.sync_rows(cx);
        let mut rows: Vec<usize> = self.lanes.iter().filter_map(|lane| lane.row.and_then(|(_, row)| row)).collect();
        rows.sort_unstable();
        rows.dedup();
        if rows != self.asked_probes {
            self.asked_probes = rows.clone();
            cx.action(SimAction::Probes { id: self.ask_id(SCOPE_RECORD), rows });
        }
        let mut nets: Vec<NetRef> = self.lanes.iter().filter_map(|lane| lane.net_ref).collect();
        nets.sort_unstable();
        nets.dedup();
        if nets != self.asked_envelopes {
            for &net in self.asked_envelopes.iter().filter(|net| nets.binary_search(net).is_err()) {
                cx.action(SimAction::Envelope { id: self.envelope_id(net), net: None });
            }
            for &net in nets.iter().filter(|net| self.asked_envelopes.binary_search(net).is_err()) {
                cx.action(SimAction::Envelope { id: self.envelope_id(net), net: Some(net) });
            }
            self.asked_envelopes = nets;
        }
        if rebuilt {
            self.area.redraw(cx);
        }
    }

    /// The spilled logic chunks the drawn lanes show in detail, asked of the bench when they
    /// change: it loads them back, and the next record draws them exactly. O(chunks asked).
    fn sync_detail(&mut self, cx: &mut Cx) {
        if self.detail != self.asked_detail {
            self.asked_detail = self.detail.clone();
            cx.action(SimAction::LogicDetail { id: self.ask_id(SCOPE_DETAIL), chunks: self.detail.clone() });
        }
    }

    /// The Scope's copy of the app's tap (`listen::ElecAudio::tap`): its nets, mix and meters,
    /// taken again when another tap goes in or the last comes out. O(1) when it has not.
    fn follow_listen(&mut self, cx: &mut Cx) {
        let Some(audio) = cx.get_global_ref::<ElecAudio>() else { return };
        let (nets, mix, generation) = audio.tap();
        if generation == self.tap_gen {
            return;
        }
        (self.tap_gen, self.tap_nets, self.mix) = (generation, nets.to_vec(), mix.cloned());
        self.meters = vec![(0.0, 0.0); self.tap_nets.len()];
        self.metering = false;
        self.redraw_soon(cx);
    }

    /// Sets a monitored lane's level (dB, clamped), live in the running tap.
    fn set_level(&mut self, cx: &mut Cx, lane: usize, db: f32) {
        let Some(net) = self.lanes.get(lane).map(|l| l.net.clone()) else { return };
        set_level_db(cx, &net, db.clamp(LEVEL_MIN_DB, LEVEL_MAX_DB));
        self.redraw_soon(cx);
    }

    fn set_master(&mut self, cx: &mut Cx, db: f32) {
        set_master_db(cx, db.clamp(LEVEL_MIN_DB, LEVEL_MAX_DB));
        self.redraw_soon(cx);
    }

    /// The level of the lane whose meter is at `x`.
    fn level_at(&self, lane: usize, x: f64) -> Option<f32> {
        let bar = self.placed.buttons.iter().find(|b| b.control == Control::Level(lane))?.rect;
        let f = ((x - bar.pos.x) / bar.size.x.max(1.0)).clamp(0.0, 1.0) as f32;
        Some(LEVEL_MIN_DB + f * (LEVEL_MAX_DB - LEVEL_MIN_DB))
    }

    fn begin_rename(&mut self, cx: &mut Cx, lane: usize) {
        let Some(rect) = self.placed.rows.iter().find(|(i, _)| *i == lane).map(|(_, r)| *r) else { return };
        let names = self.placed.names;
        let field = Rect { pos: dvec2(names.pos.x + 8.0, rect.pos.y + 3.0), size: dvec2((names.size.x - 16.0).max(1.0), CHIP) };
        self.reveal = Some(self.lanes[lane].net.clone());
        self.name_field.set_text(cx, &self.lanes[lane].label);
        self.name_field.select_all(cx);
        self.rename = Some((self.lanes[lane].net.clone(), field));
        self.rename_focus = true;
        self.redraw_soon(cx);
    }

    fn end_rename(&mut self, cx: &mut Cx, commit: Option<String>) {
        let Some((net, _)) = self.rename.take() else { return };
        self.rename_focus = false;
        if cx.has_key_focus(self.name_field.area()) {
            cx.set_key_focus(self.area);
        }
        let (Some(text), Some(bench), Some(lane)) = (commit, self.bench.clone(), self.lanes.iter().find(|lane| !lane.selection && lane.net == net)) else {
            self.redraw_soon(cx);
            return;
        };
        // A flag placed by its old name stays where it is under the new one.
        let keep = cx.get_global_ref::<ElecLive>().and_then(|live| {
            let probes = &live.snap.benches.iter().find(|out| out.out.name == bench)?.out.probes;
            rename_keeps(&live.snap.design, probes.iter().find(|probe| probe.of == lane.of)?, &text)
        });
        if let Some(action) = rename_probe_op(&bench, &lane.of, &lane.label, &text, keep.as_deref()) {
            cx.widget_action(self.uid, action);
        }
        self.redraw_soon(cx);
    }

    fn press(&mut self, cx: &mut Cx, abs: DVec2, shift: bool, alt: bool, taps: u32) {
        let p = &self.placed;
        if let Some(control) = p.hit(abs) {
            if let Control::Lane(i, _) | Control::Level(i) = control {
                self.focused_lane = Some(self.lanes[i].net.clone());
            }
            if let Control::Level(lane) = control {
                self.press = Some(Press::Level(lane));
                if let Some(db) = self.level_at(lane, abs.x) {
                    self.set_level(cx, lane, db);
                }
            } else {
                self.press = Some(Press::Button(control));
            }
            return;
        }
        if self.menu.is_some() {
            if p.menu.is_none_or(|rect| !rect.contains(abs)) {
                self.menu = None;
                self.placed.buttons.clear();
                self.redraw_soon(cx);
            }
            return;
        }
        if let Some(lane) = self.lane_at_y(abs.y).filter(|_| p.names.contains(abs) || p.plot.contains(abs)) {
            self.focused_lane = Some(self.lanes[lane].net.clone());
            self.area.redraw(cx);
        }
        if p.names.contains(abs) {
            // picking a decoder's lanes: the lane clicked takes the next role
            if self.picking.is_some() {
                if let Some(lane) = self.lane_at_y(abs.y) {
                    let net = self.lanes[lane].net.clone();
                    self.pick(cx, Some(net));
                }
                return;
            }
            if let Some(lane) = self.lane_at_y(abs.y).filter(|&i| !self.lanes[i].selection) {
                if taps >= 2 && !self.lanes[lane].replay() {
                    self.press = None;
                    self.begin_rename(cx, lane);
                    return;
                }
                self.press = Some(Press::Name { net: self.lanes[lane].net.clone(), start: abs, moved: false });
            }
            return;
        }
        if p.plot.contains(abs) || p.ruler.contains(abs) {
            let on_ruler = p.ruler.contains(abs);
            if let Some(which) = self.cursor_near(abs.x) {
                self.press = Some(Press::Cursor(which));
                return;
            }
            // the take (one drag model, V2.5): a double-click fits; a drag on the traces pans and a
            // drag on the ruler moves the play head, heard; a click on either puts the play head
            // there; Alt or Shift on the traces places a cursor (Alt with a drag pans)
            if let (Some(w), Some(_)) = (self.take.window, self.take_end(cx)) {
                if taps >= 2 {
                    self.take.fit();
                    self.redraw_soon(cx);
                    return;
                }
                if on_ruler {
                    self.press = Some(Press::Scrub);
                    self.scrub_at(cx, abs.x);
                    return;
                }
                if !alt && !shift {
                    self.press = Some(Press::Pan { start: abs, from: w, moved: false });
                    cx.set_cursor(MouseCursor::Grab);
                    return;
                }
                self.press = Some(Press::Plot { start: abs, lag: w.t0, moved: false, shift });
                return;
            }
            let lag = self.timebase.map_or(0.0, |tb| tb.lag);
            self.press = Some(Press::Plot { start: abs, lag, moved: false, shift });
        }
    }

    fn drag(&mut self, cx: &mut Cx, abs: DVec2) {
        match self.press.clone() {
            Some(Press::Name { net, start, moved }) => {
                if moved || (abs - start).length() > 4.0 {
                    self.press = Some(Press::Name { net, start, moved: true });
                    self.insert_at = Some(self.insertion(abs.y));
                    self.drag_at = Some(abs);
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.redraw_soon(cx);
                }
            }
            Some(Press::Plot { start, lag, moved, shift: true }) => {
                // a shift-drag measures: A where it began, B under the pointer, on the lane it
                // began on (user: "Cursors = shift-drag measure")
                if moved || (abs - start).length() > 3.0 {
                    self.press = Some(Press::Plot { start, lag, moved: true, shift: true });
                    if !moved {
                        self.cursors.a = Some(self.snapped(start.x));
                        self.measure_lane = self.placed.rows.iter().find(|(_, block)| block.contains(start))
                            .and_then(|&(i, _)| self.lanes.get(i)).map(|lane| lane.net.clone());
                    }
                    self.cursors.b = Some(self.snapped(abs.x));
                    self.redraw_soon(cx);
                }
            }
            Some(Press::Plot { start, lag, moved, shift }) => {
                if moved || (abs - start).length() > 3.0 {
                    self.press = Some(Press::Plot { start, lag, moved: true, shift });
                    let (t0, t1) = self.placed.window;
                    let per_px = (t1 - t0) / self.placed.plot.size.x.max(1.0);
                    if let Some(w) = self.take.window.filter(|_| self.take_end(cx).is_some()) {
                        // an Alt-drag on the take pans it, from where the press found it
                        let at_press = TakeWindow { t0: lag, t1: lag + w.span() };
                        self.take.drag(at_press, abs.x - start.x, self.placed.plot.size.x);
                    } else if let Some(tb) = &mut self.timebase {
                        tb.lag = lag + (abs.x - start.x) * per_px;
                    }
                    self.trigger_key = None;
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.redraw_soon(cx);
                }
            }
            Some(Press::Pan { start, from, moved }) => {
                if moved || (abs.x - start.x).abs() > 3.0 {
                    self.press = Some(Press::Pan { start, from, moved: true });
                    // the grabbed instant stays under the pointer
                    self.take.drag(from, abs.x - start.x, self.placed.plot.size.x);
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.redraw_soon(cx);
                }
            }
            Some(Press::Scrub) => {
                self.scrub_at(cx, abs.x);
                cx.set_cursor(MouseCursor::ColResize);
            }
            Some(Press::Cursor(which)) => {
                let t = self.snapped(abs.x);
                self.set_cursor(which, t);
                cx.set_cursor(MouseCursor::ColResize);
                self.redraw_soon(cx);
            }
            Some(Press::Level(lane)) => {
                if let Some(db) = self.level_at(lane, abs.x) {
                    self.set_level(cx, lane, db);
                }
                cx.set_cursor(MouseCursor::ColResize);
            }
            _ => {}
        }
    }

    fn release(&mut self, cx: &mut Cx, abs: DVec2, over: bool) {
        let Some(press) = self.press.take() else { return };
        match press {
            Press::Button(control) => {
                if over && self.placed.hit(abs) == Some(control) {
                    self.activate(cx, control);
                }
            }
            Press::Name { net, moved, .. } => {
                if moved {
                    if let Some(lane) = self.lanes.iter().position(|lane| !lane.selection && lane.net == net) {
                        self.commit_reorder(lane, self.insertion(abs.y));
                    }
                }
                self.insert_at = None;
                self.drag_at = None;
                self.redraw_soon(cx);
            }
            Press::Plot { moved, shift, .. } => {
                if !moved && over && self.placed.plot.size.x > 0.0 {
                    let t = self.snapped(abs.x);
                    self.set_cursor(cursor_for_click(&self.cursors, t, shift), t);
                    self.redraw_soon(cx);
                }
            }
            Press::Pan { moved, .. } => {
                // a click on the traces moves the play head there, heard from there
                if !moved && over {
                    self.scrub_at(cx, abs.x);
                }
                self.end_scrub(cx);
                cx.set_cursor(MouseCursor::Default);
            }
            Press::Scrub => self.end_scrub(cx),
            Press::Cursor(_) | Press::Level(_) => self.redraw_soon(cx),
        }
    }

    fn hover(&mut self, cx: &mut Cx, abs: DVec2) {
        let p = &self.placed;
        let hit = p.hit(abs);
        let cursor = if matches!(hit, Some(Control::Level(_) | Control::Scope(Chip::Master))) {
            MouseCursor::ColResize
        } else if hit.is_some() {
            MouseCursor::Hand
        } else if self.menu.is_some() {
            MouseCursor::Default
        } else if p.names.contains(abs) {
            MouseCursor::Hand
        } else if (p.plot.contains(abs) || p.ruler.contains(abs)) && self.cursor_near(abs.x).is_some() {
            MouseCursor::ColResize
        } else if p.ruler.contains(abs) && self.take.window.is_some() {
            // the take's ruler moves the play head, its traces are grabbed to pan
            MouseCursor::ColResize
        } else if p.plot.contains(abs) && self.take.window.is_some() {
            MouseCursor::Grab
        } else if p.plot.contains(abs) || p.ruler.contains(abs) {
            MouseCursor::Hand
        } else {
            MouseCursor::Default
        };
        cx.set_cursor(cursor);
        let t = (self.menu.is_none() && hit.is_none() && (p.plot.contains(abs) || p.ruler.contains(abs))).then(|| p.t_of(abs.x));
        let previous = self.pointer.and_then(|point| p.hit(point));
        self.pointer = Some(abs);
        if t != self.hover_t || previous != hit {
            self.hover_t = t;
            self.redraw_soon(cx);
        }
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f) {
        if rect.size.x > 0.0 && rect.size.y > 0.0 {
            self.draw_quad.color = color;
            self.draw_quad.draw_abs(cx, rect);
        }
    }

    fn text_width_code(&mut self, cx: &mut Cx2d, text: &str) -> f64 {
        self.text_width(cx, true, text)
    }

    /// Centres the label inside its control.
    fn draw_centred(&mut self, cx: &mut Cx2d, code: bool, rect: Rect, text: &str, color: Vec4f) {
        let draw = if code { &mut self.draw_code } else { &mut self.draw_text };
        let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
        let size = dvec2(f64::from(laid.size_in_lpxs.width * draw.font_scale), f64::from(laid.size_in_lpxs.height * draw.font_scale));
        let at = rect.pos + dvec2(((rect.size.x - size.x) * 0.5).max(0.0), ((rect.size.y - size.y) * 0.5).max(0.0));
        draw.color = color;
        draw.draw_abs(cx, at, text);
    }

    fn text_width(&mut self, cx: &mut Cx2d, code: bool, text: &str) -> f64 {
        let draw = if code { &mut self.draw_code } else { &mut self.draw_text };
        let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
        f64::from(laid.size_in_lpxs.width * draw.font_scale)
    }

    fn open_menu(&mut self, menu: Menu) {
        self.menu = Some(menu);
        self.menu_scroll = 0.0;
        self.placed.buttons.clear();
        self.hover_t = None;
    }

    fn activate(&mut self, cx: &mut Cx, control: Control) {
        if !matches!(control, Control::Scope(Chip::Master | Chip::Save | Chip::Mode)) {
            if self.menu.take().is_some() { self.placed.buttons.clear(); }
        }
        match control {
            Control::Scope(chip) => self.chip(cx, chip),
            Control::Lane(lane, chip) => self.lane_chip(cx, lane, chip),
            Control::Decoder(id, chip) => self.decoder_chip(cx, id, chip),
            Control::Level(_) => {}
        }
    }

    fn face(&mut self, cx: &mut Cx2d, rect: Rect, color: Vec4f) {
        self.draw_face.color = color;
        self.draw_face.draw_abs(cx, rect);
    }

    /// A row's card on the sheet's large corner: its first `split` points on `left` (square where
    /// it meets the rest), the rest on `right`; `focus` lights the left part, `tint` the whole.
    fn card(&mut self, cx: &mut Cx2d, card: Rect, split: f64, left: Vec4f, right: Vec4f, focus: Option<Vec4f>, tint: Option<Vec4f>) {
        let r = self.draw_face.radius;
        self.draw_face.radius = self.radius_lg as f32;
        self.face(cx, card, right);
        cx.push_clip_rect(Rect { pos: card.pos, size: dvec2(split.clamp(0.0, card.size.x), card.size.y) });
        self.face(cx, card, left);
        if let Some(color) = focus { self.face(cx, card, color); }
        cx.pop_clip_rect();
        if let Some(color) = tint { self.face(cx, card, color); }
        self.draw_face.radius = r;
    }

    /// Draws a control face under its label. The menu owns the pointer while open.
    fn control_face(&mut self, cx: &mut Cx2d, look: &InstrumentLook, rect: Rect, fill: Option<[f32; 4]>) {
        let hover = self.menu.is_none() && self.pointer.is_some_and(|p| rect.contains(p));
        let face = if hover {
            Some(fill.map_or(look.hover_face, |base| mix_rgba(base, look.hover_face, 0.5)))
        } else { fill };
        if let Some(color) = face { self.face(cx, rect, rgba(color, 1.0)); }
    }

    /// The ruler's tools at its right end: the Scope menu and, in the take, Fit (a double-click on
    /// the ruler fits too). Its left cell is the area's corner chip's; zoom is the wheel or a pinch.
    fn draw_ruler_tools(&mut self, cx: &mut Cx2d, look: &InstrumentLook, ruler: Rect, take: bool) {
        let y = ruler.pos.y + (RULER_H - LANE_GAP - CHIP) * 0.5;
        let more = Rect { pos: dvec2(ruler.pos.x + ruler.size.x - LANE_GAP - 3.0 - CHIP, y), size: dvec2(CHIP, CHIP) };
        self.control_face(cx, look, more, None);
        self.placed.button(Control::Scope(Chip::More), more, "Scope menu", false);
        self.draw_centred(cx, false, more, "⋯", rgba(look.text_secondary, 1.0));
        if !take { return }
        let fit = Rect { pos: more.pos - dvec2(CHIP + 2.0, 0.0), size: more.size };
        self.control_face(cx, look, fit, None);
        self.placed.button(Control::Scope(Chip::Fit), fit, "Fit the take", false);
        // the fit glyph: four corner brackets
        let ink = rgba(look.text_secondary, 1.0);
        let (a, b, t, l) = (fit.pos + dvec2(4.5, 4.5), fit.pos + fit.size - dvec2(4.5, 4.5), 1.25, 3.5);
        for (x, y, right, down) in [(a.x, a.y, true, true), (b.x, a.y, false, true), (a.x, b.y, true, false), (b.x, b.y, false, false)] {
            let (x0, y0) = (if right { x } else { x - l }, if down { y } else { y - t });
            self.quad(cx, Rect { pos: dvec2(x0, y0), size: dvec2(l, t) }, ink);
            let (x0, y0) = (if right { x } else { x - t }, if down { y } else { y - l });
            self.quad(cx, Rect { pos: dvec2(x0, y0), size: dvec2(t, l) }, ink);
        }
    }

    /// The saved trace and the way back to the live run, at the start of the ruler.
    /// A decoder pick shows its prompt here while it lasts.
    fn draw_trace_banner(&mut self, cx: &mut Cx2d, look: &InstrumentLook, ruler: Rect) {
        if self.picking.is_some() { return }
        let Some(opened) = cx.get_global_ref::<ElecLive>().and_then(|live| live.opened.as_ref()) else { return };
        let chips = [(Some(Chip::Live), "Back to live".to_owned()), (None, format!("Saved trace {}", opened.path))];
        let mut x = ruler.pos.x;
        cx.push_clip_rect(ruler);
        for (chip, label) in chips {
            let w = self.text_width(cx, false, &label) + 12.0;
            let r = Rect { pos: dvec2(x, ruler.pos.y + 1.0), size: dvec2(w, MENU_ROW) };
            self.face(cx, r, rgba(look.panel, 1.0));
            if let Some(chip) = chip {
                self.control_face(cx, look, r, None);
                if let Some(hit) = semantics::intersect(r, ruler) {
                    self.placed.button(Control::Scope(chip), hit, label.clone(), false);
                }
            }
            self.draw_centred(cx, false, r, &label, rgba(look.text, 1.0));
            x += w;
        }
        cx.pop_clip_rect();
    }

    /// A manual decoder pick leaves the lane names available to click.
    fn draw_pick_prompt(&mut self, cx: &mut Cx2d, look: &InstrumentLook, ruler: Rect) {
        let Some(pick) = &self.picking else { return };
        let spec = pick.protocol.spec();
        let role = &spec.inputs[pick.nets.len()];
        let prompt = format!("{} · click {}", spec.name, role.role.to_uppercase());
        let skip = role.optional.then(|| format!("no {}", role.role.to_uppercase()));
        let mut x = ruler.pos.x;
        cx.push_clip_rect(ruler);
        for (chip, label) in [(Some(Chip::Cancel), Some("Cancel".to_owned())), (Some(Chip::Skip), skip), (None, Some(prompt))] {
            let Some(label) = label else { continue };
            let w = self.text_width(cx, false, &label) + 12.0;
            let r = Rect { pos: dvec2(x, ruler.pos.y + 1.0), size: dvec2(w, MENU_ROW) };
            self.face(cx, r, rgba(look.panel, 1.0));
            if let Some(chip) = chip {
                self.control_face(cx, look, r, None);
                if let Some(hit) = semantics::intersect(r, ruler) {
                    self.placed.button(Control::Scope(chip), hit, label.clone(), false);
                }
            }
            self.draw_centred(cx, false, r, &label, rgba(look.text, 1.0));
            x += w;
        }
        cx.pop_clip_rect();
    }

    /// The visible menu replaces the control hit list until it closes.
    fn draw_menu(&mut self, cx: &mut Cx2d, look: &InstrumentLook, view: Rect, status: &str, take: bool) {
        self.placed.menu = None;
        let Some(menu) = self.menu.clone() else { return };
        let anchor = match &menu {
            Menu::Scope | Menu::Decode => Some(Control::Scope(Chip::More)),
            Menu::Lane(net) => self.lane_index(net).map(|i| Control::Lane(i, LaneChip::More)),
            Menu::Kind(net) => self.lane_index(net).map(|i| Control::Lane(i, if self.lanes[i].selection { LaneChip::More } else { LaneChip::Kind })),
        };
        let Some(anchor) = anchor.and_then(|c| self.placed.buttons.iter().find(|b| b.control == c).map(|b| b.rect)) else {
            self.menu = None;
            return;
        };
        let can_save = matches!(menu, Menu::Scope) && (!self.tap_nets.is_empty() || listen::jack_recorded(cx));
        let measured = self.cursors.a.is_some() || self.cursors.b.is_some();
        let mut rows = Vec::new();
        match menu {
            Menu::Scope => {
                let mut item = |chip: Option<Chip>, text: String, on| rows.push((chip.map(Control::Scope), text, on));
                item(Some(Chip::TakeModel), "Recording view".into(), take);
                item(Some(Chip::ScopeModel), "Trigger sweep".into(), !take);
                let continuous = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.status.continuous);
                if (take && continuous && self.take.held) || (!take && self.timebase.is_some_and(|tb| tb.lag != 0.0)) {
                    item(Some(Chip::Follow), "Follow".into(), false);
                }
                if take && !continuous {
                    item(Some(Chip::FollowHead), "Follow playhead".into(), self.take.follow);
                }
                if !take {
                    if let Some(t) = &self.trigger {
                        let name = self.lanes.iter().find(|l| l.net == t.net).map_or(t.net.as_str(), |l| l.label.as_str());
                        item(None, format!("Trigger: {name} {}", slope_word(t.slope)), false);
                        item(Some(Chip::Mode), format!("Trigger mode: {}", if t.mode == TrigMode::Auto { "auto" } else { "normal" }), false);
                    }
                }
                // Decode only with a bus (user): lanes that play a protocol's roles, or a decoder
                // already open; O(lanes × roles) on the menu's opening
                if self.picking.is_none() && (!self.decoders.is_empty() || !self.offers().is_empty()) {
                    item(Some(Chip::Decode), "Decode…".into(), false);
                }
                // the project's saved traces, the one shown lit, and the way back to the run
                let (traces, shown) = cx.get_global_ref::<ElecLive>()
                    .map(|live| (live.traces.clone(), live.opened.as_ref().map(|opened| opened.path.clone()))).unwrap_or_default();
                if shown.is_some() {
                    item(Some(Chip::Live), "Back to live".into(), false);
                }
                for (i, path) in traces.iter().enumerate() {
                    item(Some(Chip::Open(i)), format!("Open trace {}", trace_name(path)), shown.as_deref() == Some(path.as_str()));
                }
                item(Some(Chip::Save), "Save WAV".into(), false);
                if let Some(text) = listen::save_line(cx) { item(None, text, false); }
                item(Some(Chip::Clear), "Clear".into(), false);
                if !self.tap_nets.is_empty() {
                    item(Some(Chip::Master), format!("Master {}", db_label(master_db(cx))), false);
                    if let Some(text) = listen_status(cx) { item(None, text.to_string(), false); }
                }
                if !status.is_empty() { item(None, status.into(), false); }
            }
            Menu::Decode => {
                self.offers = self.offers();
                for (i, (protocol, nets)) in self.offers.iter().enumerate() {
                    let names: Vec<_> = nets.iter().flatten().map(|net| self.lanes.iter().find(|l| &l.net == net).map_or(net.as_str(), |l| l.label.as_str())).collect();
                    rows.push((Some(Control::Scope(Chip::Offer(i))), format!("{} {}", protocol.name(), names.join(" ")), false));
                }
                for (i, protocol) in Protocol::ALL.iter().enumerate() {
                    rows.push((Some(Control::Scope(Chip::Pick(i as u8))), format!("{}…", protocol.name()), false));
                }
            }
            Menu::Lane(net) | Menu::Kind(net) => {
                let Some(i) = self.lane_index(&net) else { return };
                if matches!(self.menu, Some(Menu::Lane(_))) {
                    if self.lanes[i].selection {
                        rows.push((Some(Control::Lane(i, LaneChip::Kind)), "Show as…".into(), false));
                        rows.push((Some(Control::Lane(i, LaneChip::Close)), "Close".into(), false));
                    } else {
                        let size = self.lane_size(i);
                        for (chip, text) in [
                            (LaneChip::Rename, "Rename"),
                            (LaneChip::Trig, "Trigger on this lane"),
                            (LaneChip::Size(LaneSize::Small), "Height: small"),
                            (LaneChip::Size(LaneSize::Medium), "Height: medium"),
                            (LaneChip::Size(LaneSize::Large), "Height: large"),
                            (LaneChip::Close, "Remove probe"),
                        ] {
                            // a saved trace's lane is no probe of the bench to rename or remove
                            if self.lanes[i].replay() && matches!(chip, LaneChip::Rename | LaneChip::Close) { continue }
                            let on = match chip {
                                LaneChip::Trig => self.trigger.as_ref().is_some_and(|t| t.net == net),
                                LaneChip::Size(s) => s == size,
                                _ => false,
                            };
                            rows.push((Some(Control::Lane(i, chip)), text.into(), on));
                        }
                    }
                } else {
                    for (kind, text) in [(None, "auto"), (Some(LaneKind::Analog), "analog"), (Some(LaneKind::Digital), "digital")] {
                        rows.push((Some(Control::Lane(i, LaneChip::SetKind(kind))), text.into(), self.overrides.get(&net).copied() == kind));
                    }
                }
            }
        }
        let width = rows.iter().map(|(_, text, _)| self.text_width(cx, false, text) + 24.0).fold(80.0, f64::max).min(view.size.x);
        let content_h = MENU_ROW * rows.len() as f64;
        let height = (content_h + 4.0).min(view.size.y);
        let right = view.pos.x + view.size.x;
        let bottom = view.pos.y + view.size.y;
        let below = anchor.pos.y + anchor.size.y;
        let y = if below + height <= bottom { below } else { (anchor.pos.y - height).max(view.pos.y).min(bottom - height) };
        let popup = Rect { pos: dvec2(anchor.pos.x.min(right - width).max(view.pos.x), y), size: dvec2(width, height) };
        self.placed.menu = Some(popup);
        self.placed.buttons.clear();
        self.menu_scroll = self.menu_scroll.clamp(0.0, (content_h + 4.0 - height).max(0.0));
        self.draw_face.new_draw_call(cx);
        self.draw_face.radius = self.radius_lg as f32;
        self.face(cx, popup, rgba(look.popover, 1.0));
        self.draw_face.radius = self.radius as f32;
        self.draw_text.new_draw_call(cx);
        let clip = popup.add_margin(dvec2(-2.0, -2.0));
        cx.push_clip_rect(clip);
        for (i, (control, text, on)) in rows.into_iter().enumerate() {
            let rect = Rect { pos: clip.pos + dvec2(0.0, i as f64 * MENU_ROW - self.menu_scroll), size: dvec2(clip.size.x, MENU_ROW) };
            let Some(hit) = semantics::intersect(rect, clip) else { continue };
            let enabled = match control {
                Some(Control::Scope(Chip::TakeModel)) => self.take.seen,
                Some(Control::Scope(Chip::Save)) => can_save,
                Some(Control::Scope(Chip::Clear)) => measured,
                _ => true,
            };
            if enabled && control.is_some() && self.pointer.is_some_and(|p| hit.contains(p)) {
                self.face(cx, rect, rgba(look.hover_face, 1.0));
            } else if on {
                self.face(cx, rect, rgba(look.lane_focus, 1.0));
            }
            self.draw_text.color = rgba(if !enabled { look.text_tertiary } else if control.is_some() { look.text } else { look.text_secondary }, 1.0);
            self.draw_text.draw_abs(cx, rect.pos + dvec2(6.0, 3.0), &text);
            if let Some(control) = control {
                self.placed.buttons.push(Button { control, rect: hit, label: text.into(), selected: on, enabled });
            }
        }
        cx.pop_clip_rect();
    }

    fn draw_ruler(&mut self, cx: &mut Cx2d, look: &InstrumentLook, ruler: Rect, plot_bottom: f64, take: bool) {
        let (t0, t1) = self.placed.window;
        let dpi = self.placed.dpi;
        let snap = |v: f64| (v * dpi).round() / dpi;
        if take {
            let draw = &mut self.draw_code;
            let mut measure = |label: &str| {
                let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), label);
                f64::from(laid.size_in_lpxs.width * draw.font_scale)
            };
            let ticks = ruler_ticks(TakeWindow { t0, t1 }, ruler.size.x, 24.0, &mut measure);
            for tick in &ticks {
                let x = snap(ruler.pos.x + tick.at);
                let labelled = !tick.label.is_empty();
                let mark = if labelled { 7.0 } else { 3.0 };
                self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y + ruler.size.y - mark), size: dvec2(1.0 / dpi, mark) }, rgba(look.zero_line, 1.0));
                if labelled {
                    self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y + ruler.size.y), size: dvec2(1.0 / dpi, (plot_bottom - ruler.pos.y - ruler.size.y).max(0.0)) }, rgba(look.hairline, 1.0));
                    self.draw_code.color = rgba(look.text_tertiary, 1.0);
                    self.draw_code.draw_abs(cx, dvec2(x + 3.0, ruler.pos.y + 3.0), &tick.label);
                }
            }
            return;
        }
        let step = (t1 - t0) / DIVS;
        let top = t0.abs().max(t1.abs());
        let sig = if step > 0.0 && top > 0.0 { ((top.log10().floor() - step.log10().floor()) as i64 + 2).clamp(2, 9) as usize } else { 3 };
        let label_of = |i: usize| {
            let t = t0 + step * i as f64;
            if t.abs() < step * 1e-6 { "0 s".to_owned() } else { eng(t, "s", sig) }
        };
        // every division's label when they fit, else every 2nd, 5th or 10th: never overlapping
        let widest = (0..DIVS as usize).map(|i| self.text_width_code(cx, &label_of(i))).fold(0.0, f64::max);
        let div_px = ruler.size.x / DIVS;
        let every = [1usize, 2, 5, 10].into_iter().find(|&k| k as f64 * div_px >= widest + 12.0).unwrap_or(10);
        for i in 0..=DIVS as usize {
            let t = t0 + step * i as f64;
            let x = snap(self.placed.x_of(t));
            let major = rgba(look.zero_line, 1.0);
            self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y + ruler.size.y - 6.0), size: dvec2(1.0 / dpi, 6.0) }, major);
            self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y + ruler.size.y), size: dvec2(1.0 / dpi, (plot_bottom - ruler.pos.y - ruler.size.y).max(0.0)) }, rgba(look.hairline, 1.0));
            if i < DIVS as usize && i % every == 0 {
                self.draw_code.color = rgba(look.text_tertiary, 1.0);
                self.draw_code.draw_abs(cx, dvec2(x + 3.0, ruler.pos.y + 3.0), &label_of(i));
            }
        }
        if let Some(t) = self.shown_trigger().filter(|t| (t0..t1).contains(t)) {
            let x = snap(self.placed.x_of(t));
            let hue = self.trigger.as_ref().and_then(|tr| self.lanes.iter().find(|l| l.net == tr.net)).map_or(look.text, |l| look.channel(l.hue));
            let mark = self.trigger.as_ref().map_or("T", |tr| slope_mark(tr.slope));
            let flag = Rect { pos: dvec2(x - 8.0, ruler.pos.y + 2.0), size: dvec2(16.0, ruler.size.y - 4.0) };
            self.quad(cx, flag, rgba(hue, 1.0));
            self.draw_text.color = rgba(look.canvas.canvas, 1.0);
            self.draw_text.draw_abs(cx, flag.pos + dvec2(4.0, 2.0), mark);
            // The trigger instant down every lane, faint.
            self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y + ruler.size.y), size: dvec2(1.0 / dpi, (plot_bottom - ruler.pos.y - ruler.size.y).max(0.0)) }, rgba(hue, 0.45));
        }
    }

    /// Hatch the unrecorded interval once across the visible lanes.
    /// The stretch of the take not yet recorded, hatched over each card from the front on, in
    /// the card's shape.
    fn draw_coverage(&mut self, cx: &mut Cx2d, look: &InstrumentLook, view: Rect, ruler: Rect, plot_bottom: f64, front: f64) {
        let (t0, t1) = self.placed.window;
        let xf = self.placed.x_of(front.clamp(t0, t1));
        let right = ruler.pos.x + ruler.size.x - LANE_GAP;
        if right <= xf || plot_bottom <= ruler.pos.y { return }
        self.draw_hatch.color = rgba(look.canvas.selection, 0.4);
        self.draw_hatch.radius = self.radius_lg as f32;
        cx.push_clip_rect(Rect { pos: dvec2(xf, ruler.pos.y), size: dvec2(right - xf, plot_bottom - ruler.pos.y) });
        self.draw_hatch.draw_abs(cx, ruler_card(view));
        for k in 0..self.placed.rows.len() {
            let card = lane_card(view, self.placed.rows[k].1);
            self.draw_hatch.draw_abs(cx, card);
        }
        cx.pop_clip_rect();
        self.draw_hatch.radius = 0.0;
    }

    fn draw_digital(&mut self, cx: &mut Cx2d, lane: usize, row: Rect, hue: Vec4f, slots: &[EnvSlot]) {
        let dpi = self.placed.dpi;
        let snap = |v: f64| (v * dpi).round() / dpi;
        let plot = self.placed.plot;
        let cols = f64::from(self.lanes[lane].cols.cols.max(1));
        let x_of = |x: f32| snap(plot.pos.x + f64::from(x) * plot.size.x / cols);
        let (hi_y, lo_y) = (snap(row.pos.y + 7.0), snap(row.pos.y + row.size.y - 7.0));
        let line = 1.5;
        let runs = std::mem::take(&mut self.lanes[lane].runs);
        let mut fill = hue;
        fill.w *= 0.13;
        let mut previous: Option<Run> = None;
        for run in &runs {
            let (x0, x1) = (x_of(run.x0), x_of(run.x1));
            let w = (x1 - x0).max(0.0);
            match run.level {
                Level::High => {
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, lo_y - hi_y) }, fill);
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, line) }, hue);
                }
                Level::Low => self.quad(cx, Rect { pos: dvec2(x0, lo_y - line), size: dvec2(w, line) }, hue),
                Level::Between => {
                    if w >= 1.5 {
                        self.draw_hatch.color = hue;
                        self.draw_hatch.draw_abs(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, lo_y - hi_y) });
                    }
                }
                Level::Busy => {
                    let mut busy = hue;
                    busy.w *= 0.6;
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w.max(1.0), lo_y - hi_y) }, busy);
                }
            }
            if let Some(prev) = previous {
                let narrow = |r: &Run| r.level == Level::Between && x_of(r.x1) - x_of(r.x0) < 1.5;
                if prev.level != run.level && prev.level != Level::Busy && run.level != Level::Busy && (!narrow(run) || !narrow(&prev)) {
                    self.quad(cx, Rect { pos: dvec2(x0 - line * 0.5, hi_y), size: dvec2(line, lo_y - hi_y + line) }, hue);
                }
            }
            previous = Some(*run);
        }
        self.lanes[lane].runs = runs;
        // where the record has no point, the coarse envelope's level: low or high where the whole
        // slot is, busy where it swings across both thresholds, between otherwise
        if !slots.is_empty() && self.lanes[lane].cols_key.is_some() {
            let th = self.lanes[lane].thresholds();
            let gaps = std::mem::take(&mut self.lanes[lane].gaps);
            visit_envelope(slots, &gaps, |slot, start, end| {
                let (x0, x1) = (snap(self.placed.x_of(start)), snap(self.placed.x_of(end)));
                let w = (x1 - x0).max(0.0);
                let (lo, hi) = (f64::from(slot.min), f64::from(slot.max));
                if hi <= th.low {
                    self.quad(cx, Rect { pos: dvec2(x0, lo_y - line), size: dvec2(w, line) }, hue);
                } else if lo >= th.high {
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, lo_y - hi_y) }, fill);
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, line) }, hue);
                } else if lo <= th.low && hi >= th.high {
                    let mut busy = hue;
                    busy.w *= 0.6;
                    self.quad(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w.max(1.0), lo_y - hi_y) }, busy);
                } else if w >= 1.5 {
                    self.draw_hatch.color = hue;
                    self.draw_hatch.draw_abs(cx, Rect { pos: dvec2(x0, hi_y), size: dvec2(w, lo_y - hi_y) });
                }
            });
            self.lanes[lane].gaps = gaps;
        }
    }

    fn selection_envelope(&self, cx: &Cx) -> Option<Arc<Vec<EnvSlot>>> {
        self.lane_envelope(cx, self.lanes.first().filter(|lane| lane.selection).and_then(|lane| lane.net_ref))
    }

    /// The coarse envelope the bench published for `net` at this Scope's ask, if asked.
    fn lane_envelope(&self, cx: &Cx, net: Option<NetRef>) -> Option<Arc<Vec<EnvSlot>>> {
        let net = net.filter(|net| self.asked_envelopes.binary_search(net).is_ok())?;
        let envelopes = &cx.get_global_ref::<ElecLive>()?.sim.envelopes;
        let index = envelopes.binary_search_by_key(&self.envelope_id(net), |e| e.id).ok()?;
        let answer = &envelopes[index];
        (answer.net == net).then(|| answer.slots.clone())
    }

    /// This Scope's ask id for `net`'s envelope: one per net, stable while it is asked.
    fn envelope_id(&self, net: NetRef) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        net.hash(&mut h);
        self.ask_id(SCOPE_ENVELOPE) ^ h.finish().rotate_left(7)
    }

    fn draw_analog(&mut self, cx: &mut Cx2d, lane: usize, row: Rect, hue: Vec4f, take: bool, slots: &[EnvSlot]) {
        let plot = Rect { pos: dvec2(self.placed.plot.pos.x, row.pos.y + 5.0), size: dvec2(self.placed.plot.size.x, row.size.y - 10.0) };
        let l = &self.lanes[lane];
        let (mut lo, mut hi) = l.range;
        let gaps = if l.cols_key.is_some() { l.gaps.as_slice() } else { std::slice::from_ref(&self.placed.window) };
        if !slots.is_empty() {
            if gaps == [self.placed.window] {
                (lo, hi) = (f64::INFINITY, f64::NEG_INFINITY);
            }
            visit_envelope(slots, gaps, |slot, _, _| {
                lo = lo.min(f64::from(slot.min));
                hi = hi.max(f64::from(slot.max));
            });
            if lo > hi { (lo, hi) = l.range; }
        }
        let pad = ((hi - lo) * 0.1).max(0.05);
        let span = (hi + pad) - (lo - pad);
        let y_of = |v: f32| plot.pos.y + plot.size.y * (1.0 - (f64::from(v) - (lo - pad)) / span);
        let px = 1.0 / self.placed.dpi;
        if !slots.is_empty() {
            visit_envelope(slots, gaps, |slot, start, end| {
                let x = self.placed.x_of(start);
                let w = self.placed.x_of(end) - x;
                let top = y_of(slot.max);
                self.draw_quad.color = with_alpha(hue, hue.w * 0.2);
                self.draw_quad.draw_abs(cx, Rect { pos: dvec2(x, top), size: dvec2(w, (y_of(slot.min) - top).max(px)) });
                self.draw_quad.color = hue;
                self.draw_quad.draw_abs(cx, Rect { pos: dvec2(x, y_of(slot.mean) - 0.5 * px), size: dvec2(w, px) });
            });
        }
        // the take as a wave editor draws it: a dense window's columns filled from their least
        // to their greatest value, the line over them; O(columns)
        let cols = std::mem::take(&mut self.lanes[lane].cols);
        if take && cols.density() >= 1.0 && span > 0.0 && cols.cols > 0 {
            let w = plot.size.x / f64::from(cols.cols);
            let mut body = hue;
            body.w *= 0.55;
            for c in cols.cols_data.iter().filter(|c| c.lo.is_finite() && c.hi.is_finite()) {
                let (top, bottom) = (y_of(c.hi), y_of(c.lo));
                let x = plot.pos.x + f64::from(c.x0.floor()) * w;
                self.quad(cx, Rect { pos: dvec2(x, top), size: dvec2(w.max(px), (bottom - top).max(px)) }, body);
            }
        }
        self.lanes[lane].cols = cols;
        let style = TraceStyle { color: hue, width: if take { 1.0 } else { 1.5 }, dot: 3.0 };
        draw_columns(cx, &mut self.draw_seg, &self.lanes[lane].cols, plot, lo - pad, hi + pad, &style);
    }

    /// The value of the lane at the indicated time.
    fn value_text(&self, lane: &Lane, kind: LaneKind, t: f64, slots: &[EnvSlot]) -> String {
        let coarse = || {
            let end = slots.partition_point(|slot| slot.t0 <= t);
            slots[..end].last().filter(|slot| t <= slot.t1 && slot.mean.is_finite())
                .map_or_else(|| "—".into(), |slot| format!("≈ {}", eng(f64::from(slot.mean), "V", 3)))
        };
        let (Some((_, Some(k))), Some(source)) = (lane.row, self.source.as_ref()) else {
            return if lane.selection { coarse() } else { String::new() };
        };
        let store = &source.store;
        let before_end = || !store.is_empty() && t <= store.time.at(store.len() - 1);
        if lane.selection && !before_end() { return coarse() }
        let Some(v) = source.value_at(k, t).map(f64::from).filter(|v| v.is_finite()) else {
            let level = logic_of(store, k).filter(|_| kind == LaneKind::Digital && before_end()).and_then(|trace| trace.level_at(t));
            return level.map_or_else(coarse, |high| if high { "1" } else { "0" }.into());
        };
        match kind {
            LaneKind::Digital => {
                let th = lane.thresholds();
                if v <= th.low { "0".into() } else if v >= th.high { "1".into() } else { format!("X {}", eng(v, "V", 2)) }
            }
            LaneKind::Analog => eng(v, "V", 3),
        }
    }

    /// The probe name and value share the top line. Its controls sit at the foot.
    fn draw_lane_header(&mut self, cx: &mut Cx2d, look: &InstrumentLook, i: usize, header: Rect, hue: [f32; 4], at: Option<f64>, slots: &[EnvSlot]) {
        let lane = &self.lanes[i];
        let selection = lane.selection;
        let kind = lane.kind(&self.overrides);
        let choice = self.overrides.get(&lane.net).copied();
        let audible = matches!((&lane.source.side, &lane.source.of), (Side::Sim, LaneOf::Net(_)));
        let heard = audible && listening(cx, &lane.net);
        let value = at.map_or_else(String::new, |t| self.value_text(lane, kind, t, slots));
        let renaming = self.rename.as_ref().is_some_and(|(net, _)| net == &lane.net);
        if renaming {
            if let Some((_, field)) = &mut self.rename {
                field.pos = header.pos + dvec2(7.0, 2.0);
                field.size.x = (header.size.x - 14.0).max(1.0);
            }
        }
        let left = header.pos.x + 11.0;
        let right = header.pos.x + header.size.x - 7.0;
        // the hue bar: a pill inside the card
        let bar = Rect { pos: header.pos + dvec2(LANE_GAP + 3.0, 5.0), size: dvec2(3.0, (header.size.y - 10.0 - LANE_GAP).max(0.0)) };
        let r = self.draw_face.radius;
        self.draw_face.radius = (bar.size.x * 0.5) as f32;
        self.face(cx, bar, rgba(hue, if selection { 0.5 } else { 1.0 }));
        self.draw_face.radius = r;
        let value_x = right - self.text_width_code(cx, &value);
        if !renaming && value_x - 6.0 > left {
            let clip = Rect { pos: dvec2(left, header.pos.y), size: dvec2(value_x - 6.0 - left, CHIP + 5.0) };
            cx.push_clip_rect(clip);
            self.draw_name.color = rgba(look.text, 1.0);
            let walk = Walk {
                abs_pos: Some(dvec2(left, header.pos.y + 5.0)),
                width: Size::Fixed(clip.size.x),
                height: Size::Fixed(CHIP),
                ..Walk::default()
            };
            self.draw_name.draw_walk(cx, walk, Align::default(), &self.lanes[i].label);
            cx.pop_clip_rect();
        }
        self.draw_code.color = rgba(hue, 1.0);
        self.draw_code.draw_abs(cx, dvec2(value_x, header.pos.y + 5.0), &value);

        // the controls 6 points above the card's foot (v10); a digital lane's, too short for
        // that, right under the name
        let y = (header.pos.y + header.size.y - LANE_GAP - 6.0 - CHIP).max(header.pos.y + 5.0 + CHIP);
        let kind_text = match choice {
            Some(LaneKind::Analog) => "∿ analog",
            Some(LaneKind::Digital) => "⎍ digital",
            None if kind == LaneKind::Digital => "⎍ auto",
            None => "∿ auto",
        };
        let field = Rect { pos: dvec2(left, y), size: dvec2(54.0, CHIP) };
        let speaker = Rect { pos: dvec2(left + 58.0, y), size: dvec2(CHIP, CHIP) };
        let more = Rect { pos: dvec2(right - CHIP, y), size: speaker.size };
        let (primary, text, label) = if selection { (LaneChip::Pin, "Pin", "Pin") } else { (LaneChip::Kind, kind_text, "Show as") };
        for (chip, rect, text, label, fill) in [
            (primary, field, text, label, Some(look.lane_control)),
            (LaneChip::Listen, speaker, "♪", "Listen", heard.then_some(look.accent_face)),
            (LaneChip::More, more, "⋯", "Lane menu", None),
        ] {
            if chip == LaneChip::Listen && !audible { continue }
            self.control_face(cx, look, rect, fill);
            let lit = chip == LaneChip::Listen && heard;
            self.draw_centred(cx, false, rect, text, rgba(if lit { look.text } else { look.text_secondary }, 1.0));
            if let Some(hit) = semantics::intersect(rect, self.placed.names) {
                self.placed.button(Control::Lane(i, chip), hit, label, lit);
            }
        }
        if !audible { return }
        let x = speaker.pos.x + CHIP + 4.0;
        let bar = Rect { pos: dvec2(x, y + 7.0), size: dvec2((more.pos.x - x - 8.0).clamp(0.0, 46.0), 4.0) };
        self.quad(cx, bar, rgba(look.lane_control, 1.0));
        if let Some(v) = self.tap_nets.iter().position(|n| *n == self.lanes[i].net) {
            let to_db = |a: f32| if a > 0.0 { 20.0 * a.log10() } else { LEVEL_MIN_DB };
            let (rms, peak) = self.meters.get(v).copied().unwrap_or_default();
            self.quad(cx, Rect { size: dvec2(bar.size.x * meter_x(to_db(rms)), bar.size.y), ..bar }, rgba(look.canvas.ok, 1.0));
            if peak > 0.0 {
                let x = bar.pos.x + bar.size.x * meter_x(to_db(peak));
                self.quad(cx, Rect { pos: dvec2(x - 0.5, bar.pos.y), size: dvec2(1.0, bar.size.y) }, rgba(hue, 1.0));
            }
            let x = bar.pos.x + bar.size.x * meter_x(level_db(cx, &self.lanes[i].net));
            self.quad(cx, Rect { pos: dvec2(x - 0.5, bar.pos.y - 1.0), size: dvec2(1.0, bar.size.y + 2.0) }, rgba(look.text, 0.9));
            if let Some(hit) = semantics::intersect(bar.add_margin(dvec2(0.0, 6.0)), self.placed.names) {
                self.placed.button(Control::Level(i), hit, "Level", false);
            }
        }
    }

    fn draw_scope(&mut self, cx: &mut Cx2d, view: Rect) {
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        self.placed.dpi = cx.current_dpi_factor();
        self.quad(cx, view, rgba(look.canvas.canvas, 1.0));
        self.sync_probes(cx);
        let envelope = self.selection_envelope(cx);
        let newest = Self::record(cx);
        let has_take = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.status.take.is_some() || live.sim.status.continuous || live.opened.is_some());
        // a take that appears opens on the Take model, whatever the timebase was before it
        if has_take && !self.take.seen {
            self.take.scope = false;
        }
        self.take.seen = has_take;
        let axis = self.take_axis(cx);
        let take_end = axis.map(|(_, end, _)| end);
        match &newest {
            Some((store, scrub, playing)) if take_end.is_none() => self.sync_trigger(store, *scrub, *playing),
            _ => self.capture = None,
        }
        let shown_trigger = self.shown_trigger();
        follow_trigger(&mut self.cursors, self.cursor_trigger, shown_trigger);
        self.cursor_trigger = shown_trigger;
        let newest_store = newest.as_ref().map(|(store, ..)| store.clone());
        let record = self.shown(newest);
        self.sync_decoders(newest_store.as_ref());
        self.source = record.as_ref().map(|(store, ..)| StoreSource { store: store.clone() });
        let status = match &record {
            _ if self.trace_key.is_some() => "",
            None if self.lanes.is_empty() || envelope.is_some() => "",
            None => "Play the run to see its signals",
            Some(_) if Self::reviewing(cx) => "reviewing",
            Some((_, _, false)) => "paused",
            Some(_) => "",
        };
        let status = if self.trigger.as_ref().is_some_and(|t| t.mode == TrigMode::Normal) && record.is_some() && self.capture.is_none() {
            "waiting for trigger"
        } else { status };
        let take = axis.map(|(origin, end, continuous)| self.take.window_for(origin, end, continuous));
        // Follow playhead: while the take plays (not while the hand scrubs), its page turns
        let heard = match &record { Some((_, Some(t), true)) if !Self::reviewing(cx) => Some(*t), _ => None };
        let take = match (take, heard) {
            (Some(w), Some(head)) if axis.is_some_and(|(_, _, continuous)| !continuous) => Some(self.take.page_to(w, head)),
            (take, _) => take,
        };
        show_window(cx, take.map(|w| (w.t0, w.t1)));
        self.placed.buttons.clear();
        let name_w = (view.size.x * 0.32).clamp(138.0, 150.0).min(view.size.x);
        let ruler = Rect { pos: dvec2(view.pos.x + name_w, view.pos.y), size: dvec2((view.size.x - name_w).max(0.0), RULER_H) };
        let lanes_top = ruler.pos.y + RULER_H;
        let lanes_h = (view.pos.y + view.size.y - lanes_top).max(0.0);
        self.placed.ruler = ruler;
        self.placed.names = Rect { pos: dvec2(view.pos.x, lanes_top), size: dvec2(name_w, lanes_h) };
        self.placed.plot = Rect { pos: dvec2(ruler.pos.x, lanes_top), size: dvec2(ruler.size.x, lanes_h) };
        // The record's ranges, then the window every lane shares.
        if let Some((store, scrub, _)) = &record {
            for lane in &mut self.lanes {
                if let (Some((_, Some(k))), Some(source)) = (lane.row, self.source.as_ref()) {
                    if lane.cols_key.is_none_or(|key| key.0 != Arc::as_ptr(store) as usize || key.1 != store.len()) {
                        lane.range = source.range(k);
                    }
                }
            }
            let anchor = Self::anchor(store, *scrub, self.shown_trigger());
            self.placed.window = self.timebase.map_or((0.0, 1.0), |tb| tb.window(&anchor));
        } else {
            self.placed.window = (0.0, self.timebase.map_or(1.0, |tb| tb.span()));
        }
        // the take: one fixed axis over [0, L)
        if let Some(w) = take {
            self.placed.window = (w.t0, w.t1);
        }
        // Rows: heights by kind and the decoders under them, scrolled.
        let under = self.decoders_under();
        let mut heights: Vec<f64> = (0..self.lanes.len()).map(|i| self.block_h(i, &under)).collect();
        // Share the remaining height, with three parts for an analog lane and one for digital.
        let spare = (lanes_h - ADD_ROW - heights.iter().sum::<f64>()).max(0.0);
        self.stretch.clear();
        for i in 0..self.lanes.len() {
            let share = self.lane_size(i).share();
            self.stretch.push(share);
        }
        let weight: f64 = self.stretch.iter().sum();
        for (h, extra) in heights.iter_mut().zip(&mut self.stretch) {
            *extra *= spare / weight;
            *h += *extra;
        }
        let total: f64 = heights.iter().sum();
        self.scroll_y = self.scroll_y.clamp(0.0, (total + ADD_ROW - lanes_h).max(0.0));
        if let Some(net) = self.reveal.clone() {
            if let Some(i) = self.lane_index(&net) {
                let top: f64 = heights[..i].iter().sum();
                let h = heights[i].min(lanes_h);
                if top < self.scroll_y { self.scroll_y = top; }
                if top + h > self.scroll_y + lanes_h { self.scroll_y = (top + h - lanes_h).max(0.0); }
                self.reveal = None;
                if let Some((flashed, at)) = &mut self.flash {
                    if *flashed == net && at.is_nan() {
                        *at = cx.seconds_since_app_start();
                    }
                }
            }
        }
        let cols = (self.placed.plot.size.x * self.placed.dpi).ceil().max(1.0) as u32;
        let (t0, t1) = self.placed.window;
        self.placed.rows.clear();
        self.detail.clear();
        let record_end = record.as_ref().map(|(store, ..)| store.time.at(store.len() - 1));
        // the monitored lanes' meters: read once a frame (a read takes the peak), the peak held
        if let Some(mix) = self.mix.clone() {
            let read = cx.seconds_since_app_start();
            let fall = 10f32.powf(-PEAK_FALL_DB / 20.0 * (read - self.meter_at).clamp(0.0, 1.0) as f32);
            self.meter_at = read;
            for (i, m) in self.meters.iter_mut().enumerate() {
                let r = mix.meter(i);
                // the peak since the last read, held, and never under the RMS it covers
                *m = (r.rms, r.peak.max(r.rms).max(m.1 * fall));
            }
        }
        let running = cx.get_global_ref::<ElecAudio>().is_some_and(|audio| audio.ring.running());
        self.metering = self.mix.is_some() && (running || self.meters.iter().any(|m| m.0 > 1e-4 || m.1 > 1e-4));
        let dark = base_theme(cx) != BaseTheme::Light;
        let deep = if dark { mix_rgba(look.canvas.canvas, [0.0, 0.0, 0.0, 1.0], 0.45) } else { look.canvas.canvas };
        // Every row is a card on the sheet's large corner, a gap of the panel between two: its
        // name side on the part fill, its wave side deep.
        self.draw_face.radius = self.radius as f32;
        // the ruler's left cell is the notch's ground, so the notch reads as part of the row
        self.card(cx, ruler_card(view), name_w - LANE_GAP, self.color_chrome, rgba(deep, 1.0), None, None);
        // what is drawn flat from here (hue bars, meters, ticks) lies over the cards
        self.draw_quad.new_draw_call(cx);
        cx.push_clip_rect(Rect { pos: dvec2(view.pos.x, lanes_top), size: dvec2(view.size.x, lanes_h) });
        let now = cx.seconds_since_app_start();
        let head = self.scrubbed.or_else(|| cx.get_global_ref::<ElecLive>().filter(|live| live.opened.is_none()).map(|live| {
            live.sim.scrub.or(live.sim.heard.map(|(t, _)| t)).unwrap_or(live.sim.t)
        }));
        let value_at = head.or(record_end);
        let mut y = lanes_top - self.scroll_y;
        for i in 0..self.lanes.len() {
            let kind = self.lanes[i].kind(&self.overrides);
            let h = self.lane_size(i).base() + self.stretch.get(i).copied().unwrap_or(0.0);
            let row = Rect { pos: dvec2(view.pos.x, y), size: dvec2(view.size.x, h) };
            // the lane and the decoders under it
            let block = Rect { pos: row.pos, size: dvec2(view.size.x, heights[i]) };
            y += heights[i];
            if block.pos.y + block.size.y < lanes_top || block.pos.y > lanes_top + lanes_h {
                continue;
            }
            self.placed.rows.push((i, block));
            // Reduce the window when the record, the window or the width changed.
            if let (Some((store, ..)), Some((_, Some(k)))) = (&record, self.lanes[i].row) {
                let key = (Arc::as_ptr(store) as usize, store.len(), t0.to_bits(), t1.to_bits(), cols);
                if self.lanes[i].cols_key != Some(key) {
                    let lane = &mut self.lanes[i];
                    lane.cols.clone_from(lane.cache.columns(store, k, t0, t1, cols));
                    lane.cols_key = Some(key);
                    lane.update_gaps();
                    if !lane.swing && !lane.facts.logic && lane.facts.kind == fab_elec_circuit::NetKind::Signal && swings_like_logic(&lane.cols) {
                        lane.swing = true;
                    }
                    lane.detail.clear();
                    match logic_of(store, k) {
                        // the pad's edges: the whole run, whatever the record's length; its spilled
                        // chunks the window shows in detail come back from the bench
                        Some(trace) => {
                            edge_runs(trace, t0, t1, cols, store.time.at(store.len() - 1), &mut lane.runs);
                            if kind == LaneKind::Digital {
                                lane.detail.extend(trace.spilled_in(t0, t1, 0.5 * (t1 - t0) / f64::from(cols.max(1))));
                                lane.gaps.clear();
                            }
                        }
                        None => logic_runs(&lane.cols, lane.thresholds(), &mut lane.runs),
                    }
                }
                if let Ok(k) = u32::try_from(k) {
                    self.detail.extend(self.lanes[i].detail.iter().map(|&first| (SignalId(k), first)));
                }
            } else {
                let lane = &mut self.lanes[i];
                lane.cols = Columns::default();
                lane.cache.clear();
                lane.runs.clear();
                lane.detail.clear();
                lane.cols_key = None;
            }
            let output = take.is_some() && self.outs.contains(&self.lanes[i].net);
            let hue = if output { look.text } else { look.channel(self.lanes[i].hue) };
            let header = Rect { pos: row.pos, size: dvec2(name_w, h) };
            let card = lane_card(view, block);
            let focus = (self.focused_lane.as_ref() == Some(&self.lanes[i].net)).then(|| rgba(look.lane_focus, 1.0));
            let flash = self.flash.as_ref().filter(|(net, _)| *net == self.lanes[i].net).map(|(_, at)| 1.0 - ((now - at) / FLASH).clamp(0.0, 1.0));
            let tint = flash.filter(|f| *f > 0.0).map(|f| rgba(hue, 0.28 * f as f32));
            self.card(cx, card, name_w - LANE_GAP, rgba(look.canvas.part_fill, 1.0), rgba(deep, 1.0), focus, tint);
            // the lane's coarse envelope fills what its record has no point of
            let lane_env = if self.lanes[i].selection { envelope.clone() } else { self.lane_envelope(cx, self.lanes[i].net_ref) };
            let slots = lane_env.as_deref().map(Vec::as_slice).unwrap_or_default();
            self.draw_lane_header(cx, look, i, header, hue, value_at, slots);
            // The trace, inside its card.
            let wave_w = (card.pos.x + card.size.x - self.placed.plot.pos.x).max(0.0);
            cx.push_clip_rect(Rect { pos: dvec2(self.placed.plot.pos.x, card.pos.y), size: dvec2(wave_w, h.min(card.size.y)) });
            match kind {
                LaneKind::Digital => self.draw_digital(cx, i, row, rgba(hue, 1.0), slots),
                LaneKind::Analog => self.draw_analog(cx, i, row, rgba(hue, 1.0), take.is_some(), slots),
            }
            cx.pop_clip_rect();
            // its decoders, in the hue of each one's first lane
            let mut top = row.pos.y + h;
            for &d in &under[i] {
                let first = self.decoders[d].nets.iter().flatten().find_map(|net| self.lane_index(net));
                let hue = first.map_or(hue, |l| look.channel(self.lanes[l].hue));
                top += self.draw_decoder(cx, look, d, top, rgba(hue, 1.0), record_end);
            }
        }
        // The insertion line of a dragged lane (the list-reorder rule).
        if let Some(at) = self.insert_at {
            let y = self.placed.rows.iter().find(|(i, _)| *i == at).map(|(_, r)| r.pos.y)
                .or_else(|| self.placed.rows.last().map(|(_, r)| r.pos.y + r.size.y));
            if let Some(y) = y {
                self.quad(cx, Rect { pos: dvec2(view.pos.x + 4.0, y - 1.0), size: dvec2(view.size.x - 8.0, 2.0) }, rgba(look.text, 0.9));
            }
        }
        cx.pop_clip_rect();
        if record.is_none() && envelope.is_none() && !self.lanes.is_empty() {
            self.draw_text.color = rgba(look.text_tertiary, 1.0);
            self.draw_text.draw_abs(cx, dvec2(ruler.pos.x + 12.0, lanes_top + 8.0), "No record: press Play");
        }
        let plot_bottom = (lanes_top + total - self.scroll_y).clamp(lanes_top, lanes_top + lanes_h);
        let add = Rect { pos: dvec2(view.pos.x + 4.0, lanes_top + total - self.scroll_y + 4.0), size: dvec2(58.0, MENU_ROW) };
        let lane_area = Rect { pos: dvec2(view.pos.x, lanes_top), size: dvec2(view.size.x, lanes_h) };
        if let Some(hit) = semantics::intersect(add, lane_area) {
            cx.push_clip_rect(lane_area);
            self.control_face(cx, look, add, None);
            self.draw_centred(cx, false, add, "+ add lane", rgba(look.text_tertiary, 1.0));
            self.placed.button(Control::Scope(Chip::AddLane), hit, "+ add lane", false);
            let hint = if self.add_hint || self.lanes.is_empty() {
                "Click a wire on the sheet (or press P over one) to add its lane"
            } else if take.is_some() {
                "Drag traces to pan · wheel to zoom · click for playhead · drag ruler to scrub · Shift-drag to measure · Esc clears"
            } else {
                "Drag traces to pan · wheel to zoom · click to measure · Esc clears"
            };
            self.draw_text.color = rgba(look.text_tertiary, 1.0);
            self.draw_text.draw_abs(cx, add.pos + dvec2(add.size.x + 6.0, 3.0), hint);
            cx.pop_clip_rect();
        }
        cx.push_clip_rect(Rect { pos: ruler.pos, size: dvec2(ruler.size.x, (plot_bottom - ruler.pos.y).max(0.0)) });
        self.draw_ruler(cx, look, ruler, plot_bottom, take.is_some());
        cx.pop_clip_rect();
        self.draw_ruler_tools(cx, look, ruler, take.is_some());
        if let Some(end) = take_end {
            self.draw_coverage(cx, look, view, ruler, plot_bottom, record_end.unwrap_or(0.0).min(end));
        }
        // Draw the cursors over the recorded traces and the coverage.
        self.draw_quad.new_draw_call(cx);
        let plot = self.placed.plot;
        let dpi = self.placed.dpi;
        let span_y = (plot_bottom - ruler.pos.y).max(0.0);
        cx.push_clip_rect(Rect { pos: ruler.pos, size: dvec2(plot.size.x, span_y) });
        if self.press.is_none() || matches!(self.press, Some(Press::Cursor(_))) {
            if let Some(t) = self.hover_t {
                let x = (self.placed.x_of(t) * dpi).round() / dpi;
                self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y), size: dvec2(1.0 / dpi, span_y) }, rgba(look.text, 0.25));
            }
        }
        if let Some(t) = head.filter(|t| (t0..=t1).contains(t)) {
            let x = (self.placed.x_of(t) * dpi).round() / dpi;
            let ink = rgba(look.canvas.ok, 1.0);
            // the line through each card, its head on the ruler's card
            for k in 0..self.placed.rows.len() {
                let card = lane_card(view, self.placed.rows[k].1);
                let (top, bottom) = (card.pos.y.max(lanes_top), (card.pos.y + card.size.y).min(plot_bottom));
                if bottom > top {
                    self.quad(cx, Rect { pos: dvec2(x - 1.0, top), size: dvec2(2.0, bottom - top) }, ink);
                }
            }
            self.draw_head.color = ink;
            self.draw_head.draw_abs(cx, Rect { pos: dvec2(x - 4.0, lanes_top - LANE_GAP - 6.0), size: dvec2(8.0, 6.0) });
        }
        for (label, t, a) in [("A", self.cursors.a, 0.9), ("B", self.cursors.b, 0.6)] {
            let Some(t) = t.filter(|t| (t0..t1).contains(t)) else { continue };
            let x = (self.placed.x_of(t) * dpi).round() / dpi;
            self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y), size: dvec2(1.0 / dpi, span_y) }, rgba(look.text, a));
            self.quad(cx, Rect { pos: dvec2(x, ruler.pos.y), size: dvec2(12.0, 13.0) }, rgba(look.text, a));
            self.draw_text.color = rgba(look.canvas.canvas, 1.0);
            self.draw_text.draw_abs(cx, dvec2(x + 2.0, ruler.pos.y), label);
        }
        cx.pop_clip_rect();
        self.draw_pick_prompt(cx, look, ruler);
        self.draw_trace_banner(cx, look, ruler);
        if self.picking.is_none() && (self.cursors.a.is_some() || self.cursors.b.is_some()) {
            let mut readout = cursor_readout(&self.cursors, (t1 - t0) / DIVS);
            if let Some(dv) = self.measured_dv() {
                let _ = write!(readout, "   {dv}");
            }
            let w = self.text_width_code(cx, &readout) + 8.0;
            let rect = Rect { pos: dvec2(ruler.pos.x + (ruler.size.x - w).max(0.0), ruler.pos.y), size: dvec2(w, CHIP) };
            self.face(cx, rect, rgba(look.panel, 1.0));
            self.draw_centred(cx, true, rect, &readout, rgba(look.text, 1.0));
        }
        // A hidden tab keeps no record alive: the next draw takes the newest again.
        self.source = None;
        // A lane dragged by its name rides the pointer as a chip of it, above everything drawn.
        if let (Some(at), Some(Press::Name { net, .. })) = (self.drag_at, &self.press) {
            if let Some((label, hue)) = self.lanes.iter().find(|l| !l.selection && &l.net == net).map(|l| (l.label.clone(), look.channel(l.hue))) {
                let w = self.text_width(cx, false, &label) + 22.0;
                let chip = Rect { pos: dvec2(at.x + 14.0, at.y - 12.0), size: dvec2(w, 24.0) };
                self.draw_face.new_draw_call(cx);
                self.draw_face.radius = self.radius_lg as f32;
                self.face(cx, chip, rgba(look.popover, 1.0));
                self.draw_face.radius = self.radius as f32;
                self.draw_quad.new_draw_call(cx);
                self.quad(cx, Rect { pos: chip.pos + dvec2(4.0, 4.0), size: dvec2(3.0, chip.size.y - 8.0) }, rgba(hue, 1.0));
                self.draw_text.new_draw_call(cx);
                self.draw_text.color = rgba(look.text, 1.0);
                self.draw_text.draw_abs(cx, chip.pos + dvec2(12.0, 4.0), &label);
            }
        }
        if let Some(field) = self.rename.as_ref().map(|(_, field)| *field) {
            let walk = Walk { abs_pos: Some(field.pos), width: Size::Fixed(field.size.x), height: Size::fit(), ..Walk::default() };
            cx.push_clip_rect(self.placed.names);
            let _ = self.name_field.draw_walk(cx, &mut Scope::empty(), walk);
            cx.pop_clip_rect();
            if self.rename_focus {
                self.rename_focus = false;
                cx.set_key_focus_from_user(self.name_field.area());
            }
        }
        self.draw_menu(cx, look, view, status, take.is_some());
        if (self.metering || self.flash.as_ref().is_some_and(|(_, at)| now - at < FLASH)) && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
        if self.flash.as_ref().is_some_and(|(_, at)| now - at >= FLASH) {
            self.flash = None;
        }
    }

    /// A text width without a draw context (for ellipsis): 0.6 em a character.
    fn text_width_estimate(&self, s: &str) -> f64 {
        s.chars().count() as f64 * 11.0 * 0.6
    }
}

/// `text` cut with an ellipsis to fit `room` points by `width`.
fn fit_text(text: &str, room: f64, width: impl Fn(&str) -> f64) -> String {
    if width(text) <= room {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while !out.is_empty() && width(&format!("{out}…")) > room {
        out.pop();
    }
    format!("{out}…")
}

impl Drop for LogicScope {
    fn drop(&mut self) {
        for &net in &self.asked_envelopes {
            Cx::post_action(SimAction::Envelope { id: self.envelope_id(net), net: None });
        }
        if !self.asked_probes.is_empty() {
            Cx::post_action(SimAction::Probes { id: self.ask_id(SCOPE_RECORD), rows: Vec::new() });
        }
    }
}

impl WidgetNode for LogicScope {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _point: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl LogicScope {
    /// A Scope that has not shown a take yet starts on the window last used ([`SharedTake`]).
    fn pull_take(&mut self, cx: &mut Cx) {
        if !cx.has_global::<SharedTake>() {
            cx.set_global(SharedTake::default());
        }
        let shared = cx.get_global::<SharedTake>();
        if !self.take.seen && shared.generation != self.take_generation {
            self.take = shared.view.clone();
            self.take_generation = shared.generation;
        }
    }

    /// Writes this Scope's window back as the one last used, when it changed since `before`.
    fn push_take(&mut self, cx: &mut Cx, before: &TakeView) {
        let shared = cx.get_global::<SharedTake>();
        if *before != self.take {
            shared.view = self.take.clone();
            shared.generation += 1;
            self.take_generation = shared.generation;
        }
    }

    /// This instance's id in an ask of the host (`base` mixed with its widget uid): two Scopes
    /// ask, show and hide on their own.
    fn ask_id(&self, base: u64) -> u64 {
        base ^ self.uid.0.rotate_left(17)
    }

    fn button_key(&self, b: &Button) -> u64 {
        match b.control {
            Control::Lane(i, _) | Control::Level(i) => semantics::key(&b.label, &self.lanes[i].net, 0),
            Control::Decoder(id, _) => semantics::key("decoder", &b.label, id),
            Control::Scope(Chip::Master) => LiveId::from_str("scope master").0,
            Control::Scope(Chip::Mode) => LiveId::from_str("trigger mode").0,
            Control::Scope(Chip::Offer(i)) => semantics::key("decoder offer", &b.label, i as u64),
            Control::Scope(_) => semantics::key("scope", &b.label, 0),
        }
    }

    fn draw_walk_shared(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.follow_listen(cx);
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        cx.push_clip_rect(view);
        self.draw_scope(cx, view);
        cx.pop_clip_rect();
        self.live_key = live_key(cx);
        self.asked_draw = None;
        if !self.shown {
            self.shown = true;
            Cx::post_action(SimAction::ShowScope { id: self.ask_id(SCOPE_VIEW), sink: SinkSlot::none(), column: 0.0 });
        }
        DrawStep::done()
    }

    fn handle_event_shared(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let renaming = self.rename.is_some();
        let lost = matches!(event, Event::KeyFocus(e) if !e.prev.is_empty() && e.prev == self.name_field.area());
        if renaming || lost {
            for action in cx.capture_actions(|cx| self.name_field.handle_event(cx, event, scope)) {
                match action.as_widget_action().cast() {
                    TextInputAction::Returned(text, _) => self.end_rename(cx, Some(text)),
                    TextInputAction::KeyFocusLost | TextInputAction::Escaped => self.end_rename(cx, None),
                    _ => {}
                }
            }
            if renaming && matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)) {
                return;
            }
        }
        self.follow_listen(cx);
        self.sync_probes(cx);
        // hidden: two draw events passed since a redraw was asked, and neither walked the Scope
        if self.shown && self.asked_draw.is_some_and(|at| cx.drawing_id() >= at + 2) {
            (self.shown, self.asked_draw) = (false, None);
            cx.action(SimAction::HideScope { id: self.ask_id(SCOPE_VIEW) });
            show_window(cx, None);
            self.detail.clear();
        }
        self.sync_detail(cx);
        let decoded = self.decode_worker.as_ref().is_some_and(|worker| worker.out.published() != self.decoded.as_ref().map_or(0, |(seen, _)| *seen));
        if matches!(event, Event::NextFrame(_) | Event::Signal) && (self.live_key != live_key(cx) || decoded || self.metering || self.flash.as_ref().is_some_and(|(_, at)| !at.is_nan())) {
            self.area.redraw(cx);
            self.asked_draw.get_or_insert(cx.drawing_id());
        }
        if let Event::Actions(actions) = event {
            for &PanScopeWindow(t0, t1) in actions.iter().filter_map(|a| a.downcast_ref::<PanScopeWindow>()) {
                if self.take.window.is_some() && t1 > t0 {
                    self.take.place(TakeWindow { t0, t1 });
                    self.redraw_soon(cx);
                }
            }
        }
        if self.menu.is_some() {
            let outside = match event {
                Event::MouseDown(e) => !self.area.clipped_rect(cx).contains(e.abs),
                Event::TouchUpdate(e) => e.touches.iter().any(|t| matches!(t.state, makepad_widgets_core::makepad_draw::makepad_platform::event::TouchState::Start) && !self.area.clipped_rect(cx).contains(t.abs)),
                _ => false,
            };
            if outside {
                self.menu = None;
                self.placed.buttons.clear();
                self.area.redraw(cx);
            }
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => self.hover(cx, e.abs),
            Hit::FingerHoverOut(_) => {
                let hovered = self.pointer.take().is_some();
                if self.hover_t.take().is_some() || hovered {
                    self.area.redraw(cx);
                }
            }
            Hit::FingerScroll(e) => {
                // over a monitored lane's meter or speaker chip, or the master: 1 dB a notch
                let hit = self.placed.hit(e.abs);
                let lane_level = match hit {
                    Some(Control::Level(i)) => Some(i),
                    Some(Control::Lane(i, LaneChip::Listen)) if listening(cx, &self.lanes[i].net) => Some(i),
                    _ => None,
                };
                let master = hit == Some(Control::Scope(Chip::Master));
                if lane_level.is_some() || master {
                    self.wheel += e.scroll.y;
                    let steps = (self.wheel / 40.0).trunc();
                    if steps != 0.0 {
                        self.wheel -= steps * 40.0;
                        match lane_level {
                            Some(lane) => {
                                let db = level_db(cx, &self.lanes[lane].net);
                                self.set_level(cx, lane, db - steps as f32);
                            }
                            None => self.set_master(cx, master_db(cx) - steps as f32),
                        }
                    }
                } else if self.menu.is_some() {
                    if self.placed.menu.is_some_and(|r| r.contains(e.abs)) {
                        self.menu_scroll += e.scroll.y;
                        self.area.redraw(cx);
                    }
                } else if self.placed.names.contains(e.abs) {
                    self.scroll_y += e.scroll.y;
                    self.area.redraw(cx);
                } else if let Some(d) = scroll_pans(e.scroll.x, e.scroll.y, e.modifiers.shift)
                    .filter(|_| (self.placed.plot.contains(e.abs) || self.placed.ruler.contains(e.abs)) && self.take.window.is_some() && self.take_end(cx).is_some())
                {
                    // the take pans: a sideways scroll, or Shift and the wheel (scrolled content
                    // moves against the scroll, as a list does)
                    let w = self.take.window.unwrap_or(fit_from(self.take.origin, self.take.end - self.take.origin));
                    self.take.drag(w, -d, self.placed.plot.size.x);
                    self.area.redraw(cx);
                } else if self.placed.plot.contains(e.abs) || self.placed.ruler.contains(e.abs) {
                    // Wheel notches are 40 points; a trackpad's small deltas add up to steps.
                    self.wheel += e.scroll.y;
                    let steps = (self.wheel / 40.0).trunc() as i32;
                    if steps != 0 {
                        self.wheel -= f64::from(steps) * 40.0;
                        let fraction = (e.abs.x - self.placed.plot.pos.x) / self.placed.plot.size.x.max(1.0);
                        self.zoom(cx, steps, fraction);
                    }
                }
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                cx.set_key_focus_from_user(self.area);
                self.press(cx, e.abs, e.modifiers.shift, e.modifiers.alt, e.tap_count);
            }
            Hit::FingerMove(e) => self.drag(cx, e.abs),
            Hit::FingerUp(e) => {
                if e.cancelled {
                    if matches!(self.press.take(), Some(Press::Scrub | Press::Pan { .. })) {
                        self.end_scrub(cx);
                    }
                    self.insert_at = None;
                    self.drag_at = None;
                    self.area.redraw(cx);
                } else {
                    self.release(cx, e.abs, e.is_over);
                }
            }
            Hit::KeyFocusLost(_) if self.menu.is_some() => {
                self.menu = None;
                self.placed.buttons.clear();
                self.area.redraw(cx);
            }
            // R records the loop again, as the bar's ●↻ (R) does
            Hit::KeyDown(e) if e.key_code == KeyCode::KeyR && !(e.modifiers.shift || e.modifiers.control || e.modifiers.alt || e.modifiers.logo) => {
                crate::live::transport::record(cx, crate::live::strip::RunMode::Loop);
            }
            Hit::KeyDown(e) if e.key_code == KeyCode::Escape => {
                if self.menu.take().is_some() {
                    self.placed.buttons.clear();
                } else if self.picking.take().is_none() {
                    (self.cursors, self.measure_lane) = (Cursors::default(), None);
                }
                self.area.redraw(cx);
            }
            _ => {}
        }
    }
}

impl Widget for LogicScope {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.pull_take(cx);
        let before = self.take.clone();
        let step = self.draw_walk_shared(cx, scope, walk);
        self.push_take(cx, &before);
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.pull_take(cx);
        let before = self.take.clone();
        self.handle_event_shared(cx, event, scope);
        self.push_take(cx, &before);
    }

    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Figure;
        makepad_platform::a11y::set_string(&mut node.name, "Scope");
        node.value.clear();
        // the take's visible window while the Take model shows, else the timebase's
        let (t0, t1) = self.take.window.filter(|_| !self.take.scope && self.take.seen).map_or(self.placed.window, |w| (w.t0, w.t1));
        let per_div = self.timebase.map_or_else(String::new, |tb| per_div_label(tb.per_div));
        let _ = write!(&mut node.value, "{} lanes, {per_div}, {} .. {}", self.lanes.len(), eng(t0, "s", 6), eng(t1, "s", 6));
        for lane in &self.lanes {
            let _ = write!(&mut node.value, "\n{}", lane.label);
        }
        if let Some(t) = &self.trigger {
            let _ = write!(&mut node.value, "\ntrigger {} {}", t.net, slope_word(t.slope));
        }
        if let Some(t) = self.shown_trigger() {
            let _ = write!(&mut node.value, " at {}", eng(t, "s", 5));
        }
        if let (Some(tb), true) = (self.timebase, self.cursors.a.is_some() || self.cursors.b.is_some()) {
            let _ = write!(&mut node.value, "\n{}", cursor_readout(&self.cursors, tb.per_div));
        }
        if !self.tap_nets.is_empty() {
            let _ = write!(&mut node.value, "\nlistening, master {}{}", db_label(master_db(cx)), if muted(cx) { ", muted" } else { "" });
            for (net, (rms, peak)) in self.tap_nets.iter().zip(&self.meters) {
                let label = self.lanes.iter().find(|l| l.net == *net).map_or(net.as_str(), |l| l.label.as_str());
                let db = level_db(cx, net);
                let _ = write!(&mut node.value, "\n{label} {}: rms {rms:.3} peak {peak:.3}", db_label(db));
            }
            if let Some(status) = listen_status(cx) {
                let _ = write!(&mut node.value, "\n{status}");
            }
            if let Some(save) = listen::save_line(cx) {
                let _ = write!(&mut node.value, "\n{save}");
            }
        }
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        use a11y_tree::{Actions, Node, RangeValue, Role, States};
        let clip = self.area.clipped_rect(cx);
        for button in &self.placed.buttons {
            let Some(hit) = semantics::intersect(button.rect, clip) else { continue };
            let mut node = Node {
                role: Role::Button,
                name: button.label.to_string(),
                bounds: makepad_platform::a11y::semantic_rect(hit),
                actions: Actions::PRESS,
                ..Default::default()
            };
            let db = match button.control {
                Control::Lane(i, chip) => {
                    let lane = &self.lanes[i];
                    node.description.clone_from(&lane.label);
                    if chip == LaneChip::Kind {
                        node.value = match self.overrides.get(&lane.net) {
                            Some(LaneKind::Analog) => "analog", Some(LaneKind::Digital) => "digital", None => "auto"
                        }.into();
                    }
                    None
                }
                Control::Level(i) => {
                    node.description.clone_from(&self.lanes[i].label);
                    Some(level_db(cx, &self.lanes[i].net))
                }
                Control::Scope(Chip::Master) => {
                    node.name = "Master".into();
                    Some(master_db(cx))
                }
                _ => None,
            };
            if let Some(db) = db {
                node.role = Role::Slider;
                node.value = db_label(db);
                node.actions = Actions::INCREMENT | Actions::DECREMENT | Actions::SET_VALUE;
                node.range = Some(RangeValue { min: LEVEL_MIN_DB.into(), max: LEVEL_MAX_DB.into(), now: db.into(), step: 1.0 });
            }
            node.states.set(States::DISABLED, !button.enabled);
            node.states.set(States::HAS_POPUP, matches!(button.control, Control::Scope(Chip::More | Chip::Decode) | Control::Lane(_, LaneChip::Kind | LaneChip::More)));
            node.states.set(States::SELECTED, button.selected);
            out.push((self.button_key(button), None, node));
        }
        if self.menu.is_some() { return }
        let Some(w) = self.take.window.filter(|_| !self.take.scope && self.take.seen && self.placed.ruler.size.x > 0.0) else { return };
        let mut node = Node {
            role: Role::Slider,
            bounds: makepad_platform::a11y::semantic_rect(self.placed.ruler),
            ..Default::default()
        };
        makepad_platform::a11y::set_string(&mut node.name, "time ruler");
        let _ = write!(&mut node.value, "{} .. {}", eng(w.t0, "s", 6), eng(w.t1, "s", 6));
        out.push((LiveId::from_str("time ruler").0, None, node));
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(button) = self.placed.buttons.iter().find(|b| self.button_key(b) == key) else { return ActionResult::Unhandled };
        if !button.enabled { return ActionResult::Disabled }
        let control = button.control;
        let current = match control {
            Control::Level(i) => Some(level_db(cx, &self.lanes[i].net)),
            Control::Scope(Chip::Master) => Some(master_db(cx)),
            _ => None,
        };
        if let Some(db) = current {
            let next = match request.action {
                Actions::INCREMENT => db + 1.0,
                Actions::DECREMENT => db - 1.0,
                Actions::SET_VALUE => {
                    let Some(v) = semantics::number(&request.arg, "dB") else { return ActionResult::Unsupported };
                    v as f32
                }
                _ => return ActionResult::Unsupported,
            };
            if let Control::Level(i) = control { self.set_level(cx, i, next); } else { self.set_master(cx, next); }
        } else if request.action == Actions::PRESS {
            self.pull_take(cx);
            let before = self.take.clone();
            self.activate(cx, control);
            self.push_take(cx, &before);
        } else {
            return ActionResult::Unsupported;
        }
        ActionResult::Handled
    }
}

impl LogicScopeRef {
    /// [`LogicScope::show_lane`] through the widget reference.
    pub fn show_lane(&self, cx: &mut Cx, net: &str) -> bool {
        self.borrow_mut().is_some_and(|mut scope| scope.show_lane(cx, net))
    }
}

/// The logic trace of signal `k` in `store`, when a pad reads its net.
/// A saved trace's name in a menu: its file's stem (`captures/take-1.trace` is `take-1`).
fn trace_name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".trace").unwrap_or(file)
}

fn logic_of(store: &TraceStore, k: usize) -> Option<&LogicTrace> {
    store.logic(SignalId(u32::try_from(k).ok()?))
}
