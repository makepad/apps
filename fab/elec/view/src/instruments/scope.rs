//! One signal or meter lane: a tag, answers, and the existing plot gestures.

use super::{
    acquire::Zone,
    axis::{axis_label, axis_prefix, linear_ticks, log_ticks, si_label, time_label, Tick},
    delay::{edge_near, follow_edge, DelayGesture, DelayMark},
    distribution::Distribution,
    distribution_view::Histogram,
    gesture::{drag_db_reference, drag_waterfall_share, fit_time, fit_values, lock_axis, waterfall_split, zoom_db_about, zoom_time_about, zoom_value_about, Axis, ZoneDrag},
    look::InstrumentLook,
    trend::TrendQuantity,
    trigger_ui::{centred_level, ghost_level, holdoff_drag},
    phosphor::{Hits, PhosphorMap},
    phosphor_view::{PhosphorLook, PhosphorView, Pushed, Stretch},
    reticle::{answer_geometry, snap, Geometry, ReticleLabel},
    simfeed::{CaptureFeatures, Feature, FeatureKind},
    span::{bracket_end_at, value_bracket_end_at, BracketLabel, End, SpanStats, ValueBracketLabel},
    spectrum::{bin_x, SpectrumMeasurements},
};
use crate::{
    canvas::looks::Tokens,
    live::tag::{DrawProbeTag, LaneSide, ProbeTag, TagKind, TagState, TagStyle},
    trace::{cursor::eng, spark::NetSpark},
};
use fab_elec_bench::trigger::{validate_qualifier, validate_runt, EdgeTrigger, Qualifier, Slope, Window};
use fab_elec_bench::source::{Caps, ChanId, Clipped, Over, Resolved, Side, TrigResolved};
use std::sync::Arc;
use makepad_widgets_core::makepad_draw::{
    shader::draw_text::{FontFamily, TextOverflow},
    text::layouter::LaidoutText,
};
use makepad_widgets_core::*;
use std::{collections::HashMap, ops::Range, rc::Rc, time::{Duration, Instant}};

#[path = "scope_nodes.rs"]
mod nodes;

#[path = "scope_distributions.rs"]
mod distributions;

#[path = "scope_palette.rs"]
mod palette;
use palette::{answer_name, SPECTRUM_ANSWERS};

mod registration {
    use super::*;

    script_mod! {
        use mod.prelude.widgets_internal.*
        use mod.widgets.*

        mod.widgets.SignalAnswerLabel = Label {
            padding: 0
            draw_text +: {text_style: theme.font_code {font_size: 11}}
        }
        mod.widgets.SignalAnswerRow = View {
            width: Fit height: 26 flow: Right align: Align {y: 0.5}
            name := mod.widgets.SignalAnswerLabel {}
            value := mod.widgets.SignalAnswerLabel {}
            unit := mod.widgets.SignalAnswerLabel {}
            ghost := mod.widgets.SignalAnswerLabel {}
            stats := mod.widgets.SignalAnswerLabel {visible: false padding: Inset {left: 16}}
            expect := mod.widgets.SignalAnswerLabel {visible: false padding: Inset {left: 16}}
            pin := ButtonFlatter {
                width: 40 height: 26 padding: 0 margin: 0 text: "pin" visible: false
                draw_text +: {text_style: theme.font_code {font_size: 11} get_color: fn() {return self.color}}
            }
            trigger_shorter := ButtonFlatter {
                width: Fit height: 26 padding: Inset {left: 8 right: 8} margin: 0 text: "trigger shorter" visible: false
                draw_text +: {text_style: theme.font_code {font_size: 11} get_color: fn() {return self.color}}
            }
            trigger_longer := ButtonFlatter {
                width: Fit height: 26 padding: Inset {left: 8 right: 8} margin: 0 text: "trigger longer" visible: false
                draw_text +: {text_style: theme.font_code {font_size: 11} get_color: fn() {return self.color}}
            }
            stop_on_fail := ButtonFlatter {
                width: Fit height: 26 padding: Inset {left: 8 right: 8} margin: 0 text: "stop on fail" visible: false
                draw_text +: {text_style: theme.font_code {font_size: 11} get_color: fn() {return self.color}}
            }
        }
        mod.widgets.SignalAnswerDistributionRow = View {
            width: Fit height: Fit flow: Down
            answer := mod.widgets.SignalAnswerRow {}
            distribution := mod.widgets.AnswerDistribution {visible: false}
        }
        mod.widgets.SignalSpectrumChoice = ButtonFlatter {
            height: 26 margin: 0 padding: Inset {left: 6 right: 6}
            draw_text +: {text_style: theme.font_code {font_size: 11}}
        }
        mod.widgets.SignalSpectrumRow = View {
            width: Fit height: 26 flow: Right align: Align {y: 0.5}
            name := mod.widgets.SignalAnswerLabel {width: 84}
        }
        mod.widgets.SignalMeasureRow = View {
            width: Fit height: Fit flow: Down
            heading := mod.widgets.SignalAnswerLabel {height: 26}
            choice := ButtonFlatter {
                width: Fit height: 26 margin: 0 padding: Inset {left: 6 right: 6}
                draw_text +: {text_style: theme.font_code {font_size: 11} get_color: fn() {return self.color}}
            }
        }
        mod.widgets.SignalLaneBase = #(SignalLane::register_widget(vm))
        mod.widgets.SignalLane = set_type_default() do mod.widgets.SignalLaneBase {
            width: Fill height: Fit
            phosphor: PhosphorView {}
            spectrum_view: PhosphorView {}
            spark: NetSpark {}
            draw_tag: mod.draw.DrawProbeTag {}
            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 11}}
            draw_axis +: {color: theme.color_text text_style: theme.font_code {font_size: 10}}
            draw_label +: {color: theme.color_text text_style: theme.font_regular {font_size: 13}}
            draw_more +: {
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.box(0., 0., self.rect_size.x, self.rect_size.y, 4.)
                    sdf.fill(self.color)
                    return sdf.result
                }
            }
            answer_row: mod.widgets.SignalAnswerDistributionRow {}
            answer_tip: mod.widgets.DragChip {}
            measure_row: mod.widgets.SignalMeasureRow {}
            measure_popover: Popover {
                trigger: Manual trap_focus: true
                content := ScrollYView {width: Fit height: Fit flow: Down}
            }
            answers_popover: Popover {
                trigger: Manual trap_focus: true
                content := View {
                    width: Fit height: Fit flow: Down
                }
            }
            value_popover: Popover {
                trigger: Manual trap_focus: true
                content := View {
                    width: Fit height: Fit flow: Down spacing: 4
                    input := TextInput {
                        width: 220 height: Fit keep_focus_on_submit: true
                        draw_text +: {text_style: theme.font_code {font_size: 11}}
                    }
                    error := mod.widgets.SignalAnswerLabel {visible: false}
                }
            }
            spectrum_popover: Popover {
                trigger: Manual trap_focus: true
                content := View {
                    width: Fit height: Fit flow: Down
                    window := mod.widgets.SignalSpectrumRow {
                        name +: {text: "window"}
                        hann := mod.widgets.SignalSpectrumChoice {}
                        blackman := mod.widgets.SignalSpectrumChoice {}
                        flat_top := mod.widgets.SignalSpectrumChoice {}
                        rect := mod.widgets.SignalSpectrumChoice {}
                    }
                    frequency := mod.widgets.SignalSpectrumRow {
                        name +: {text: "frequency"}
                        log := mod.widgets.SignalSpectrumChoice {}
                        linear := mod.widgets.SignalSpectrumChoice {}
                    }
                    waterfall := mod.widgets.SignalSpectrumRow {
                        name +: {text: "waterfall"}
                        on := mod.widgets.SignalSpectrumChoice {}
                        off := mod.widgets.SignalSpectrumChoice {}
                    }
                }
            }
        }
    }
}

/// Registers the lane after the phosphor screen and net spark.
pub fn script_mod(vm: &mut ScriptVm) {
    registration::script_mod(vm);
}

/// The smallest and largest supported divisions, in base units.
pub const DIV_MIN: f64 = 1e-12;
pub const DIV_MAX: f64 = 1e12;
pub const INTENSITY_MIN: f64 = 10.0;
pub const INTENSITY_MAX: f64 = 100_000.0;

/// A bracket is pulled off only across its drag axis, beyond the lane's 24-point margin.
#[doc(hidden)]
pub fn pulled_off(lane: Rect, pointer: DVec2, across_y: bool) -> bool {
    if across_y { pointer.y < lane.pos.y - 24.0 || pointer.y > lane.pos.y + lane.size.y + 24.0 }
    else { pointer.x < lane.pos.x - 24.0 || pointer.x > lane.pos.x + lane.size.x + 24.0 }
}

/// Moves along the bounded 1-2-5 menu; zero steps snaps to the nearest entry.
pub fn div_steps(value: f64, steps: i32) -> f64 {
    let value = if value.is_nan() || value <= 0.0 {
        DIV_MIN
    } else {
        value.clamp(DIV_MIN, DIV_MAX)
    };
    let log = value.log10();
    let mut best = 0i32;
    let mut distance = f64::INFINITY;
    for i in 0..=72 {
        let candidate = division(i);
        let d = (candidate.log10() - log).abs();
        if d < distance {
            distance = d;
            best = i;
        }
    }
    division(best.saturating_add(steps).clamp(0, 72))
}

fn division(index: i32) -> f64 {
    [1.0, 2.0, 5.0][(index % 3) as usize] * 10.0_f64.powi(index / 3 - 12)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The probe's label: its title ("U2.3"), then after a line break the pin's own name when it has one ("NRST").
    pub name: String,
    pub unit: String,
    pub volts_per_div: f64,
    /// The centre value is this offset times the channel scale.
    pub offset_div: f64,
    pub on: bool,
    pub color_index: usize,
}

impl Channel {
    /// The label's first line, shown on the probe tag.
    pub fn title(&self) -> &str {
        self.name.split('\n').next().unwrap_or_default()
    }

    /// The pin's own name, the label's second line, if it has one.
    pub fn pin_name(&self) -> Option<&str> {
        self.name.split_once('\n').map(|(_, name)| name.trim()).filter(|name| !name.is_empty())
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            name: "CH1".into(),
            unit: "V".into(),
            volts_per_div: 1.0,
            offset_div: 0.0,
            on: true,
            color_index: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trigger {
    pub auto: bool,
    /// Zero-based channel index.
    pub channel: usize,
    pub level: f64,
    pub slope: Slope,
    pub holdoff: f64,
    pub qualify: Qualifier,
    pub zone: Option<Zone>,
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            auto: true,
            channel: 0,
            level: 0.0,
            slope: Slope::Rising,
            holdoff: 0.0,
            qualify: Qualifier::None,
            zone: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Persistence {
    Off,
    Seconds(f64),
    Infinite,
}

impl Default for Persistence {
    fn default() -> Self { Self::Seconds(0.2) }
}

impl Persistence {
    /// The old preset lookup remains exact; continuous controls use Seconds directly.
    pub const ALL: [Self; 6] = [Self::Off, Self::Seconds(0.05), Self::Seconds(0.2), Self::Seconds(1.0), Self::Seconds(5.0), Self::Infinite];

    pub fn tau(self) -> f64 {
        match self { Self::Off => 0.0, Self::Seconds(seconds) => seconds, Self::Infinite => f64::INFINITY }
    }

    /// Finds an exact preset; does not round an arbitrary time to a preset.
    pub fn from_tau(tau: f64) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.tau() == tau)
    }
}

/// `Fft` opens a spectrum lens beside the time plot; both share the capture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Domain { #[default] Time, Fft }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FftSettings {
    pub ref_dbv: f64,
    pub db_per_div: f64,
    pub log_freq: bool,
    pub window: fab_elec_sim::fft::Window,
    pub waterfall: bool,
    /// The waterfall's share of the cell height, 0.15 to 0.75; defaults to one third.
    pub waterfall_share: f64,
    /// The spectrum column's share of the plot width, clamped to 0.2 through 0.6.
    pub cell_share: f64,
}

impl Default for FftSettings {
    fn default() -> Self { Self { ref_dbv: 10.0, db_per_div: 10.0, log_freq: true, window: fab_elec_sim::fft::Window::Hann, waterfall: true, waterfall_share: 1.0 / 3.0, cell_share: 0.38 } }
}

const FFT_WINDOWS: [fab_elec_sim::fft::Window; 4] = [fab_elec_sim::fft::Window::Hann, fab_elec_sim::fft::Window::BlackmanHarris, fab_elec_sim::fft::Window::FlatTop, fab_elec_sim::fft::Window::Rect];

impl FftSettings {
    fn window_label(self) -> &'static str {
        use fab_elec_sim::fft::Window::*;
        match self.window { Hann => "Hann", BlackmanHarris => "Blackman-Harris", FlatTop => "Flat top", Rect => "Rect", Hamming => "Hamming" }
    }

    /// Window row in popover order, with its labels and current choice; no text allocation.
    pub fn window_choices(self) -> [(&'static str, bool); 4] {
        FFT_WINDOWS.map(|window| (Self { window, ..self }.window_label(), self.window == window))
    }
}

/// Tile the plot with its time column, one-point divider, and optional spectrum column.
/// A collapsed lane keeps the time geometry and never opens a cell.
pub fn cell_layout(plot: Rect, lens: bool, collapsed: bool, share: f64) -> (Rect, Option<Rect>, Option<Rect>) {
    if !lens || collapsed { return (plot, None, None); }
    let width = plot.size.x.max(0.0);
    let divider = width.min(1.0);
    let share = if share.is_finite() { share.clamp(0.2, 0.6) } else { 0.38 };
    let cell = (width * share).min(width - divider);
    let time = Rect { size: dvec2(width - divider - cell, plot.size.y), ..plot };
    let divider = Rect { pos: plot.pos + dvec2(time.size.x, 0.0), size: dvec2(divider, plot.size.y) };
    let cell = Rect { pos: divider.pos + dvec2(divider.size.x, 0.0), size: dvec2(cell, plot.size.y) };
    (time, Some(divider), Some(cell))
}

/// The host applies these settings to acquisition on its worker.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopeSettings {
    pub channels: Vec<Channel>,
    /// None selects automatically; an explicit list retains the user's answer order.
    pub answers: Option<Vec<String>>,
    pub time_per_div: f64,
    pub domain: Domain,
    pub fft: FftSettings,
    pub trigger: Trigger,
    pub position: f64,
    pub persistence: Persistence,
    pub intensity: f64,
    pub running: bool,
    pub single: bool,
}

impl Default for ScopeSettings {
    fn default() -> Self {
        Self {
            channels: vec![Channel::default()],
            answers: None,
            time_per_div: 10e-6,
            domain: Domain::Time,
            fft: FftSettings::default(),
            trigger: Trigger::default(),
            position: 0.5,
            persistence: Persistence::Seconds(0.2),
            intensity: 1000.0,
            running: true,
            single: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ScopeAction {
    /// Discard the old accumulation before applying the following settings.
    Clear,
    Reconfigure(ScopeSettings),
    Close,
    TriggerLane,
    /// The source index (in set_sources order) now shown as the full picture.
    SwapSources { primary: usize },
    DragTag { abs: DVec2, channel: Option<usize> },
    DragTrigger { abs: DVec2, grab: f64 },
    /// The expanded lane height in points; release restores animated stack reflow.
    Resize { height: f64, done: bool },
    /// One shared time bracket, in ordered trigger-relative seconds, or its removal.
    Bracket(Option<(f64, f64)>),
    /// The bench owns cross-lane hit testing, preview and retained delay brackets.
    Delay(DelayGesture),
    Pin { name: &'static str, on: bool },
    StopOnFail { expect: String, on: bool },
    #[default]
    None,
}

pub(super) const DEFAULT_LANE_HEIGHT: f64 = 160.0;
pub(super) const MIN_LANE_HEIGHT: f64 = 56.0;
const RESIZE_SNAP: f64 = 16.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeOutcome { pub height: f64, pub collapse: bool }

/// Delta from the press, or from the last snap. A meter reopens at the signal minimum;
/// the handle rebases there so subsequent movement continues to change its height.
pub fn resize(current: f64, dy: f64, collapsed: bool) -> ResizeOutcome {
    if collapsed {
        let collapse = dy <= RESIZE_SNAP;
        ResizeOutcome { height: if collapse { current.max(MIN_LANE_HEIGHT) } else { MIN_LANE_HEIGHT }, collapse }
    } else {
        let height = current + dy;
        ResizeOutcome { height: height.max(MIN_LANE_HEIGHT), collapse: height < MIN_LANE_HEIGHT - RESIZE_SNAP }
    }
}

impl ScopeSettings {
    /// The first choice captures today's automatic order; later choices preserve it.
    pub fn toggle_answer(&mut self, name: &str, automatic: &[&str]) -> Vec<ScopeAction> {
        let Some(name) = answer_name(name) else { return Vec::new() };
        self.edit(false, |s| {
            let answers = s.answers.get_or_insert_with(|| automatic.iter().filter_map(|name| answer_name(name)).map(str::to_owned).collect());
            if answers.iter().any(|answer| answer == name) { answers.retain(|answer| answer != name); }
            else { answers.push(name.into()); }
        })
    }

    pub fn set_automatic_answers(&mut self) -> Vec<ScopeAction> { self.edit(false, |s| s.answers = None) }

    /// Worker axes omit display toggles and shares; only actual raster size changes resize acquisition.
    pub fn spectrum(&self) -> Option<FftSettings> { (self.domain == Domain::Fft).then_some(FftSettings { waterfall: true, waterfall_share: 1.0 / 3.0, cell_share: 0.38, ..self.fft }) }
    pub fn set_domain(&mut self, domain: Domain) -> Vec<ScopeAction> { self.edit(true, |s| s.domain = domain) }
    /// Continuous spectrum value mapping, committed together so the pointer's level stays fixed.
    pub fn set_fft_view(&mut self, reference: f64, db_per_div: f64) -> Vec<ScopeAction> {
        if !reference.is_finite() || !db_per_div.is_finite() || db_per_div <= 0.0 { return Vec::new(); }
        self.edit(true, |s| { s.fft.ref_dbv = reference; s.fft.db_per_div = db_per_div.clamp(0.1, 50.0); })
    }
    pub fn step_db(&mut self, steps: i32) -> Vec<ScopeAction> {
        self.set_fft_view(self.fft.ref_dbv, div_steps(self.fft.db_per_div, steps).clamp(0.1, 50.0))
    }
    pub fn step_ref(&mut self, steps: i32) -> Vec<ScopeAction> { self.set_fft_view(self.fft.ref_dbv + steps as f64 * self.fft.db_per_div, self.fft.db_per_div) }
    pub fn toggle_log(&mut self) -> Vec<ScopeAction> { self.edit(true, |s| s.fft.log_freq = !s.fft.log_freq) }
    pub fn set_window(&mut self, window: fab_elec_sim::fft::Window) -> Vec<ScopeAction> { self.edit(true, |s| s.fft.window = window) }
    pub fn cycle_window(&mut self, steps: i32) -> Vec<ScopeAction> {
        let i = FFT_WINDOWS.iter().position(|&w| w == self.fft.window).unwrap_or(0) as i64;
        self.set_window(FFT_WINDOWS[(i + i64::from(steps)).rem_euclid(4) as usize])
    }
    pub fn toggle_waterfall(&mut self) -> Vec<ScopeAction> { self.edit(false, |s| s.fft.waterfall = !s.fft.waterfall) }
    /// Resize only the displayed split, keeping acquisition axes and accumulated pictures.
    pub fn set_waterfall_share(&mut self, share: f64) -> Vec<ScopeAction> {
        if !share.is_finite() { return Vec::new(); }
        self.edit(false, |s| s.fft.waterfall_share = share.clamp(0.15, 0.75))
    }

    pub fn set_cell_share(&mut self, share: f64) -> Vec<ScopeAction> {
        if !share.is_finite() { return Vec::new(); }
        self.edit(false, |s| s.fft.cell_share = share.clamp(0.2, 0.6))
    }

    /// Continuous axes shared by pointer gestures and typed accessibility values.
    fn set_view(&mut self, window: Option<Window>, values: Option<(usize, f64, f64)>) -> bool {
        let mut changed = false;
        if let Some(window) = window.filter(|window| *window != self.window()) {
            let span = window.pre + window.post;
            if span.is_finite() && span > 0.0 && window.pre.is_finite() {
                let time = (span / 10.0).clamp(DIV_MIN, DIV_MAX);
                let position = (window.pre / span).clamp(0.0, 1.0);
                changed |= self.time_per_div != time || self.position != position;
                self.time_per_div = time;
                self.position = position;
            }
        }
        if let Some((channel, scale, offset)) = values {
            if scale.is_finite() && scale > 0.0 && offset.is_finite() {
                if let Some(c) = self.channels.get_mut(channel) {
                    let scale = scale.clamp(DIV_MIN, DIV_MAX);
                    let centre = offset * scale;
                    let (lo, hi) = (centre - 4.0 * scale, centre + 4.0 * scale);
                    if lo.is_finite() && hi.is_finite() && hi > lo {
                        changed |= c.volts_per_div != scale || c.offset_div != offset;
                        c.volts_per_div = scale;
                        c.offset_div = offset;
                    }
                }
            }
        }
        changed
    }

    /// Simulated seconds without an edge before an automatic sweep.
    pub fn auto_after(&self) -> Option<f64> {
        let span = 10.0 * self.time_per_div;
        (self.trigger.auto && !self.single).then_some((4.0 * span).max(10e-3))
    }

    pub fn window(&self) -> Window {
        let span = 10.0 * self.time_per_div;
        let pre = self.position * span;
        Window { pre, post: span - pre }
    }

    /// Maps the trigger channel; an empty channel list uses the default scale.
    pub fn map(&self, plot_px: (u32, u32)) -> PhosphorMap {
        self.map_channel(self.trigger.channel, plot_px)
            .unwrap_or_else(|| self.channel_map(&Channel::default(), plot_px))
    }

    pub fn map_channel(&self, channel: usize, plot_px: (u32, u32)) -> Option<PhosphorMap> {
        self.channels.get(channel).map(|c| self.channel_map(c, plot_px))
    }

    fn channel_map(&self, channel: &Channel, plot_px: (u32, u32)) -> PhosphorMap {
        let centre = channel.offset_div * channel.volts_per_div;
        let half = 4.0 * channel.volts_per_div;
        PhosphorMap::screen(plot_px.0, plot_px.1, self.window().pre, 10.0 * self.time_per_div, centre - half, centre + half)
    }

    pub fn trigger(&self) -> EdgeTrigger {
        let scale = self.channels.get(self.trigger.channel).map_or(1.0, |c| c.volts_per_div);
        EdgeTrigger {
            level: self.trigger.level,
            hyst: 0.02 * 8.0 * scale,
            slope: self.trigger.slope,
            holdoff: self.trigger.holdoff,
        }
    }

    pub fn look(&self, light: bool) -> PhosphorLook {
        PhosphorLook {
            tau: self.persistence.tau(),
            gain: self.intensity as f32,
            bloom: if light { 0.0 } else { 0.6 },
            light,
        }
    }

    /// Slider position in logarithmic gain space.
    pub fn intensity_fraction(&self) -> f64 {
        (self.intensity.log10() - 1.0) / 4.0
    }

    fn actions(&self, clear: bool) -> Vec<ScopeAction> {
        let mut actions = Vec::with_capacity(if clear { 2 } else { 1 });
        if clear {
            actions.push(ScopeAction::Clear);
        }
        actions.push(ScopeAction::Reconfigure(self.clone()));
        actions
    }

    fn edit(&mut self, clear: bool, edit: impl FnOnce(&mut Self)) -> Vec<ScopeAction> {
        let before = self.clone();
        edit(self);
        if *self == before {
            Vec::new()
        } else {
            self.actions(clear)
        }
    }

    pub fn set_channels(&mut self, channels: Vec<Channel>) -> Vec<ScopeAction> {
        if channels
            .iter()
            .any(|c| !c.volts_per_div.is_finite() || c.volts_per_div <= 0.0 || !c.offset_div.is_finite())
        {
            return Vec::new();
        }
        self.edit(true, |s| {
            s.channels = channels;
            for c in &mut s.channels {
                c.volts_per_div = div_steps(c.volts_per_div, 0);
            }
            s.trigger.channel = s.trigger.channel.min(s.channels.len().saturating_sub(1));
        })
    }

    pub fn set_scale(&mut self, channel: usize, scale: f64) -> Vec<ScopeAction> {
        if channel >= self.channels.len() || !scale.is_finite() || scale <= 0.0 {
            return Vec::new();
        }
        self.edit(true, |s| s.channels[channel].volts_per_div = div_steps(scale, 0))
    }

    pub fn step_scale(&mut self, channel: usize, steps: i32) -> Vec<ScopeAction> {
        let Some(c) = self.channels.get(channel) else { return Vec::new() };
        self.set_scale(channel, div_steps(c.volts_per_div, steps))
    }

    pub fn set_offset(&mut self, channel: usize, offset_div: f64) -> Vec<ScopeAction> {
        if channel >= self.channels.len() || !offset_div.is_finite() {
            return Vec::new();
        }
        self.edit(true, |s| s.channels[channel].offset_div = offset_div)
    }

    /// The offset that moves a channel's picture `dy` points down from the press.
    pub fn offset_dragged(&self, channel: usize, start_offset: f64, dy: f64, plot_height: f64) -> f64 {
        if channel >= self.channels.len() || !dy.is_finite() || !plot_height.is_finite() || plot_height <= 0.0 {
            return start_offset;
        }
        start_offset + 8.0 * dy / plot_height
    }

    pub fn set_channel_on(&mut self, channel: usize, on: bool) -> Vec<ScopeAction> {
        if channel >= self.channels.len() {
            return Vec::new();
        }
        self.edit(true, |s| s.channels[channel].on = on)
    }

    pub fn set_time_per_div(&mut self, time: f64) -> Vec<ScopeAction> {
        if !time.is_finite() || time <= 0.0 {
            return Vec::new();
        }
        self.edit(true, |s| s.time_per_div = div_steps(time, 0))
    }

    pub fn step_time(&mut self, steps: i32) -> Vec<ScopeAction> {
        self.set_time_per_div(div_steps(self.time_per_div, steps))
    }

    pub fn set_trigger_channel(&mut self, channel: usize) -> Vec<ScopeAction> {
        if channel >= self.channels.len() {
            return Vec::new();
        }
        self.edit(true, |s| s.trigger.channel = channel)
    }

    pub fn set_trigger_level(&mut self, level: f64) -> Vec<ScopeAction> {
        if !level.is_finite() {
            return Vec::new();
        }
        let level = self.runt_clamped_level(level);
        // The screen's axes are unchanged: the picture stays and the next waveforms grade over it.
        self.edit(false, |s| s.trigger.level = level)
    }

    pub fn set_trigger_slope(&mut self, slope: Slope) -> Vec<ScopeAction> {
        self.edit(false, |s| s.trigger.slope = slope)
    }

    pub fn set_trigger_qualifier(&mut self, qualifier: Qualifier) -> Vec<ScopeAction> {
        if !validate_qualifier(qualifier) { return Vec::new(); }
        if let Qualifier::Runt { other } = qualifier { if !validate_runt(&self.trigger(), other) { return Vec::new(); } }
        self.edit(false, |s| s.trigger.qualify = qualifier)
    }

    pub fn set_runt(&mut self, other: Option<f64>) -> Vec<ScopeAction> {
        self.set_trigger_qualifier(other.map_or(Qualifier::None, |other| Qualifier::Runt { other }))
    }

    /// A runt's second level keeps its side of the trigger level: the level stops a band short.
    pub fn runt_clamped_level(&self, level: f64) -> f64 {
        let Qualifier::Runt { other } = self.trigger.qualify else { return level };
        let hyst = self.trigger().hyst;
        if other > self.trigger.level { level.min((other - hyst).next_down()) } else { level.max((other + hyst).next_up()) }
    }

    fn clamp_runt_level(&self, value: f64) -> f64 {
        let Qualifier::Runt { other } = self.trigger.qualify else { return value };
        let trigger = self.trigger();
        // Validation is strict, so stop at the first representable value beyond the band.
        if other > trigger.level { value.max((trigger.level + trigger.hyst).next_up()) }
        else { value.min((trigger.level - trigger.hyst).next_down()) }
    }

    pub fn set_zone(&mut self, zone: Option<Zone>) -> Vec<ScopeAction> {
        if zone.is_some_and(|zone| !zone.valid()) { return Vec::new(); }
        self.edit(false, |s| s.trigger.zone = zone)
    }

    /// The pinned answer's current width, or reciprocal frequency, becomes the interval bound.
    pub fn trigger_from_answer(&mut self, quantity: TrendQuantity, value: f64, shorter: bool) -> Vec<ScopeAction> {
        if !value.is_finite() || value <= 0.0 { return Vec::new(); }
        let (interval, slope) = match quantity {
            TrendQuantity::Width => (value, Slope::Falling),
            TrendQuantity::Freq => (1.0 / value, Slope::Rising),
            _ => return Vec::new(),
        };
        if !interval.is_finite() || interval <= 0.0 { return Vec::new(); }
        let (min, max) = if shorter { (0.0, interval) } else { (interval, f64::INFINITY) };
        let qualifier = if quantity == TrendQuantity::Width { Qualifier::Width { min, max } } else { Qualifier::Period { min, max } };
        let mut actions = self.edit(false, |s| { s.trigger.qualify = qualifier; s.trigger.slope = slope; });
        if actions.is_empty() { actions.push(ScopeAction::Reconfigure(self.clone())); }
        actions.insert(0, ScopeAction::TriggerLane);
        actions
    }

    pub fn trigger_qualifier_label(&self) -> String {
        let (name, min, max) = match self.trigger.qualify {
            Qualifier::None => return String::new(),
            Qualifier::Width { min, max } => ("w", min, max),
            Qualifier::Period { min, max } => ("T", min, max),
            // The second level's side names the pulse direction.
            Qualifier::Runt { other } => return format!("runt {} {}", if other > self.trigger.level { "<" } else { ">" }, si_label(other, "V")),
        };
        if max == f64::INFINITY { format!("{name} > {}", si_label(min, "s")) }
        else if min == 0.0 { format!("{name} < {}", si_label(max, "s")) }
        else { format!("{} ≤ {name} < {}", si_label(min, "s"), si_label(max, "s")) }
    }

    pub fn set_trigger_auto(&mut self, auto: bool) -> Vec<ScopeAction> {
        self.edit(false, |s| s.trigger.auto = auto)
    }

    pub fn set_holdoff(&mut self, holdoff: f64) -> Vec<ScopeAction> {
        if !holdoff.is_finite() {
            return Vec::new();
        }
        self.edit(false, |s| s.trigger.holdoff = holdoff.max(0.0))
    }

    pub fn set_position(&mut self, position: f64) -> Vec<ScopeAction> {
        if !position.is_finite() {
            return Vec::new();
        }
        self.edit(true, |s| s.position = position.clamp(0.0, 1.0))
    }

    pub fn set_persistence(&mut self, persistence: Persistence) -> Vec<ScopeAction> {
        if matches!(persistence, Persistence::Seconds(t) if !t.is_finite() || t <= 0.0) { return Vec::new(); }
        self.edit(false, |s| s.persistence = persistence)
    }

    pub fn set_intensity(&mut self, intensity: f64) -> Vec<ScopeAction> {
        if !intensity.is_finite() {
            return Vec::new();
        }
        self.edit(false, |s| s.intensity = intensity.clamp(INTENSITY_MIN, INTENSITY_MAX))
    }

    pub fn set_intensity_fraction(&mut self, fraction: f64) -> Vec<ScopeAction> {
        if !fraction.is_finite() {
            return Vec::new();
        }
        self.set_intensity(10.0_f64.powf(1.0 + 4.0 * fraction.clamp(0.0, 1.0)))
    }

    pub fn set_running(&mut self, running: bool) -> Vec<ScopeAction> {
        self.edit(false, |s| {
            s.running = running;
            s.single = false;
        })
    }

    /// Arming starts a fresh acquisition, including when already armed.
    pub fn set_single(&mut self, single: bool) -> Vec<ScopeAction> {
        if single {
            self.single = true;
            self.running = true;
            self.actions(true)
        } else {
            self.edit(false, |s| s.single = false)
        }
    }
}

/// One answer of one lane: a named quantity, how far it spreads over the events measured, and the
/// finest digit worth showing. Values are in `unit` (duty in percent).
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    /// Zero-based lane (channel) index.
    pub lane: usize,
    /// "frequency", "duty", "Vpp", "rise", ... as the rows are named today.
    pub name: String,
    pub value: f64,
    pub unit: String,
    /// Population deviation over the events behind `value`; 0 for one event or an exact value.
    pub sigma: f64,
    /// Events behind `value` (cycles, pulses, transitions or samples); 1 when not counted.
    pub n: u64,
    /// The source's resolution for this quantity in `unit` (an amplitude step, a sample period for a
    /// time); 0 when exact (the simulator).
    pub step: f64,
}
impl Measurement {
    /// A row without spread (n = 1, sigma = 0, step = 0): what every tuple row was.
    pub fn exact(lane: usize, name: impl Into<String>, value: f64, unit: impl Into<String>) -> Self {
        Self { lane, name: name.into(), value, unit: unit.into(), sigma: 0.0, n: 1, step: 0.0 }
    }

    /// Apply a resolved channel's amplitude and sample resolution in this row's units.
    pub fn set_resolution(&mut self, resolved: &Resolved, channel: ChanId) {
        let dt = if resolved.rate > 0.0 { 1.0 / resolved.rate } else { 0.0 };
        self.step = match self.unit.as_str() {
            "V" | "A" => resolved.channels.iter().find(|c| c.id == channel).map_or(0.0, |c| c.step),
            "s" => dt,
            "Hz" if matches!(self.name.as_str(), "freq" | "frequency") => self.value * self.value * dt,
            _ => 0.0,
        };
    }
}

fn answer_scale(m: &Measurement) -> f64 {
    if m.value == 0.0 || !m.value.is_finite() || matches!(m.unit.as_str(), "%" | "dBV") { return 1.0; }
    let mut group = ((m.value.abs().log10() / 3.0).floor() as i32).clamp(-10, 10);
    if m.value.abs() / 10.0_f64.powi(group * 3) >= 999.95 && group < 10 { group += 1; }
    10.0_f64.powi(group * 3)
}

fn bright_end(m: &Measurement, value: &str, scale: f64) -> usize {
    let uncertainty = (m.sigma / (m.n.max(1) as f64).sqrt()).max(m.step / 2.0) / scale;
    if uncertainty <= 0.0 { return value.len(); }
    // Keep the decade containing the uncertainty (16 mV keeps hundredths of a volt).
    let place = uncertainty.log10().floor();
    let decimal = value.find('.').unwrap_or(value.len());
    let integers = value[..decimal].bytes().filter(|c| c.is_ascii_digit()).count() as i32;
    let mut digit = 0;
    for (at, ch) in value.char_indices() {
        if ch.is_ascii_digit() {
            if ((integers - digit - 1) as f64) < place { return at; }
            digit += 1;
        }
    }
    value.len()
}

/// Split the existing SI-formatted number (without its unit) into bright and dim digits.
/// Digits finer than max(sigma / sqrt(n), step / 2) are dim; exact answers stay wholly bright.
pub fn answer_digits(m: &Measurement) -> (String, String) {
    let label = si_label(m.value, &m.unit);
    let value = label.split_once(' ').map_or(label.as_str(), |(value, _)| value);
    let at = bright_end(m, value, answer_scale(m));
    (value[..at].into(), value[at..].into())
}

/// Population spread and event count for the answers popover; source resolution stays explicit.
pub fn answer_stats_text(m: &Measurement) -> String {
    let mut text = if m.n == 1 || m.sigma == 0.0 && m.step == 0.0 { "exact".into() } else {
        let place = 10.0_f64.powf(m.sigma.abs().log10().floor() - 1.0);
        let sigma = if place > 0.0 && place.is_finite() { (m.sigma / place).round() * place } else { m.sigma };
        let mut text = format!("sigma {}   n ", si_label(sigma, &m.unit));
        let digits = m.n.to_string();
        for (i, c) in digits.chars().enumerate() { if i > 0 && (digits.len() - i) % 3 == 0 { text.push('\u{2009}'); } text.push(c); }
        text
    };
    if m.step > 0.0 { text.push_str(&format!("   step {}", si_label(m.step, &m.unit))); }
    text
}

/// A bench expectation for one measurement of a net, independent of the bench language.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneExpect {
    /// The bench's name for this expectation.
    pub name: String,
    /// The lowered net name used to identify a signal lane.
    pub net: String,
    /// Measurement name as used by lanes: `freq`, `duty`, `Vpp`, `rise`, `mean`, ...
    pub answer: String,
    /// Inclusive lower bound in the measurement's unit (duty in percent), if present.
    pub min: Option<f64>,
    /// Inclusive upper bound in the measurement's unit (duty in percent), if present.
    pub max: Option<f64>,
}

/// Judge the displayed measurement, inclusively, and format its bounds in the answer's unit.
/// Missing bounds and non-finite values or bounds cannot produce a verdict.
pub fn expect_mark(value: f64, unit: &str, min: Option<f64>, max: Option<f64>) -> Option<(bool, String)> {
    if !value.is_finite() || min.is_some_and(|v| !v.is_finite()) || max.is_some_and(|v| !v.is_finite()) { return None; }
    let bound = match (min, max) {
        (Some(min), Some(max)) => format!("{} to {}", si_label(min, unit), si_label(max, unit)),
        (Some(min), None) => format!("> {}", si_label(min, unit)),
        (None, Some(max)) => format!("< {}", si_label(max, unit)),
        (None, None) => return None,
    };
    let pass = min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max);
    Some((pass, format!("{} ({bound})", if pass { "ok" } else { "fails" })))
}

/// What an eye-mode screen marks, in the trigger channel's units and seconds after the trigger. The
/// trigger fires on a crossing, so crossings sit at whole unit intervals and eye centres halfway between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeMarks {
    pub ui: f64,
    pub top: f64,
    pub bottom: f64,
    pub threshold: f64,
    pub jitter_pp: f64,
}

/// A selected answer keeps its value and unit separate for the channel/secondary text colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub name: String, pub value: String, pub unit: String,
    /// Byte ranges of uncertain digits in value; the complete string still determines layout.
    pub dim: Vec<Range<usize>>,
    /// Cached expectation verdicts and bound text, in bench order; empty without a judgement.
    pub marks: Vec<(bool, String)>,
}

impl Answer {
    fn failing(&self) -> bool { self.marks.iter().any(|(pass, _)| !*pass) }
}

/// The lane's selected answers and their accessible description.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answers { pub items: Vec<Answer>, pub text: String }

impl Answers {
    fn append(&mut self, mut other: Self) {
        if other.items.is_empty() && other.text.is_empty() { return; }
        if other.items.is_empty() && !other.text.is_empty() && !self.items.is_empty() {
            other.items.push(Answer { name: other.text.clone(), value: String::new(), unit: String::new(), dim: Vec::new(), marks: Vec::new() });
        }
        if self.items.is_empty() { self.text.clear(); }
        if !self.text.is_empty() && !other.text.is_empty() { self.text.push_str("   "); }
        self.text.push_str(&other.text);
        self.items.append(&mut other.items);
    }

    /// Append the time gate after every spectral answer, retaining an empty-spectrum note.
    pub fn append_gate(&mut self, duration: f64) {
        if self.items.is_empty() && !self.text.is_empty() {
            self.items.push(Answer { name: self.text.clone(), value: String::new(), unit: String::new(), dim: Vec::new(), marks: Vec::new() });
        }
        let label = si_label(duration, "s");
        let (value, unit) = label.split_once(' ').unwrap_or((&label, ""));
        if !self.text.is_empty() { self.text.push_str("   "); }
        self.text.push_str("gated "); self.text.push_str(&label);
        self.items.push(Answer { name: "gated ".into(), value: value.into(), unit: unit.into(), dim: Vec::new(), marks: Vec::new() });
    }
}

/// The space between two answers on the head line.
const ANSWER_GAP: f64 = 16.0;
const MORE_PAD: f64 = 6.0;

/// Answer placement uses the lane's own width, in points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerWidthClass {
    /// At least 900 points: all fitting answers, then a plain count.
    Wide,
    /// From 520 to 900 points: at most three answers, then a chip.
    Medium,
    /// Below 520 points: overflow answers wrap beneath the plot.
    Narrow,
}

impl AnswerWidthClass {
    /// Classifies a lane without rounding at either breakpoint.
    pub fn from_width(width: f64) -> Self {
        if width >= 900.0 { Self::Wide } else if width >= 520.0 { Self::Medium } else { Self::Narrow }
    }
}

/// Whole answers in source order; every range indexes the input widths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnswerLayout {
    /// The first `head` answers occupy the head line.
    pub head: usize,
    /// Answers represented by the count after the head line.
    pub more: usize,
    /// Whether that count is an interactive chip.
    pub chip: bool,
    /// Extra rows beneath the plot, with no omitted or repeated answers.
    pub rows: Vec<Range<usize>>,
}

/// Packs whole answers into `room`, including the 16-point gaps. `more_width` is the measured
/// unpadded count text width (the widest possible count); chip padding is reserved here. Wrapped
/// rows use the same room as the head line, allowing only a single oversized answer to exceed it.
pub fn layout_answers(widths: &[f64], room: f64, class: AnswerWidthClass, collapsed: bool, more_width: f64) -> AnswerLayout {
    let room = room.max(0.0);
    let wrap = class == AnswerWidthClass::Narrow && !collapsed;
    let chip = collapsed || class == AnswerWidthClass::Medium;
    let cap = if class == AnswerWidthClass::Medium && !collapsed { widths.len().min(3) } else { widths.len() };
    let mut layout = AnswerLayout::default();
    let mut used = 0.0;
    for (i, width) in widths.iter().take(cap).enumerate() {
        if i > 0 { used += ANSWER_GAP; }
        used += width;
        if used > room { break; }
        let more = if !wrap && i + 1 < widths.len() { ANSWER_GAP + more_width + if chip { MORE_PAD * 2.0 } else { 0.0 } } else { 0.0 };
        if used + more <= room { layout.head = i + 1; }
    }
    if wrap {
        let mut start = layout.head;
        while start < widths.len() {
            let mut end = start + 1;
            let mut used = widths[start];
            while end < widths.len() && used + ANSWER_GAP + widths[end] <= room {
                used += ANSWER_GAP + widths[end];
                end += 1;
            }
            layout.rows.push(start..end);
            start = end;
        }
    } else {
        layout.more = widths.len() - layout.head;
        layout.chip = chip && layout.more > 0;
    }
    layout
}

/// Current number and ghost suffix on the current answer's SI scale and precision. Trim only
/// zeroes shared by both, so a 10 kHz answer can compare as 10.00 kHz (was 9.98).
fn ghost_answer(now: &Measurement, was: &Measurement) -> Option<(String, String)> {
    if now.lane != was.lane || now.name != was.name || now.unit != was.unit || !now.value.is_finite() || !was.value.is_finite() { return None; }
    let scale = answer_scale(now);
    let scaled = now.value.abs() / scale;
    let decimals = if scaled == 0.0 { 0 } else { (3.0 - scaled.log10().floor()).clamp(0.0, 15.0) as usize };
    let number = |value: f64| {
        let mut text = format!("{:.*}", decimals, (value / scale).abs());
        if value < 0.0 && text.bytes().any(|b| matches!(b, b'1'..=b'9')) { text.insert(0, '−'); }
        text
    };
    let (mut a, mut b) = (number(now.value), number(was.value));
    if a == b { return None; }
    if decimals > 0 {
        while a.ends_with('0') && b.ends_with('0') { a.pop(); b.pop(); }
        if a.ends_with('.') { a.pop(); b.pop(); }
    }
    Some((a, format!(" (was {b})")))
}

/// A differing ghost's value, without repeating the unit; unrelated or equal shown rows omit it.
pub fn ghost_suffix(now: &Measurement, was: &Measurement) -> Option<String> {
    ghost_answer(now, was).map(|(_, suffix)| suffix)
}

/// Select only the lane's rows; this runs on measurement changes, never per sample or per frame.
pub fn lane_answers(rows: &[Measurement], ghost_rows: &[Measurement], lane: usize, domain: Domain, meter: bool, normal: bool, scrubbed: bool) -> Answers {
    let mut answers = lane_main_answers(rows, ghost_rows, lane, Domain::Time, meter, normal, scrubbed);
    if domain == Domain::Fft && !meter { answers.append(lane_main_answers(rows, ghost_rows, lane, Domain::Fft, false, false, false)); }
    append_held(&mut answers, rows, lane);
    answers
}

fn append_held(answers: &mut Answers, rows: &[Measurement], lane: usize) {
    // A window that repeats a steady source's held values says so: nothing is changing.
    if !answers.items.is_empty() && rows.iter().any(|r| r.lane == lane && r.name == "held") {
        answers.text.push_str("   held");
        answers.items.push(Answer { name: "held".into(), value: String::new(), unit: String::new(), dim: Vec::new(), marks: Vec::new() });
    }
}

fn lane_answer_names(rows: &[Measurement], lane: usize, domain: Domain, meter: bool) -> Vec<&'static str> {
    let has = |name| rows.iter().any(|r| r.lane == lane && r.name == name);
    if domain == Domain::Fft && !meter {
        // The largest harmonic joins the FFT answers; keep its identity in the same order.
        let harmonic = rows.iter().filter(|r| r.lane == lane && matches!(r.name.as_str(), "H2" | "H3" | "H4" | "H5") && r.unit == "dBc")
            .max_by(|a, b| a.value.total_cmp(&b.value)).map_or("", |r| match r.name.as_str() { "H2" => "H2", "H3" => "H3", "H4" => "H4", "H5" => "H5", _ => "" });
        ["peak f", "peak", harmonic, "2nd f", "2nd"].into_iter().filter(|name| !name.is_empty()).collect()
    }
    else if !meter && has("freq") && has("duty") { vec!["freq", "duty", "Vpp", "rise"] }
    else { vec!["mean", "Vpp"] }
}

fn lane_main_answers(rows: &[Measurement], ghost_rows: &[Measurement], lane: usize, domain: Domain, meter: bool, normal: bool, scrubbed: bool) -> Answers {
    let row = |name: &str| rows.iter().find(|r| r.lane == lane && r.name == name);
    if domain == Domain::Fft && !meter && row("gate too short").is_some() {
        return Answers { items: Vec::new(), text: "gate too short".into() };
    }
    let names = lane_answer_names(rows, lane, domain, meter);
    let mut answers = lane_named_answers(rows, ghost_rows, lane, &names, names.first() == Some(&"mean"));
    if answers.items.is_empty() {
        answers.text.push_str(if scrubbed { "no samples at this instant" } else if normal { "waiting for a trigger" } else { "" });
    }
    answers
}

fn lane_named_answers(rows: &[Measurement], ghost_rows: &[Measurement], lane: usize, names: &[&str], ripple: bool) -> Answers {
    let row = |name: &str| rows.iter().find(|r| r.lane == lane && r.name == name && r.value.is_finite());
    let mut answers = Answers::default();
    for name in names {
        if name.is_empty() { continue; }
        let Some(m) = row(name).or_else(|| (*name == "Vpp").then(|| row("Ipp")).flatten()) else { continue };
        let label = if m.unit == "dBc" { format!("{:.1} dBc", m.value) } else { si_label(m.value, &m.unit) };
        let (value, unit) = label.split_once(' ').unwrap_or((&label, ""));
        let mut value = value.to_owned();
        let (prefix, mut suffix) = match *name {
            "duty" => ("", "% high".to_owned()),
            "Vpp" => (if ripple { "ripple " } else { "" }, format!("{unit} pp")),
            "rise" => ("rise ", unit.to_owned()),
            "width" => ("width ", unit.to_owned()),
            "top" => ("top ", unit.to_owned()),
            "base" => ("base ", unit.to_owned()),
            "min" => ("min ", unit.to_owned()),
            "max" => ("max ", unit.to_owned()),
            "overshoot" => ("overshoot ", unit.to_owned()),
            "RBW" => ("RBW ", unit.to_owned()),
            "span" => ("span ", unit.to_owned()),
            "H2" => ("H2 ", unit.to_owned()),
            "H3" => ("H3 ", unit.to_owned()),
            "H4" => ("H4 ", unit.to_owned()),
            "H5" => ("H5 ", unit.to_owned()),
            "2nd f" => ("2nd ", unit.to_owned()),
            _ => ("", unit.to_owned()),
        };
        if let Some(was) = ghost_rows.iter().find(|r| r.lane == lane && r.name == m.name && r.unit == m.unit) {
            if let Some((current, ghost)) = ghost_answer(m, was) {
                value = current;
                suffix.push_str(&ghost);
            }
        }
        if !answers.text.is_empty() { answers.text.push_str("   "); }
        answers.text.push_str(prefix);
        answers.text.push_str(&value);
        answers.text.push('\u{2009}');
        answers.text.push_str(&suffix);
        // Harmonic levels are exact readings of the transform: no uncertain digits.
        let at = if m.unit == "dBc" { value.len() } else { bright_end(m, &value, answer_scale(m)) };
        let dim = if at == value.len() { Vec::new() } else { vec![at..value.len()] };
        answers.items.push(Answer { name: prefix.into(), value, unit: suffix, dim, marks: Vec::new() });
    }
    answers
}

/// Source identity for a lane; order at installation is primary, then optional secondary.
/// A channel names an overlaid net's hue; None keeps the same-net twin presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneSource { pub side: Side, pub name: String, pub channel: Option<usize> }

fn side_word(side: Side) -> &'static str {
    match side { Side::Sim => "sim", Side::Real => "real", Side::Replay => "replay" }
}

/// Compare the common frequency, duty, peak-to-peak level and rise measurements of one net.
/// Ratios use signed percent relative to secondary; levels and times use signed base-unit
/// differences with SI prefixes. Missing, nonfinite or mismatched-unit pairs are omitted;
/// a zero ratio reference has no defined difference. Each comparison stays one layout item.
pub fn twin_answers(a: &[Measurement], b: &[Measurement], primary: Side, secondary: Side) -> Answers {
    twin_named_answers(a, b, primary, secondary, &["freq", "duty", "Vpp", "rise"])
}

fn twin_named_answers(a: &[Measurement], b: &[Measurement], primary: Side, secondary: Side, names: &[&str]) -> Answers {
    let mut answers = Answers::default();
    for &name in names {
        let find = |rows: &[Measurement]| rows.iter().position(|r| r.lane == 0 && (r.name == name || name == "Vpp" && r.name == "Ipp"));
        let (Some(ai), Some(bi)) = (find(a), find(b)) else { continue };
        let (a, b) = (&a[ai], &b[bi]);
        if a.name != b.name || a.unit != b.unit || !a.value.is_finite() || !b.value.is_finite() { continue; }
        let ratio = matches!(name, "freq" | "duty");
        let delta = if ratio { (a.value - b.value) / b.value.abs() * 100.0 } else { a.value - b.value };
        // Four significant digits on a shared scale keep small differences readable (10.02/10.00).
        let scale = if matches!(a.unit.as_str(), "%" | "dBV") { 1.0 } else {
            10.0_f64.powi(((a.value.abs().max(b.value.abs()).max(f64::MIN_POSITIVE).log10() / 3.0).floor() as i32).clamp(-10, 10) * 3)
        };
        let largest = (a.value.abs().max(b.value.abs()) / scale).max(f64::MIN_POSITIVE);
        let decimals = (3.0 - largest.log10().floor()).clamp(0.0, 6.0) as usize;
        // The difference is no finer than the values shown: rounded to their last digit (as a
        // fraction of the reference for a ratio), and a difference below it reads as none.
        let shown = scale * 10.0_f64.powi(-(decimals as i32));
        let step = if ratio { shown / b.value.abs() * 100.0 } else { shown };
        let difference = if !delta.is_finite() || !step.is_finite() { "—".into() } else {
            let rounded = (delta / step).round() * step;
            let sign = if rounded < 0.0 { "−" } else { "+" };
            if rounded == 0.0 { format!("±0 {}", if ratio { "%" } else { a.unit.as_str() }) }
            else if ratio {
                let places = (-step.log10().floor()).clamp(0.0, 3.0) as usize;
                let text = format!("{:.*}", places, rounded.abs());
                let text = if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.') } else { text.as_str() };
                format!("{sign}{text} %")
            } else { format!("{sign}{}", si_label(rounded.abs(), &a.unit)) }
        };
        let unit_label = si_label(scale, &a.unit);
        let unit = unit_label.split_once(' ').map_or(a.unit.as_str(), |(_, unit)| unit);
        let prefix = match name { "freq" => String::new(), "Vpp" => "pp ".into(), _ => format!("{name} ") };
        let av = format!("{:.*}", decimals, a.value / scale);
        let bv = format!("{:.*}", decimals, b.value / scale);
        let mut value = format!("{av} {unit} {}   ", side_word(primary));
        let mut dim = Vec::new();
        let at = bright_end(a, &av, scale);
        if at < av.len() { dim.push(at..av.len()); }
        let at = bright_end(b, &bv, scale);
        if at < bv.len() { dim.push(value.len() + at..value.len() + bv.len()); }
        value.push_str(&format!("{bv} {}   {difference}", side_word(secondary)));
        if !answers.text.is_empty() { answers.text.push_str("   "); }
        answers.text.push_str(&prefix); answers.text.push_str(&value);
        answers.items.push(Answer { name: prefix.into(), value, unit: String::new(), dim, marks: Vec::new() });
    }
    answers
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part { Tag, Secondary, Plot(usize), Scale(usize), Offset(usize), Time, Domain, TriggerChannel, Collapse, Resize, Slope, Qualifier, Zone, LevelMarker, RuntLevel, PositionMarker, Close, More, AddAnswer, Spectrum, Divider, CellDivider, SpectrumScale, SpectrumOffset, Frequency, Holdoff, Answer(usize) }

impl Part {
    fn key(self) -> u64 {
        match self {
            Self::Plot(i) => 200 + i as u64,
            Self::Answer(i) => 300 + i as u64,
            Self::Holdoff => 14,
            Self::AddAnswer => 30,
            Self::Resize => 31,
            Self::RuntLevel => 32,
            Self::Tag => 1, Self::Secondary => 11, Self::Scale(i) => 100 + i as u64 * 2, Self::Offset(i) => 101 + i as u64 * 2,
            Self::Time => 2, Self::Domain => 3, Self::TriggerChannel => 4, Self::Collapse => 5,
            Self::Zone => 20, Self::Qualifier => 19, Self::Slope => 6, Self::LevelMarker => 7, Self::PositionMarker => 8, Self::Close => 9, Self::More => 10, Self::Spectrum => 12, Self::Divider => 13, Self::CellDivider => 15, Self::SpectrumScale => 16, Self::SpectrumOffset => 17, Self::Frequency => 18,
        }
    }
    fn glyph(self) -> bool { matches!(self, Self::Secondary | Self::Domain | Self::TriggerChannel | Self::Collapse | Self::Close | Self::Slope | Self::Qualifier | Self::Zone | Self::More | Self::AddAnswer | Self::Spectrum) }
}

pub(crate) struct LabelLayout {
    pub(crate) width: f64,
    full: Rc<LaidoutText>,
    fitted: Vec<(f64, Rc<LaidoutText>, bool)>,
    used: bool,
}

impl LabelLayout {
    pub(super) fn fit(&mut self, cx: &mut Cx2d, draw: &mut DrawText, width: f64) -> Rc<LaidoutText> {
        if self.width <= width {
            return self.full.clone();
        }
        if let Some((_, layout, used)) = self.fitted.iter_mut().find(|(cached_width, _, _)| *cached_width == width) {
            *used = true;
            return layout.clone();
        }
        let overflow = draw.text_overflow;
        let max_lines = draw.max_lines;
        draw.text_overflow = TextOverflow::Ellipsis;
        draw.max_lines = 1;
        let layout = draw.layout(
            cx,
            0.0,
            0.0,
            Some((width / f64::from(draw.font_scale)) as f32),
            false,
            Align::default(),
            self.full.text.as_str(),
        );
        draw.text_overflow = overflow;
        draw.max_lines = max_lines;
        self.fitted.push((width, layout.clone(), true));
        layout
    }
}

#[derive(Default)]
pub(crate) struct LabelCache {
    font: Option<(BaseTheme, FontFamily, [u32; 3])>,
    labels: HashMap<String, LabelLayout>,
}

impl LabelCache {
    pub(crate) fn begin_frame(&mut self, draw: &DrawText, theme: BaseTheme) -> bool {
        let font = (
            theme,
            draw.text_style.font_family.clone(),
            [
                draw.text_style.font_size.to_bits(),
                draw.font_scale.to_bits(),
                draw.text_style.line_spacing.to_bits(),
            ],
        );
        let changed = self.font.as_ref() != Some(&font);
        if changed {
            self.labels.clear();
            self.font = Some(font);
        }
        self.labels.retain(|_, label| {
            let keep = label.used && !label.full.has_missing_glyph;
            label.used = false;
            label.fitted.retain_mut(|(_, layout, used)| {
                let keep = *used && !layout.has_missing_glyph;
                *used = false;
                keep
            });
            keep
        });
        changed
    }

    pub(crate) fn layout(&mut self, cx: &mut Cx2d, draw: &DrawText, text: &str) -> Rc<LaidoutText> {
        self.get(cx, draw, text).full.clone()
    }

    pub(crate) fn get(&mut self, cx: &mut Cx2d, draw: &DrawText, text: &str) -> &mut LabelLayout {
        if !self.labels.contains_key(text) {
            let full = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
            self.labels.insert(
                text.into(),
                LabelLayout {
                    width: f64::from(full.size_in_lpxs.width * draw.font_scale),
                    full,
                    fitted: Vec::new(),
                    used: true,
                },
            );
        }
        let label = self.labels.get_mut(text).unwrap();
        label.used = true;
        label
    }
}

struct ValueAxis {
    ticks: Vec<Tick>,
    lo: f64,
    hi: f64,
    unit: String,
}

struct PartRect {
    part: Part,
    rect: Rect,
    selected: bool,
}

#[derive(Clone, Copy)]
struct Drag {
    part: Part,
    grab: f64,
    start: f64,
    // The divider waits for slop and keeps the available height captured at the press.
    pending: Option<DVec2>,
    extent: f64,
}

#[derive(Clone, Copy)]
struct AnswerDrag {
    index: usize,
    name: &'static str,
    press: DVec2,
    started: bool,
    outside: bool,
}

const ANSWER_DRAG: LiveId = live_id!(elec_answer);

pub(super) const PAD: f64 = 8.0;
pub(super) const GAP: f64 = 4.0;
pub(super) const ROW: f64 = 26.0;
const MARKER: f64 = 20.0;
// A 24-point wheel notch changes the span by ten percent; trackpads stay continuous.
const WHEEL_ZOOM: f64 = 0.004390021485742762; // -ln(0.9) / 24
const DRAG_ZOOM: f64 = 120.0;

#[derive(Clone, Copy)]
struct ViewDrag {
    press: DVec2,
    last: DVec2,
    window: Window,
    values: (f64, f64),
    at: f64,
    axis: Option<Axis>,
    free: bool,
}

/// A signal lane, or its collapsed meter face; acquisition stays with the stack host.
/// A lane answer's measurement name. An alias, because the Script derive's field parser does not
/// count a `<` written straight before `&` as an opening bracket (`Vec<Option<&'static str>>`).
type AnswerName = Option<&'static str>;

#[derive(Script, WidgetRegister, WidgetRef, WidgetSet)]
pub struct SignalLane {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live(false)]
    closable: bool,
    #[live]
    phosphor: PhosphorView,
    #[live]
    spectrum_view: PhosphorView,
    #[live]
    spark: NetSpark,
    #[live(false)]
    shared_time: bool,
    #[live]
    draw_shape: DrawColor,
    #[live]
    draw_more: DrawColor,
    #[live]
    draw_tag: DrawProbeTag,
    #[rust]
    tag_ghost: bool,
    /// One retained child, made from this template so its labels enter the widget tree.
    #[live]
    answers_popover: ScriptObjectRef,
    #[live]
    answer_row: ScriptObjectRef,
    #[live]
    answer_tip: ScriptObjectRef,
    #[rust]
    answer_tip_widget: Option<WidgetRef>,
    #[rust]
    answer_drag: Option<AnswerDrag>,
    #[rust]
    answers_popover_widget: Option<WidgetRef>,
    #[rust]
    answers_popover_key: Option<(u64, bool, usize)>,
    #[rust]
    answers_anchor: Option<Rect>,
    #[rust]
    answers_anchor_part: Option<Part>,
    #[live]
    measure_popover: ScriptObjectRef,
    #[live]
    measure_row: ScriptObjectRef,
    #[rust]
    measure_popover_widget: Option<WidgetRef>,
    #[rust]
    measure_popover_key: Option<(u64, bool, usize)>,
    #[rust]
    measure_anchor: Option<Rect>,
    #[rust]
    measure_revision: u64,
    #[live]
    value_popover: ScriptObjectRef,
    #[rust]
    value_popover_widget: Option<WidgetRef>,
    #[rust]
    value_entry_part: Option<Part>,
    #[rust]
    value_anchor: Option<Rect>,
    #[live]
    spectrum_popover: ScriptObjectRef,
    #[rust]
    spectrum_popover_widget: Option<WidgetRef>,
    #[rust]
    spectrum_popover_key: Option<FftSettings>,
    #[rust]
    spectrum_anchor: Option<Rect>,
    /// The spectrum raster's size at a divider press: kept while the drag stretches the picture.
    #[rust]
    divider_raster: Option<DVec2>,
    #[rust]
    cell_rasters: Option<(DVec2, DVec2)>,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_axis: DrawText,
    #[rust]
    axis_cache: LabelCache,
    #[rust]
    axis_mode: bool,
    #[rust]
    text_cache: LabelCache,
    #[rust]
    text_cache_draw: Option<u64>,
    #[rust]
    label_cache: LabelCache,
    #[rust]
    area: Area,
    #[rust]
    drawn_at: DVec2,
    #[rust]
    plot: Rect,
    #[rust]
    time_plot_drawn: bool,
    #[rust]
    spectrum_cell: Rect,
    #[rust]
    spectrum_plot: Rect,
    #[rust]
    time_area: Area,
    #[rust]
    settings: ScopeSettings,
    #[rust]
    measurements: Vec<Measurement>,
    #[rust]
    expects: Vec<LaneExpect>,
    #[rust]
    stop_on_fail: Vec<String>,
    #[rust]
    ghost_rows: Vec<Measurement>,
    #[rust]
    features: Option<Arc<CaptureFeatures>>,
    #[rust]
    feature_lane: usize,
    #[rust]
    answer_geometries: [Option<Geometry>; 4],
    #[rust]
    answer_names: Vec<AnswerName>,
    #[rust]
    pins: Vec<TrendQuantity>,
    #[rust]
    distributions: Vec<(TrendQuantity, Distribution)>,
    #[rust]
    distribution_ranges: Vec<(TrendQuantity, (f32, f32))>,
    #[rust]
    distribution_plots: Vec<(TrendQuantity, Histogram)>,
    #[rust]
    reticle_label: ReticleLabel,
    #[rust]
    zone_drag: Option<ZoneDrag>,
    #[rust]
    runt_preview: Option<f64>,
    #[rust]
    zone_lane: usize,
    #[rust]
    bracket: Option<(f64, f64)>,
    #[rust(true)]
    bracket_visible: bool,
    #[rust]
    span_stats: Option<SpanStats>,
    #[rust]
    span_answered: bool,
    #[rust]
    bracket_label: BracketLabel,
    /// The stationary end; ordering can change freely when the dragged end crosses it.
    #[rust]
    bracket_drag: Option<f64>,
    #[rust]
    bracket_off: bool,
    #[rust]
    bracket_press: Option<(f64, f64)>,
    #[rust]
    value_bracket: Option<(f64, f64)>,
    #[rust]
    value_bracket_drag: Option<f64>,
    #[rust]
    value_bracket_off: bool,
    /// The value bracket is being made by an alt + drag on the value axis: it is not pulled off.
    #[rust]
    value_bracket_axis: bool,
    #[rust]
    value_bracket_press: Option<(f64, f64)>,
    #[rust]
    bracket_remove_overlay: Option<DrawList2d>,
    #[rust]
    value_bracket_label: ValueBracketLabel,
    #[rust]
    delay_marks: Vec<DelayMark>,
    #[rust]
    delay_drag: bool,
    #[rust]
    delay_enabled: bool,
    #[rust]
    secondary_measurements: Vec<Measurement>,
    #[rust]
    spectrum_measurements: Option<SpectrumMeasurements>,
    #[rust]
    harmonic_answers: [Option<Answer>; 4],
    #[rust]
    harmonic_width: f64,
    #[rust]
    spectrum_hover: Option<usize>,
    #[rust]
    sources: Vec<LaneSource>,
    #[rust]
    source_labels: Vec<String>,
    #[rust]
    primary_source: usize,
    #[rust]
    sources_changed: bool,
    #[rust]
    overlay_preview: bool,
    #[rust]
    source_swap_pending: bool,
    #[rust]
    source_clear_pending: bool,
    #[rust]
    answers: Answers,
    #[rust]
    semantic_summary: Option<Rc<std::cell::RefCell<super::semantics::Summary>>>,
    /// Formatted with the answer snapshot, including changes that leave the headline unchanged.
    #[rust]
    answer_stats: Vec<String>,
    #[rust]
    answer_marks: Vec<String>,
    #[rust]
    unjudged_answers: Vec<String>,
    #[rust]
    more_labels: Vec<String>,
    #[rust]
    answers_at: Option<Instant>,
    #[rust]
    answers_timer: Timer,
    #[rust]
    answers_revision: u64,
    #[rust]
    answers_layout: AnswerLayout,
    #[rust]
    answer_widths: Vec<f64>,
    /// Layout slots retain the original answer identity when a carried answer is omitted.
    #[rust]
    answer_indices: Vec<usize>,
    #[rust]
    answers_layout_key: Option<(u64, f64, bool, bool, Option<(usize, bool)>)>,
    #[rust]
    answers_tag_width: f64,
    #[rust(true)]
    answers_spark: bool,
    #[rust(true)]
    answers_dirty: bool,
    #[rust]
    net: String,
    #[rust(true)]
    trigger_lane: bool,
    #[rust]
    collapsed: bool,
    #[rust(DEFAULT_LANE_HEIGHT)]
    height: f64,
    #[rust]
    tag_press: Option<(DVec2, usize)>,
    #[rust]
    trigger_tag: Rect,
    #[rust]
    trigger_press: Option<(DVec2, f64)>,
    #[rust]
    trigger_ghost: Option<f64>,
    #[rust]
    holdoff_area: Area,
    #[rust]
    holdoff_axis: Rect,
    #[rust]
    holdoff_grab: Option<f64>,
    #[rust]
    holdoff_note: String,
    #[rust]
    axes: Option<ValueAxis>,
    #[rust]
    spectrum_axes: Option<ValueAxis>,
    #[rust]
    spectrum_axes_key: Option<(FftSettings, f64, f64)>,
    #[rust]
    axes_key: Option<(PhosphorMap, Domain, FftSettings, f64, f64)>,
    #[rust]
    time_ticks: Vec<Tick>,
    #[rust]
    time_key: Option<(Window, f64, Option<FftSettings>, u32)>,
    #[rust]
    time_note: String,
    #[rust]
    frequency_ticks: Vec<Tick>,
    #[rust]
    frequency_key: Option<(Window, f64, Option<FftSettings>, u32)>,
    #[rust]
    frequency_note: String,
    #[rust]
    occupied: Vec<(f64, f64)>,
    #[rust]
    shared_ticks: Vec<Tick>,
    #[rust]
    shared_key: Option<(Window, f64, Option<FftSettings>, u32)>,
    #[rust]
    shared_note: String,
    #[rust]
    trigger_state: super::acquire::TriggerState,
    #[rust]
    measurement_note: Option<String>,
    #[rust]
    source_caps: Option<Arc<Caps>>,
    #[rust]
    source_resolved: Option<Arc<Resolved>>,
    #[rust]
    source_channel: Option<ChanId>,
    #[rust]
    source_steps: Vec<Answer>,
    /// The simulation's time resolution under Fast (`set_sim_res`), read out with the answers.
    #[rust]
    sim_res: Option<f64>,
    #[rust]
    trigger_note: String,
    #[rust]
    clipped: Clipped,
    #[rust]
    eye_marks: Option<EyeMarks>,
    #[rust]
    eye_height: String,
    #[rust]
    eye_width: String,
    #[rust]
    parts: Vec<PartRect>,
    #[rust]
    pointer: Option<DVec2>,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    view_drag: Option<ViewDrag>,
    #[rust]
    time_fit_period: Option<f64>,
    #[rust]
    focused: Option<Part>,
    #[rust(true)]
    dark: bool,
    #[rust]
    last_look: Option<PhosphorLook>,
}

impl ScriptHook for SignalLane {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            if let Some(widget) = &mut self.answers_popover_widget {
                widget.script_apply(vm, apply, scope, self.answers_popover.as_object().into());
            }
            if let Some(widget) = &self.answers_popover_widget {
                let content = widget.as_popover().content();
                if let Some(mut view) = content.borrow_mut::<View>() {
                    for (_, row) in &mut view.children { row.script_apply(vm, apply, scope, self.answer_row.as_object().into()); }
                };
            }
            self.answers_layout_key = None;
            self.answers_popover_key = None;
            if let Some(widget) = &mut self.measure_popover_widget {
                widget.script_apply(vm, apply, scope, self.measure_popover.as_object().into());
            }
            if let Some(widget) = &self.measure_popover_widget {
                let content = widget.as_popover().content();
                if let Some(mut view) = content.borrow_mut::<View>() {
                    for (_, row) in &mut view.children { row.script_apply(vm, apply, scope, self.measure_row.as_object().into()); }
                };
            }
            self.measure_popover_key = None;
            if let Some(widget) = &mut self.value_popover_widget {
                widget.script_apply(vm, apply, scope, self.value_popover.as_object().into());
            }
            if let Some(widget) = &mut self.spectrum_popover_widget {
                widget.script_apply(vm, apply, scope, self.spectrum_popover.as_object().into());
            }
            self.spectrum_popover_key = None;
        }
    }
}

impl SignalLane {
    /// The preview lane keeps the same tag geometry.
    pub(super) fn set_tag_ghost(&mut self, cx: &mut Cx, ghost: bool) {
        if self.tag_ghost != ghost { self.tag_ghost = ghost; self.area.redraw(cx); }
    }

    pub fn settings(&self) -> &ScopeSettings {
        &self.settings
    }

    /// Install the sources of this lane, primary first and optional secondary second. Call only
    /// when source identities change, not per frame. An empty list leaves the legacy probe tag.
    /// The caller redraws after installation; texture cleanup is deferred to the next draw/push.
    pub fn set_sources(&mut self, sources: &[LaneSource]) {
        assert!(sources.len() <= 2, "a signal lane shows at most two sources: twin sides or overlaid nets");
        if self.sources == sources { return; }
        self.ghost_rows.clear();
        self.spectrum_measurements = None;
        self.harmonic_answers = Default::default();
        self.spectrum_hover = None;
        // The bench's plain sim source and its labelled overlay source are the same net.
        let retained = sources.first().and_then(|next| self.sources.iter().position(|old| {
            if next == old { return true; }
            if next.side != old.side { return false; }
            match (next.channel, old.channel) {
                (Some(a), Some(b)) => a == b && next.name == old.name,
                (Some(channel), None) | (None, Some(channel)) => self.zone_lane == channel,
                _ => false,
            }
        }));
        if !self.sources.is_empty() && retained != Some(self.primary_source) {
            if self.sources.len() == 2 && retained == Some(1 - self.primary_source) {
                std::mem::swap(&mut self.measurements, &mut self.secondary_measurements);
                self.source_swap_pending = !self.source_swap_pending;
            } else {
                self.measurements.clear();
                self.source_clear_pending = true;
            }
        }
        self.sources = sources.to_vec();
        self.source_labels = sources.iter().map(|source| {
            if source.channel.is_some() { return source.name.clone(); }
            let side = side_word(source.side);
            if source.name == side { source.name.clone() } else { format!("{} {side}", source.name) }
        }).collect();
        self.primary_source = 0;
        self.secondary_measurements.clear();
        self.sources_changed = true;
        self.answers_dirty = true;
        self.answers_at = None;
        self.answers_layout_key = None;
    }

    /// Identities in their installation order, independent of primary selection.
    pub fn sources(&self) -> &[LaneSource] { &self.sources }
    /// Stable installation index currently drawn as the full picture.
    pub fn primary_source(&self) -> usize { self.primary_source }

    pub(super) fn overlay_pair(&self) -> bool { self.sources.len() == 2 && self.sources.iter().all(|source| source.channel.is_some()) }

    pub fn set_overlay_preview(&mut self, cx: &mut Cx, preview: bool) {
        if self.overlay_preview == preview { return; }
        self.overlay_preview = preview;
        self.phosphor.set_twin_preview(preview);
        self.spectrum_view.set_twin_preview(preview);
        self.area.redraw(cx);
    }

    pub(super) fn overlay_plot(&self, cx: &Cx) -> Option<Rect> {
        if self.collapsed || self.plot.size.x <= 0.0 || self.plot.size.y <= 0.0 { return None; }
        Some(Rect { pos: self.plot.pos + self.area.rect(cx).pos - self.drawn_at, ..self.plot })
    }

    pub(super) fn clear_source_picture(&mut self, cx: &mut Cx, source: usize) {
        self.sync_sources(cx);
        self.phosphor.clear_layer(cx, source != self.primary_source);
        self.spectrum_view.clear_layer(cx, source != self.primary_source);
    }

    pub(super) fn push_source_still(&mut self, cx: &mut Cx, source: usize, spectrum: bool, hits: Hits) -> Pushed {
        self.sync_sources(cx);
        if source == self.primary_source {
            return if spectrum { self.spectrum_view.push_still(cx, hits) } else { self.phosphor.push_still(cx, hits) };
        }
        if spectrum { self.push_spectrum_frame(cx, source, hits) } else { self.push_source_frame(cx, source, hits) }
    }

    fn sync_sources(&mut self, cx: &mut Cx) {
        if !self.sources_changed { return; }
        if self.source_swap_pending { self.phosphor.swap_twin(cx); self.spectrum_view.swap_twin(cx); }
        self.phosphor.clear_twin(cx);
        self.spectrum_view.clear_twin(cx);
        if self.sources.len() == 2 { self.phosphor.enable_twin(); self.spectrum_view.enable_twin(); }
        if self.source_clear_pending { self.phosphor.clear_picture(cx); self.spectrum_view.clear_picture(cx); }
        let twin = self.overlay_pair().then(|| InstrumentLook::for_theme(self.dark).channel(self.sources[1 - self.primary_source].channel.unwrap()));
        self.phosphor.set_twin_channel(twin); self.spectrum_view.set_twin_channel(twin);
        self.source_swap_pending = false; self.source_clear_pending = false; self.sources_changed = false;
    }

    /// Swaps retained pictures and answers, returning the action a host can persist. No feed is
    /// restarted; subsequent frames and measurements keep using their installation index.
    pub fn swap_sources(&mut self, cx: &mut Cx) -> ScopeAction {
        if self.sources.len() != 2 { return ScopeAction::None; }
        self.sync_sources(cx);
        self.primary_source = 1 - self.primary_source;
        std::mem::swap(&mut self.measurements, &mut self.secondary_measurements);
        self.set_spectrum_measurements(cx, None);
        self.phosphor.swap_twin(cx);
        self.spectrum_view.swap_twin(cx);
        self.answers_dirty = true;
        self.answers_at = None;
        self.area.redraw(cx);
        ScopeAction::SwapSources { primary: self.primary_source }
    }

    /// Submit hits from a stable source index, returning its storage through the usual mailbox.
    pub fn push_source_frame(&mut self, cx: &mut Cx, source: usize, hits: Hits) -> Pushed {
        self.sync_sources(cx);
        if source == self.primary_source { return self.phosphor.push_frame(cx, hits); }
        if self.sources.len() != 2 || source >= 2 || self.plot_px() != Some((hits.w, hits.h)) { return Pushed::WrongSize(hits); }
        self.phosphor.set_ghost_hits(cx, hits)
    }

    /// Measurement updates from either running source; equal rows do no allocation or redraw.
    pub fn set_source_measurements(&mut self, cx: &mut Cx, source: usize, rows: &[Measurement]) {
        if source == self.primary_source { self.set_lane_measurements(cx, rows, 0); }
        else if source < self.sources.len() && self.secondary_measurements != rows {
            self.secondary_measurements.clear(); self.secondary_measurements.extend_from_slice(rows);
            self.answers_dirty = true; self.area.redraw(cx);
        }
    }

    /// Standalone sources join time and spectral rows only when either snapshot changes.
    pub fn set_source_measurements_with_spectrum(&mut self, cx: &mut Cx, source: usize, rows: &[Measurement], spectrum: &[Measurement]) {
        let target = if source == self.primary_source { &mut self.measurements }
            else if source < self.sources.len() { &mut self.secondary_measurements } else { return };
        if target.iter().eq(rows.iter().chain(spectrum)) { return; }
        target.clear(); target.extend_from_slice(rows); target.extend_from_slice(spectrum);
        self.answers_dirty = true; self.area.redraw(cx);
    }

    /// Current selected answers before the display's 100 ms refresh throttle.
    pub fn source_answers(&self) -> Answers {
        let (mut answers, _, unjudged) = self.expected_answers();
        if self.settings.domain == Domain::Fft && !self.collapsed {
            if let Some((t0, t1)) = self.bracket { answers.append_gate(t1 - t0); }
        }
        for name in &unjudged {
            if !answers.text.is_empty() { answers.text.push_str("   "); }
            answers.text.push_str(name); answers.text.push_str(" not judged");
        }
        answers
    }

    fn automatic_answer_names(&self, meter: bool) -> Vec<&'static str> {
        let mut names = if self.sources.len() == 2 && !self.overlay_pair() && !meter { vec!["freq", "duty", "Vpp", "rise"] }
            else { lane_answer_names(&self.measurements, 0, Domain::Time, meter) };
        if self.spectrum().is_some() { names.extend(lane_answer_names(&self.measurements, 0, Domain::Fft, false)); }
        names
    }

    fn selected_answer_names(&self, meter: bool) -> Vec<&'static str> {
        let mut names = if meter { self.automatic_answer_names(true) }
            else { self.settings.answers.as_ref().map_or_else(|| self.automatic_answer_names(false), |names| names.iter().filter_map(|name| answer_name(name)).collect()) };
        for name in self.pins.iter().map(|q| q.name()).chain(self.expects.iter().filter_map(|expect| Self::expected_answer_name(&expect.answer))) {
            if !names.contains(&name) { names.push(name); }
        }
        names
    }

    fn answer_required(&self, name: &str) -> bool {
        self.pins.iter().any(|q| q.name() == name) || self.answer_expected(name)
    }

    fn answer_expected(&self, name: &str) -> bool {
        self.expects.iter().any(|expect| Self::expected_answer_name(&expect.answer) == Some(name))
    }

    /// Shared by a drop off the lane and the answer's lookup DISMISS action.
    #[doc(hidden)]
    pub fn remove_answer(&mut self, cx: &mut Cx, name: &str) -> bool {
        let Some(name) = answer_name(name) else { return false };
        if self.answer_expected(name) { return false; }
        let automatic = self.automatic_answer_names(false);
        let selected = self.settings.answers.as_ref().map_or_else(|| automatic.contains(&name), |names| names.iter().any(|answer| answer == name));
        let actions = if selected { self.settings.toggle_answer(name, &automatic) }
            else if self.settings.answers.is_none() {
                // A pin can be outside the automatic set; removal still makes the list explicit.
                self.settings.edit(false, |s| s.answers = Some(automatic.iter().map(|name| (*name).into()).collect()))
            } else { Vec::new() };
        if self.pins.iter().any(|q| q.name() == name) {
            self.pins.retain(|q| q.name() != name);
            self.distribution_pins_changed(cx);
            cx.widget_action(self.uid, ScopeAction::Pin { name, on: false });
        }
        self.answers_dirty = true;
        self.answers_at = None;
        self.answers_layout_key = None;
        self.emit(cx, actions);
        self.refresh_answers(cx);
        self.area.redraw(cx);
        true
    }

    fn expected_answer_name(name: &str) -> Option<&'static str> {
        if name == "Ipp" { Some("Vpp") } else { answer_name(name) }
    }

    // Twins compare selected time quantities; spectral rows always read the primary transform.
    fn named_answers(&self, name: &str, ripple: bool) -> Answers {
        if self.measurement_note.is_some() { return Answers::default(); }
        let spectral = SPECTRUM_ANSWERS.contains(&name);
        if spectral && (self.spectrum().is_none() || self.measurement("gate too short").is_some()) { return Answers::default(); }
        if self.sources.len() == 2 && !self.overlay_pair() && !self.collapsed && !spectral {
            let answers = twin_named_answers(&self.measurements, &self.secondary_measurements,
                self.sources[self.primary_source].side, self.sources[1 - self.primary_source].side, &[name]);
            if !answers.items.is_empty() || !self.answer_required(name) { return answers; }
        }
        lane_named_answers(&self.measurements, &self.ghost_rows, 0, &[name], ripple)
    }

    fn main_answers(&self) -> (Answers, Vec<AnswerName>) {
        let names = self.selected_answer_names(self.collapsed);
        let ripple = names.first() == Some(&"mean");
        let mut answers = Answers::default();
        let mut answer_names = Vec::new();
        for name in names {
            let mut selected = self.named_answers(name, ripple);
            let required = self.answer_required(name);
            let explicit = !self.collapsed && self.settings.answers.is_some() && (!SPECTRUM_ANSWERS.contains(&name) || self.spectrum().is_some());
            if selected.items.is_empty() && (required || explicit) {
                selected = Answers { text: format!("{name} -"), items: vec![Answer {
                    name: format!("{name} "), value: "-".into(), unit: String::new(), dim: Vec::new(), marks: Vec::new(),
                }] };
            }
            if !selected.items.is_empty() { answer_names.push(Some(name)); }
            answers.append(selected);
        }
        if self.spectrum().is_some() && self.measurement("gate too short").is_some() {
            answers.append(Answers { items: Vec::new(), text: "gate too short".into() });
        }
        if answers.items.is_empty() && answers.text.is_empty() && !self.settings.trigger.auto { answers.text = "waiting for a trigger".into(); }
        if self.measurement_note.is_none() { append_held(&mut answers, &self.measurements, 0); }
        answer_names.resize(answers.items.len(), None);
        (answers, answer_names)
    }

    /// Access to the screen for the host's feed and returned buffers.
    pub fn phosphor(&mut self) -> &mut PhosphorView {
        &mut self.phosphor
    }

    pub fn spectrum_view(&mut self) -> &mut PhosphorView { &mut self.spectrum_view }
    pub fn spectrum_setup(&self) -> Option<(FftSettings, (u32, u32))> { self.spectrum().zip(self.spectrum_view.plot_px()) }

    pub fn push_spectrum_frame(&mut self, cx: &mut Cx, source: usize, hits: Hits) -> Pushed {
        self.sync_sources(cx);
        if self.spectrum().is_none() { return Pushed::WrongSize(hits); }
        if source == self.primary_source { return self.spectrum_view.push_frame(cx, hits); }
        if self.sources.len() != 2 || source >= 2 || self.spectrum_view.plot_px() != Some((hits.w, hits.h)) { return Pushed::WrongSize(hits); }
        self.spectrum_view.set_ghost_hits(cx, hits)
    }

    /// Measure the same walk used by drawing, including wrapped answer rows, without painting.
    pub(super) fn layout_walk(&mut self, cx: &mut Cx2d, mut walk: Walk) -> Walk {
        let theme = base_theme(cx);
        self.dark = theme != BaseTheme::Light;
        if self.text_cache_draw != Some(cx.drawing_id()) {
            if self.text_cache.begin_frame(&self.draw_text, theme) { self.answers_layout_key = None; }
            self.text_cache_draw = Some(cx.drawing_id());
        }
        self.refresh_answers(cx);
        if walk.width.is_fit() { walk.width = Size::Fixed(900.0); }
        if walk.height.is_fit() { walk.height = Size::Fixed(if self.collapsed { 28.0 } else { self.height }); }
        let base = cx.peek_walk_turtle(walk);
        if !self.collapsed { self.height = base.size.y; }
        self.layout_answers(cx, base.size.x);
        let wrapped_height = self.answers_layout.rows.len() as f64 * ROW;
        if wrapped_height > 0.0 { walk.height = Size::Fixed(base.size.y + wrapped_height); }
        walk
    }

    // Base excludes wrapped answers; both layout-only acquisition and drawing use these cells.
    fn plot_layout(&self, base: Rect) -> (Rect, Option<Rect>, Rect, Rect, f64) {
        let fft = self.spectrum().is_some();
        let row_height = if self.shared_time { 0.0 } else { 22.0 };
        let height = if self.collapsed { 160.0 } else { base.size.y };
        let available = (height - 43.0 - row_height).max(1.0);
        let full = Rect { pos: base.pos + dvec2(64.0, 42.0), size: dvec2((base.size.x - 108.0).max(1.0), available) };
        let (time, divider, cell) = cell_layout(full, fft, self.collapsed, self.settings.fft.cell_share);
        let cell = cell.unwrap_or_default();
        let spectrum_height = (available + row_height - 22.0).max(1.0);
        let (plot_height, waterfall_height) = if fft && self.settings.fft.waterfall { waterfall_split(spectrum_height, self.settings.fft.waterfall_share) } else { (spectrum_height, 0.0) };
        let axis_width = 48.0f64.min(cell.size.x * 0.3);
        let spectrum = Rect { pos: cell.pos + dvec2(axis_width, 0.0), size: dvec2((cell.size.x - axis_width).max(0.0), plot_height) };
        (time, divider, cell, spectrum, waterfall_height)
    }

    /// A never-drawn offscreen card still needs its real axes for measurements and a stable feed.
    pub(super) fn setup_for_size(&self, size: DVec2, dpi: f64) -> super::acquire::LaneSetup {
        let (time, _, _, spectrum, _) = self.plot_layout(Rect { pos: dvec2(0.0, 0.0), size });
        let cell_drag = self.drag.is_some_and(|drag| drag.part == Part::CellDivider);
        let time_size = (if cell_drag { self.cell_rasters.map(|sizes| sizes.0) } else { None }).unwrap_or(time.size);
        let spectrum_size = (if cell_drag { self.cell_rasters.map(|sizes| sizes.1) }
            else { self.drag.filter(|drag| drag.part == Part::Divider).and(self.divider_raster) }).unwrap_or(spectrum.size);
        let pixels = |size: DVec2| ((size.x * dpi).ceil().max(0.0) as u32, (size.y * dpi).ceil().max(0.0) as u32);
        super::acquire::LaneSetup { map: self.settings.map(pixels(time_size)), spectrum: self.spectrum().map(|fft| (fft, pixels(spectrum_size))), drawn: true }
    }

    pub fn plot_px(&self) -> Option<(u32, u32)> {
        self.phosphor.plot_px()
    }

    /// The time picture holds data and no queued source change clears it at the next draw.
    pub(super) fn holds_picture(&self) -> bool { self.phosphor.picture_has_data() && !self.picture_clear_pending() }

    /// A source change that clears the pictures waits for the next draw.
    pub(super) fn picture_clear_pending(&self) -> bool { self.sources_changed && self.source_clear_pending }

    /// Keeps the old screen and its answers beneath the next live picture, reusing row storage.
    pub fn freeze_ghost(&mut self, cx: &mut Cx) {
        self.sync_sources(cx);
        if self.sources.len() < 2 {
            self.phosphor.freeze_ghost(cx);
            if self.spectrum().is_some() { self.spectrum_view.freeze_ghost(cx); }
            self.ghost_rows.clone_from(&self.measurements);
        } else { self.ghost_rows.clear(); }
        self.answers_dirty = true;
        self.answers_at = None;
        self.area.redraw(cx);
    }

    /// Chooses whether the old screen lies over the live picture.
    pub fn set_ghost_on_top(&mut self, cx: &mut Cx, on_top: bool) {
        self.phosphor.set_ghost_on_top(cx, on_top);
        self.spectrum_view.set_ghost_on_top(cx, on_top);
    }

    /// Hides the old screen while keeping the live picture.
    pub fn clear_ghost(&mut self, cx: &mut Cx) {
        self.phosphor.clear_ghost(cx);
        self.spectrum_view.clear_ghost(cx);
        self.ghost_rows.clear();
        self.answers_dirty = true;
        self.answers_at = None;
        self.area.redraw(cx);
    }

    /// Hears an authoritative snapshot without echoing actions to the host.
    pub fn set_settings(&mut self, cx: &mut Cx, settings: ScopeSettings) {
        if self.settings != settings { self.zone_drag = None; }
        if self.settings.answers != settings.answers { self.answers_at = None; self.answers_layout_key = None; }
        if self.settings.domain != settings.domain { self.clear_ghost(cx); self.measurements.clear(); self.secondary_measurements.clear(); self.set_spectrum_measurements(cx, None); self.focused = None; self.drag = None; self.view_drag = None; self.clear_bracket_drag(); self.clear_value_bracket_drag(); }
        self.settings = settings;
        self.update_stretch();
        self.answers_dirty = true;
        self.update_look(cx);
        self.area.redraw(cx);
    }

    /// Replace this net's expectations. Unchanged expectations do no formatting or redraw.
    pub fn set_expects(&mut self, cx: &mut Cx, expects: &[LaneExpect]) {
        if self.expects == expects { return; }
        self.expects.clear(); self.expects.extend_from_slice(expects);
        self.answers_popover_key = None;
        self.answers_dirty = true;
        self.answers_at = None;
        self.area.redraw(cx);
    }

    /// Keep the last drawn answers for at least 100 ms, even when captures arrive faster.
    pub fn set_measurements(&mut self, cx: &mut Cx, measurements: Vec<Measurement>) {
        self.measurements = measurements;
        self.answers_popover_key = None;
        self.answers_dirty = true;
        self.area.redraw(cx);
    }

    /// Latest completed worker spectrum; marker rendering never needs its bins.
    pub fn set_spectrum_measurements(&mut self, cx: &mut Cx, spectrum: Option<SpectrumMeasurements>) {
        if self.spectrum_measurements == spectrum { return; }
        self.spectrum_measurements = spectrum;
        self.harmonic_answers = std::array::from_fn(|i| spectrum.and_then(|s| s.harmonics[i]).map(|(_, dbc)| Answer {
            name: format!("H{} ", i + 2), value: format!("{dbc:.1}"), unit: "dBc".into(), dim: Vec::new(), marks: Vec::new(),
        }));
        self.spectrum_hover = None;
        self.answers_layout_key = None;
        self.area.redraw(cx);
    }

    /// Compare borrowed publication rows before copying: repeating the same measured answers is
    /// allocation-free even when the feed publishes a new snapshot generation.
    pub fn set_lane_measurements(&mut self, cx: &mut Cx, rows: &[Measurement], lane: usize) {
        let own = || rows.iter().filter(|row| row.lane == lane);
        if self.measurements.len() == own().count() && self.measurements.iter().zip(own()).all(|(a, b)| a.name == b.name && a.value == b.value && a.unit == b.unit && a.sigma == b.sigma && a.n == b.n && a.step == b.step) { return; }
        self.measurements.clear();
        self.measurements.extend(own().cloned().map(|mut row| { row.lane = 0; row }));
        self.answers_popover_key = None;
        self.answers_dirty = true;
        self.area.redraw(cx);
    }

    /// Share the latest capture; unchanged Arcs do no work. The host passes None while scrubbing.
    pub fn set_features(&mut self, cx: &mut Cx, features: Option<Arc<CaptureFeatures>>, lane: usize) {
        let same = match (&self.features, &features) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b), (None, None) => true, _ => false,
        };
        if same && self.feature_lane == lane { return; }
        self.features = features;
        self.feature_lane = lane;
        let list = self.features.as_ref().and_then(|capture| capture.lanes.get(lane)).map_or(&[][..], Vec::as_slice);
        self.answer_geometries = ["freq", "duty", "Vpp", "rise"].map(|name| answer_geometry(name, list));
        if self.pointer.is_some_and(|p| self.plot.contains(p)) || self.hovered_answer().is_some() { self.area.redraw(cx); }
    }

    pub fn set_bracket(&mut self, cx: &mut Cx, bracket: Option<(f64, f64)>) {
        if self.bracket == bracket { return; }
        self.bracket = bracket;
        self.span_stats = None;
        self.span_answered = false;
        self.answers_dirty = true;
        self.frequency_key = None;
        if self.settings.domain == Domain::Fft && !self.collapsed {
            self.spectrum_view.clear(cx);
            self.measurements.retain(|row| !matches!(row.name.as_str(), "peak f" | "peak" | "2nd f" | "2nd" | "RBW" | "span" | "H2" | "H3" | "H4" | "H5" | "gate too short"));
            self.set_spectrum_measurements(cx, None);
        }
        if bracket.is_none() { self.clear_bracket_drag(); }
        self.area.redraw(cx);
    }

    pub fn set_span_stats(&mut self, cx: &mut Cx, stats: Option<SpanStats>, answered: bool) {
        let stats = if answered { stats } else { None };
        if self.span_stats == stats && self.span_answered == answered { return; }
        self.span_stats = stats;
        self.span_answered = answered;
        self.area.redraw(cx);
    }

    /// A scrubbed still hides the live bracket without changing its endpoints or answer.
    pub fn set_bracket_visible(&mut self, cx: &mut Cx, visible: bool) {
        if self.bracket_visible == visible { return; }
        self.bracket_visible = visible;
        if !visible { self.clear_bracket_drag(); }
        self.area.redraw(cx);
    }

    /// The lane's net, its solver channel, its probe's hue and whether it is the trigger's.
    pub fn set_identity(&mut self, cx: &mut Cx, net: &str, channel: usize, hue: usize, trigger: bool) {
        if self.net != net { self.net = net.into(); }
        if self.zone_lane != channel { self.zone_drag = None; self.zone_lane = channel; }
        if let Some(c) = self.settings.channels.first_mut() { c.color_index = hue; }
        if self.trigger_lane != trigger { self.trigger_lane = trigger; if !trigger { self.time_area = Area::default(); self.holdoff_area = Area::default(); self.holdoff_grab = None; } self.area.redraw(cx); }
    }
    pub fn set_collapsed(&mut self, cx: &mut Cx, collapsed: bool) {
        if collapsed { self.zone_drag = None; }
        if self.collapsed != collapsed { self.collapsed = collapsed; self.clear_bracket_drag(); self.clear_value_bracket_drag(); self.answers_dirty = true; self.redraw(cx); }
    }
    pub fn collapsed(&self) -> bool { self.collapsed }

    pub(in crate::instruments) fn height(&self) -> f64 { self.height }

    pub(in crate::instruments) fn set_height(&mut self, cx: &mut Cx, height: f64) {
        if self.height != height { self.height = height; self.redraw(cx); }
    }

    fn apply_resize(&mut self, cx: &mut Cx, outcome: ResizeOutcome, done: bool) {
        self.set_height(cx, outcome.height);
        if self.collapsed != outcome.collapse { self.adjust(cx, Part::Collapse, 1); }
        cx.widget_action(self.uid, ScopeAction::Resize { height: self.height, done });
        self.area.redraw(cx);
    }
    /// What the source can do and what it resolved, for the sw/blind note and steps answer.
    /// Apply on source/configuration changes, not on paint; set_sources names who the lane shows.
    pub fn set_capabilities(&mut self, cx: &mut Cx, caps: Arc<Caps>, resolved: Option<Arc<Resolved>>, channel: ChanId) {
        if self.source_caps.as_ref() == Some(&caps) && self.source_resolved == resolved && self.source_channel == Some(channel) { return; }
        self.source_steps.clear();
        self.trigger_note.clear();
        if let Some(resolved) = &resolved {
            // An exact source (no channel has steps: the simulator) misses nothing to a software
            // trigger over its stream, so only a stepped device says "sw" there.
            let stepped = caps.channels.iter().any(|c| c.bits > 0);
            self.trigger_note = match resolved.trigger {
                TrigResolved::Software { over: Over::Stream } if stepped => "sw".into(),
                TrigResolved::Software { over: Over::Blocks { blind } } => format!("sw, blind {:.1} %", blind * 100.0),
                _ => String::new(),
            };
            if let Some(c) = resolved.channels.iter().find(|c| c.id == channel && c.step > 0.0) {
                let unit = self.settings.channels.first().map_or("V", |c| c.unit.as_str());
                self.source_steps.push(Answer { name: String::new(), value: si_label(c.step, unit), unit: "steps".into(), dim: Vec::new(), marks: Vec::new() });
                self.source_steps.push(Answer { name: String::new(), value: si_label(resolved.rate, "S/s"), unit: String::new(), dim: Vec::new(), marks: Vec::new() });
            }
        }
        self.source_caps = Some(caps); self.source_resolved = resolved; self.source_channel = Some(channel);
        self.answers_dirty = true; self.area.redraw(cx);
    }

    /// The simulation's time resolution (`LiveStatus::res`; 0 is Exact): the answers read it out
    /// as a real scope's sample interval (`res 37.5 µs`), so a stopped record's vertical edges read
    /// as what they are. Redraws only on a change.
    pub fn set_sim_res(&mut self, cx: &mut Cx, res: f64) {
        let res = (res > 0.0).then_some(res);
        if self.sim_res != res { self.sim_res = res; self.answers_dirty = true; self.area.redraw(cx); }
    }

    /// Actual frame saturation is separate from a view asking beyond a hardware range.
    pub fn set_clipped(&mut self, cx: &mut Cx, clipped: Clipped) {
        if self.clipped != clipped { self.clipped = clipped; self.answers_dirty = true; self.area.redraw(cx); }
    }

    pub fn set_trigger_state(&mut self, state: super::acquire::TriggerState) { self.trigger_state = state; }
    pub fn spectrum(&self) -> Option<FftSettings> { if self.collapsed { None } else { self.settings.spectrum() } }
    pub fn spark(&mut self) -> &mut NetSpark { &mut self.spark }
    pub fn sync_time(&mut self, cx: &mut Cx, settings: &ScopeSettings) {
        if (self.settings.persistence, self.settings.intensity) != (settings.persistence, settings.intensity) {
            self.settings.persistence = settings.persistence;
            self.settings.intensity = settings.intensity;
            self.update_look(cx);
        }
        if (self.settings.time_per_div, self.settings.position, self.settings.trigger, self.settings.running) != (settings.time_per_div, settings.position, settings.trigger, settings.running) {
            self.settings.running = settings.running;
            self.settings.time_per_div = settings.time_per_div;
            self.settings.position = settings.position;
            self.settings.trigger = Trigger { channel: 0, ..settings.trigger };
            self.update_stretch();
            self.answers_dirty = true;
            self.area.redraw(cx);
        }
    }

    // Select and judge the same snapshot, before it is throttled or laid out. Twins judge their
    // primary measurement; the ghost and secondary value never supply a primary verdict.
    fn expected_answers(&self) -> (Answers, Vec<Option<&'static str>>, Vec<String>) {
        let (mut answers, mut answer_names) = self.main_answers();
        if let Some(note) = &self.measurement_note {
            answers.append(Answers { items: Vec::new(), text: note.clone() });
            answer_names.resize(answers.items.len(), None);
        }
        let mut unjudged = Vec::new();
        for expect in &self.expects {
            let selected = answer_names.iter().enumerate().find_map(|(i, name)| {
                let name = (*name)?;
                if answers.items.get(i).is_none_or(|answer| answer.value == "-") { return None; }
                let row = self.measurements.iter().find(|r| r.lane == 0 && (r.name == name || name == "Vpp" && r.name == "Ipp"))?;
                (row.name == expect.answer).then_some((i, row))
            });
            if let Some((i, row)) = selected {
                if let Some(mark) = expect_mark(row.value, &row.unit, expect.min, expect.max) {
                    if let Some(answer) = answers.items.get_mut(i) { answer.marks.push(mark); continue; }
                }
            }
            if !unjudged.contains(&expect.answer) { unjudged.push(expect.answer.clone()); }
        }
        if answers.items.iter().any(|answer| !answer.marks.is_empty()) {
            answers.text.clear();
            for answer in &answers.items {
                if !answers.text.is_empty() { answers.text.push_str("   "); }
                answers.text.push_str(&answer.name); answers.text.push_str(&answer.value);
                if !answer.unit.is_empty() { answers.text.push('\u{2009}'); answers.text.push_str(&answer.unit); }
                for (_, mark) in &answer.marks {
                    answers.text.push(' ');
                    answers.text.push_str(&mark.replace(" (< ", ", under ").replace(" (> ", ", over ").replace(" (", ", ").replace(')', ""));
                }
            }
        }
        (answers, answer_names, unjudged)
    }

    fn refresh_answers(&mut self, cx: &mut Cx) {
        if !self.answers_dirty { return; }
        let interval = Duration::from_millis(100);
        if let Some(left) = self.answers_at.and_then(|at| interval.checked_sub(at.elapsed())) {
            // One pending refresh also delivers the final change after acquisition pauses.
            if self.answers_timer.is_empty() { self.answers_timer = cx.start_timeout(left.as_secs_f64()); }
            return;
        }
        let (mut answers, names, unjudged) = self.expected_answers();
        let names_changed = self.answer_names != names;
        self.answer_names = names;
        if let Some(drag) = &mut self.answer_drag {
            drag.index = self.answer_names.iter().position(|name| *name == Some(drag.name)).unwrap_or(usize::MAX);
        }
        let stats: Vec<_> = self.answer_names.iter().map(|name| name.and_then(|name| {
            self.measurements.iter().find(|r| r.lane == 0 && (r.name == name || name == "Vpp" && r.name == "Ipp"))
        }).map_or_else(String::new, answer_stats_text)).collect();
        for answer in &self.source_steps {
            answers.text.push_str(&format!("   {} {}", answer.value, answer.unit));
            answers.items.push(answer.clone());
        }
        if let Some(res) = self.sim_res {
            let value = format!("res {}", crate::live::strip::format_time(res));
            answers.text.push_str("   ");
            answers.text.push_str(&value);
            answers.items.push(Answer { name: String::new(), value, unit: String::new(), dim: Vec::new(), marks: Vec::new() });
        }
        if self.clipped.any() {
            answers.text.push_str("   clipped");
            answers.items.push(Answer { name: String::new(), value: "clipped".into(), unit: String::new(), dim: Vec::new(), marks: Vec::new() });
        }
        if self.settings.domain == Domain::Fft && !self.collapsed {
            if let Some((t0, t1)) = self.bracket { answers.append_gate(t1 - t0); }
        }
        for name in &unjudged {
            if !answers.text.is_empty() { answers.text.push_str("   "); }
            answers.text.push_str(name); answers.text.push_str(" not judged");
        }
        if names_changed || self.answers != answers || self.answer_stats != stats || self.unjudged_answers != unjudged {
            self.more_labels = (0..=answers.items.len() + unjudged.len()).map(|n| format!("+{n}")).collect();
            self.answer_marks = answers.items.iter().map(|answer| answer.marks.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("   ")).collect();
            self.answers = answers;
            if let Some(summary) = &self.semantic_summary { summary.borrow_mut().answer(&self.net, &self.answers.text); }
            self.answer_stats = stats;
            self.unjudged_answers = unjudged;
            self.answers_revision = self.answers_revision.wrapping_add(1);
        }
        cx.stop_timer(self.answers_timer);
        self.answers_timer = Timer::empty();
        self.answers_at = Some(Instant::now());
        self.measure_revision = self.measure_revision.wrapping_add(1);
        self.answers_dirty = false;
    }

    fn answers_popover_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        if self.answers_popover_widget.is_none() {
            self.answers_popover_widget = Some(cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.answers_popover.as_object().into())));
            cx.widget_tree_mark_dirty(self.uid);
        }
        self.answers_popover_widget.as_ref().unwrap().clone()
    }

    pub fn set_pins(&mut self, cx: &mut Cx, pins: Vec<TrendQuantity>) {
        if self.pins == pins { return; }
        self.pins = pins;
        self.distribution_pins_changed(cx);
    }

    pub fn set_stop_on_fail(&mut self, cx: &mut Cx, mut expects: Vec<String>) {
        expects.sort();
        if self.stop_on_fail == expects { return; }
        self.stop_on_fail = expects;
        self.answers_popover_key = None;
        self.area.redraw(cx);
    }

    fn answer_pin_actions(&mut self, cx: &mut Cx, popover: &PopoverRef, actions: &Actions) {
        let content = popover.content();
        for i in 0..self.answers.items.len() {
            let id = LiveId::from_str(&format!("answer{i}"));
            let Some(name) = self.answer_names.get(i).copied().flatten() else { continue };
            let Some(quantity) = TrendQuantity::from_name(name) else { continue };
            let row = content.widget(cx, &[id]);
            for (button, shorter) in [(id!(trigger_shorter), true), (id!(trigger_longer), false)] {
                if self.pins.contains(&quantity) && row.button(cx, &[button]).clicked(actions) {
                    if let Some(value) = self.measurement(name) {
                        let edits = self.settings.trigger_from_answer(quantity, value, shorter);
                        self.emit(cx, edits);
                    }
                }
            }
            if row.button(cx, ids!(pin)).clicked(actions) {
                let on = !self.pins.contains(&quantity);
                if on { self.pins.push(quantity); } else { self.pins.retain(|q| *q != quantity); }
                self.distribution_pins_changed(cx);
                cx.widget_action(self.uid, ScopeAction::Pin { name, on });
            }
        }
        let start = self.answers.items.len() + self.unjudged_answers.len();
        let mut pins_changed = false;
        for (i, expect) in self.expects.iter().enumerate() {
            let Some(quantity) = TrendQuantity::from_name(&expect.answer) else { continue };
            let row = content.widget(cx, &[LiveId::from_str(&format!("answer{}", start + i))]);
            if row.button(cx, ids!(pin)).clicked(actions) {
                let on = !self.pins.contains(&quantity);
                if on { self.pins.push(quantity); } else { self.pins.retain(|q| *q != quantity); }
                self.answers_popover_key = None;
                pins_changed = true;
                cx.widget_action(self.uid, ScopeAction::Pin { name: quantity.name(), on });
            }
            if row.button(cx, ids!(stop_on_fail)).clicked(actions) {
                let on = !self.stop_on_fail.contains(&expect.name);
                if on { self.stop_on_fail.push(expect.name.clone()); self.stop_on_fail.sort(); }
                else { self.stop_on_fail.retain(|name| *name != expect.name); }
                self.answers_popover_key = None;
                cx.widget_action(self.uid, ScopeAction::StopOnFail { expect: expect.name.clone(), on });
            }
        }
        if pins_changed { self.distribution_pins_changed(cx); }
        self.refresh_answers(cx);
        self.update_answers_popover(cx, popover);
    }

    fn update_answers_popover(&mut self, cx: &mut Cx, popover: &PopoverRef) {
        let channel = self.settings.channels.first().map_or(0, |c| c.color_index);
        let key = (self.answers_revision, self.dark, channel);
        if self.answers_popover_key == Some(key) { return; }
        let content = popover.content();
        let look = &InstrumentLook::for_theme(self.dark).on_panel();
        let color = self.channel_color(0);
        let expect_start = self.answers.items.len() + self.unjudged_answers.len();
        let count = expect_start + self.expects.len();
        let uid = content.widget_uid();
        let rows = if let Some(mut view) = content.borrow_mut::<View>() {
            let changed = view.children.len() != count;
            while view.children.len() < count {
                let id = LiveId::from_str(&format!("answer{}", view.children.len()));
                let row = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.answer_row.as_object().into()));
                cx.widget_tree_insert_child(uid, id, row.clone());
                view.children.push((id, row));
            }
            view.children.truncate(count);
            if changed { cx.widget_tree_mark_dirty(uid); }
            view.children.iter().map(|(_, row)| row.clone()).collect::<Vec<_>>()
        } else { return };
        for (i, row) in rows.iter().enumerate() {
            row.button(cx, ids!(trigger_shorter)).set_visible(cx, false);
            row.button(cx, ids!(trigger_longer)).set_visible(cx, false);
            let stop = row.button(cx, ids!(stop_on_fail));
            if let Some(expect) = i.checked_sub(expect_start).and_then(|i| self.expects.get(i)) {
                let quantity = TrendQuantity::from_name(&expect.answer);
                let on = self.stop_on_fail.contains(&expect.name);
                stop.set_visible(cx, quantity.is_some());
                stop.set_text(cx, if on { "stop on fail: on" } else { "stop on fail" });
                if let Some(mut button) = stop.borrow_mut() {
                    let ink = if on { look.canvas.violation } else { look.text_tertiary };
                    button.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
                }
                let pin = row.button(cx, ids!(pin));
                pin.set_visible(cx, quantity.is_some());
                if let Some(mut button) = pin.borrow_mut() {
                    let ink = if quantity.is_some_and(|q| self.pins.contains(&q)) { color } else { look.text_tertiary };
                    button.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
                }
                for id in [id!(unit), id!(ghost), id!(stats)] { row.label(cx, &[id]).set_visible(cx, false); }
                let name = row.label(cx, ids!(name));
                name.set_visible(cx, true); name.set_text(cx, &format!("{}  ", expect.name));
                name.set_text_color(cx, vec4(look.text_secondary[0], look.text_secondary[1], look.text_secondary[2], look.text_secondary[3]));
                let value = row.label(cx, ids!(value));
                value.set_text(cx, &expect.answer);
                value.set_text_color(cx, vec4(color[0], color[1], color[2], color[3]));
                let verdict = self.measurements.iter().find(|r| r.lane == 0 && r.name == expect.answer)
                    .and_then(|r| expect_mark(r.value, &r.unit, expect.min, expect.max));
                let mark = row.label(cx, ids!(expect));
                mark.set_visible(cx, true); mark.set_text(cx, verdict.as_ref().map_or("not judged", |(_, text)| text.as_str()));
                let ink = verdict.map_or(look.text_tertiary, |(pass, _)| if pass { look.canvas.ok } else { look.canvas.violation });
                mark.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
                continue;
            }
            stop.set_visible(cx, false);
            let answer = self.answers.items.get(i);
            let unjudged = i >= self.answers.items.len();
            let quantity = if unjudged { None } else { self.answer_names.get(i).copied().flatten().and_then(TrendQuantity::from_name) };
            if let Some(q) = quantity.filter(|q| self.pins.contains(q) && matches!(q, TrendQuantity::Width | TrendQuantity::Freq)) {
                if self.measurement(q.name()).is_some_and(|value| value > 0.0 && (q != TrendQuantity::Freq || (1.0 / value).is_finite())) {
                    let shorter = row.button(cx, ids!(trigger_shorter));
                    let longer = row.button(cx, ids!(trigger_longer));
                    shorter.set_visible(cx, true); longer.set_visible(cx, true);
                    shorter.set_text(cx, if q == TrendQuantity::Width { "trigger shorter" } else { "period shorter" });
                    longer.set_text(cx, if q == TrendQuantity::Width { "trigger longer" } else { "period longer" });
                    for button in [&shorter, &longer] {
                        if let Some(mut button) = button.borrow_mut() { button.draw_text.color = vec4(color[0], color[1], color[2], color[3]); }
                    }
                }
            }
            let pin = row.button(cx, ids!(pin));
            pin.set_visible(cx, quantity.is_some());
            if let Some(mut button) = pin.borrow_mut() {
                let ink = if quantity.is_some_and(|q| self.pins.contains(&q)) { color } else { look.text_tertiary };
                button.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
            }
            let name = row.label(cx, ids!(name));
            let text = answer.map_or_else(|| self.unjudged_answers[i - self.answers.items.len()].as_str(), |a| a.name.as_str());
            name.set_visible(cx, !text.is_empty());
            name.set_text(cx, text);
            name.set_text_color(cx, vec4(look.text_secondary[0], look.text_secondary[1], look.text_secondary[2], look.text_secondary[3]));
            let value = row.label(cx, ids!(value));
            value.set_text(cx, answer.map_or("", |a| a.value.as_str()));
            let ink = if answer.is_some_and(Answer::failing) { look.canvas.violation } else { color };
            value.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
            let unit = row.label(cx, ids!(unit));
            let text = answer.map_or("", |a| a.unit.as_str());
            let at = text.find(" (was ").unwrap_or(text.len());
            unit.set_visible(cx, !text.is_empty());
            unit.set_text(cx, &format!("\u{2009}{}", &text[..at]));
            unit.set_text_color(cx, vec4(look.text_secondary[0], look.text_secondary[1], look.text_secondary[2], look.text_secondary[3]));
            let ghost = row.label(cx, ids!(ghost));
            ghost.set_visible(cx, at < text.len());
            ghost.set_text(cx, &text[at..]);
            ghost.set_text_color(cx, vec4(look.text_tertiary[0], look.text_tertiary[1], look.text_tertiary[2], look.text_tertiary[3]));
            let stats = row.label(cx, ids!(stats));
            let text = if unjudged { "" } else { self.answer_stats.get(i).map_or("", String::as_str) };
            stats.set_visible(cx, !text.is_empty());
            stats.set_text(cx, text);
            stats.set_text_color(cx, vec4(look.text_tertiary[0], look.text_tertiary[1], look.text_tertiary[2], look.text_tertiary[3]));
            let mark = row.label(cx, ids!(expect));
            let text = if unjudged { "not judged" } else { self.answer_marks.get(i).map_or("", String::as_str) };
            mark.set_visible(cx, !text.is_empty());
            mark.set_text(cx, text);
            let ink = if unjudged { look.text_tertiary } else if answer.is_some_and(Answer::failing) { look.canvas.violation } else { look.canvas.ok };
            mark.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
        }
        self.update_answer_distributions(cx, &rows);
        self.answers_popover_key = Some(key);
        content.redraw(cx);
    }

    fn toggle_answers(&mut self, cx: &mut Cx, anchor: Part) {
        let Some(rect) = self.answer_part_rect(anchor) else { return };
        let widget = self.answers_popover_widget(cx);
        let popover = widget.as_popover();
        if popover.is_open() { popover.close(cx); self.answers_anchor = None; self.answers_anchor_part = None; }
        else {
            self.update_answers_popover(cx, &popover);
            let rect = Rect { pos: rect.pos + self.area.rect(cx).pos - self.drawn_at, ..rect };
            popover.open_at(cx, rect);
            self.answers_anchor = Some(rect);
            self.answers_anchor_part = Some(anchor);
        }
        self.area.redraw(cx);
    }

    fn answer_drag_preview(&self) -> Option<(usize, bool)> {
        let drag = self.answer_drag?;
        (drag.started && self.answer_names.get(drag.index).copied().flatten() == Some(drag.name))
            .then(|| (drag.index, drag.outside && !self.answer_expected(drag.name)))
    }

    fn answer_alpha(&self, index: usize) -> f32 {
        if self.answer_drag.is_some_and(|drag| drag.started && drag.index == index && self.answer_names.get(index).copied().flatten() == Some(drag.name)) { 0.35 } else { 1.0 }
    }

    fn update_answer_tip(&self, cx: &mut Cx, tip: &WidgetRef, drag: AnswerDrag) {
        let text = if self.answer_expected(drag.name) { "expected: remove its expectation in the design".into() }
            else {
                let mut text = if drag.outside { "remove ".into() } else { String::new() };
                if let Some(answer) = self.answers.items.get(drag.index) {
                    if answer.name.is_empty() { text.push_str(drag.name); text.push(' '); }
                    text.push_str(&answer.name); text.push_str(&answer.value);
                    if !answer.unit.is_empty() { text.push('\u{2009}'); text.push_str(&answer.unit); }
                    for (_, mark) in &answer.marks { text.push(' '); text.push_str(mark); }
                } else { text.push_str(drag.name); }
                text
            };
        let label = tip.label(cx, ids!(label));
        label.set_text(cx, &text);
        let ink = self.channel_color(0);
        label.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
    }

    fn handle_answer_drag(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(mut drag) = self.answer_drag else { return false };
        let ours = |items: &[DragItem]| items.iter().any(|item| matches!(item, DragItem::String { internal_id: Some(id), .. } if *id == ANSWER_DRAG));
        if drag.started {
            match event {
                Event::Drag(e) if ours(&e.items) => {
                    let outside = !self.area.rect(cx).contains(e.abs);
                    if let DragHit::Drag(hit) = event.drag_hits(cx, self.area) {
                        *hit.response.lock().unwrap_or_else(|p| p.into_inner()) = DragResponse::Move;
                    }
                    if drag.outside != outside {
                        drag.outside = outside;
                        self.answer_drag = Some(drag);
                        if let Some(tip) = &self.answer_tip_widget { self.update_answer_tip(cx, tip, drag); }
                        self.area.redraw(cx);
                    }
                    cx.set_cursor(MouseCursor::Grabbing);
                    return true;
                }
                Event::Drop(e) if ours(&e.items) => {
                    self.answer_drag = None;
                    if !self.area.rect(cx).contains(e.abs) { self.remove_answer(cx, drag.name); }
                    else { let _ = event.drag_hits(cx, self.area); }
                    cx.set_cursor(self.pointer_cursor(e.abs));
                    self.area.redraw(cx);
                    return true;
                }
                Event::DragEnd => {
                    // A drop consumed the state above; a bare end only restores the preview.
                    self.answer_drag = None;
                    cx.set_cursor(MouseCursor::Default);
                    self.area.redraw(cx);
                    return true;
                }
                _ => return matches!(event, Event::MouseMove(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::FingerCancel(_)),
            }
        }
        if !matches!(event, Event::MouseMove(_) | Event::MouseUp(_) | Event::TouchUpdate(_) | Event::FingerCancel(_)) { return false; }
        // open_at takes the popover's sweep lock immediately. Lift only that lock while
        // checking the lane's captured opening press, before the popover handles the event.
        let popover = self.answers_popover_widget.as_ref().filter(|widget| widget.as_popover().is_open()).cloned();
        if let Some(widget) = &popover { cx.sweep_unlock(widget.area()); }
        let hit = event.hits(cx, self.area);
        if let Some(widget) = &popover { cx.sweep_lock(widget.area()); }
        match hit {
            Hit::FingerMove(e) => {
                if (e.abs - drag.press).length() > 4.0 {
                    if let Some(widget) = popover { widget.as_popover().close(cx); }
                    self.answers_anchor = None;
                    self.answers_anchor_part = None;
                    self.pointer = None;
                    drag.started = true;
                    drag.outside = !self.area.rect(cx).contains(e.abs);
                    self.answer_drag = Some(drag);
                    let tip = self.answer_tip_widget.get_or_insert_with(|| cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.answer_tip.as_object().into()))).clone();
                    self.update_answer_tip(cx, &tip, drag);
                    cx.start_dragging_with(vec![DragItem::String { value: drag.name.into(), internal_id: Some(ANSWER_DRAG) }], tip, dvec2(14.0, 12.0));
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.area.redraw(cx);
                }
                true
            }
            Hit::FingerUp(_) => { self.answer_drag = None; false }
            _ => false,
        }
    }

    fn spectrum_popover_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        if self.spectrum_popover_widget.is_none() {
            self.spectrum_popover_widget = Some(cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.spectrum_popover.as_object().into())));
            cx.widget_tree_mark_dirty(self.uid);
        }
        self.spectrum_popover_widget.as_ref().unwrap().clone()
    }

    fn update_spectrum_popover(&mut self, cx: &mut Cx, popover: &PopoverRef) {
        let fft = self.settings.fft;
        if self.spectrum_popover_key == Some(fft) { return; }
        let content = popover.content();
        let window = content.widget(cx, ids!(window));
        for (id, (label, current)) in [id!(hann), id!(blackman), id!(flat_top), id!(rect)].into_iter().zip(fft.window_choices()) {
            window.button(cx, &[id]).set_text(cx, &format!("{} {label}", if current { "●" } else { "○" }));
        }
        for (row, choices) in [
            (id!(frequency), [(id!(log), "log", fft.log_freq), (id!(linear), "linear", !fft.log_freq)]),
            (id!(waterfall), [(id!(on), "on", fft.waterfall), (id!(off), "off", !fft.waterfall)]),
        ] {
            let row = content.widget(cx, &[row]);
            for (id, label, current) in choices {
                row.button(cx, &[id]).set_text(cx, &format!("{} {label}", if current { "●" } else { "○" }));
            }
        }
        self.spectrum_popover_key = Some(fft);
        content.redraw(cx);
    }

    fn toggle_spectrum(&mut self, cx: &mut Cx) {
        let Some(rect) = self.parts.iter().find(|part| part.part == Part::Spectrum).map(|part| part.rect) else { return };
        let widget = self.spectrum_popover_widget(cx);
        let popover = widget.as_popover();
        if popover.is_open() { popover.close(cx); self.spectrum_anchor = None; }
        else {
            self.update_spectrum_popover(cx, &popover);
            let rect = Rect { pos: rect.pos + self.area.rect(cx).pos - self.drawn_at, ..rect };
            popover.open_at(cx, rect);
            self.spectrum_anchor = Some(rect);
        }
        self.area.redraw(cx);
    }

    fn spectrum_actions(&mut self, cx: &mut Cx, popover: &PopoverRef, actions: &Actions) {
        if actions.is_empty() { return; }
        let content = popover.content();
        let window = content.widget(cx, ids!(window));
        for (id, choice) in [id!(hann), id!(blackman), id!(flat_top), id!(rect)].into_iter().zip(FFT_WINDOWS) {
            if window.button(cx, &[id]).clicked(actions) {
                let actions = self.settings.set_window(choice);
                self.emit(cx, actions);
            }
        }
        let frequency = content.widget(cx, ids!(frequency));
        for (id, log) in [(id!(log), true), (id!(linear), false)] {
            if frequency.button(cx, &[id]).clicked(actions) && self.settings.fft.log_freq != log {
                let actions = self.settings.toggle_log();
                self.emit(cx, actions);
            }
        }
        let waterfall = content.widget(cx, ids!(waterfall));
        for (id, on) in [(id!(on), true), (id!(off), false)] {
            if waterfall.button(cx, &[id]).clicked(actions) && self.settings.fft.waterfall != on {
                let actions = self.settings.toggle_waterfall();
                self.emit(cx, actions);
            }
        }
        self.update_spectrum_popover(cx, popover);
    }

    /// Stores eye measurements to map onto the current screen at draw time.
    pub fn set_eye_marks(&mut self, cx: &mut Cx, marks: Option<EyeMarks>) {
        self.eye_marks = marks;
        self.eye_height = marks.map_or_else(String::new, |m| format!("height {}", eng(m.top - m.bottom, self.settings.channels.first().map_or("V", |c| c.unit.as_str()), 4)));
        self.eye_width = marks.map_or_else(String::new, |m| format!("width {}", eng(m.ui - m.jitter_pp, "s", 4)));
        self.area.redraw(cx);
    }

    /// Rename a channel without clearing its picture or changing acquisition.
    pub fn set_channel_label(&mut self, cx: &mut Cx, channel: usize, label: &str) {
        if let Some(channel) = self.settings.channels.get_mut(channel) {
            if channel.name != label {
                channel.name = label.to_owned();
                self.area.redraw(cx);
            }
        }
    }

    /// Show a host status in place of measurements until it clears.
    pub fn set_measurement_note(&mut self, cx: &mut Cx, note: Option<&str>) {
        if self.measurement_note.as_deref() != note {
            self.measurement_note = note.map(str::to_owned);
            self.answers_dirty = true;
            self.area.redraw(cx);
        }
    }

    fn update_look(&mut self, cx: &mut Cx) {
        self.spectrum_view.set_waterfall(!self.collapsed && self.settings.domain == Domain::Fft && self.settings.fft.waterfall);
        let look = self.settings.look(!self.dark);
        if self.last_look != Some(look) {
            self.phosphor.set_look(cx, look);
            self.spectrum_view.set_look(cx, look);
            self.last_look = Some(look);
        }
    }

    fn emit(&mut self, cx: &mut Cx, actions: Vec<ScopeAction>) {
        if actions.is_empty() {
            return;
        }
        for action in actions {
            if action == ScopeAction::Clear {
                self.phosphor.clear(cx);
                self.spectrum_view.clear(cx);
            }
            cx.widget_action(self.uid, action);
        }
        self.update_look(cx);
        self.area.redraw(cx);
    }

    // Keyboard/a11y keep the bounded 1-2-5 divisions, 0.1-division offset/level
    // steps (one whole dB division for FFT reference) and 0.05 trigger-position steps.
    // Pointer gestures never call this for scales.
    fn adjust(&mut self, cx: &mut Cx, part: Part, steps: i32) {
        if part == Part::More { self.toggle_answers(cx, part); return; }
        if part == Part::AddAnswer { self.toggle_measure_palette(cx); return; }
        if part == Part::Spectrum { self.toggle_spectrum(cx); return; }
        if part == Part::Secondary { let action = self.swap_sources(cx); self.emit(cx, vec![action]); return; }
        if part == Part::Domain { self.clear_ghost(cx); self.measurements.clear(); self.secondary_measurements.clear(); self.set_spectrum_measurements(cx, None); self.answers_dirty = true; self.drag = None; self.view_drag = None; self.clear_bracket_drag(); self.clear_value_bracket_drag(); }
        let s = &mut self.settings;
        let actions = match part {
            Part::Tag | Part::Secondary | Part::More | Part::AddAnswer | Part::Spectrum | Part::Holdoff | Part::Resize | Part::Answer(_) => return,
            Part::SpectrumScale => s.step_db(steps),
            Part::SpectrumOffset => s.step_ref(steps),
            Part::Scale(i) => s.step_scale(i, steps),
            Part::Plot(i) | Part::Offset(i) => { let offset = s.channels[i].offset_div; s.set_offset(i, offset + f64::from(steps) * 0.1) }
            Part::Domain => s.set_domain(if s.domain == Domain::Time { Domain::Fft } else { Domain::Time }),
            Part::Time | Part::Frequency => s.step_time(steps),
            Part::Divider => s.set_waterfall_share(s.fft.waterfall_share + steps as f64 * 0.05),
            Part::CellDivider => s.set_cell_share(s.fft.cell_share + steps as f64 * 0.05),
            Part::TriggerChannel => vec![ScopeAction::TriggerLane],
            Part::Collapse => {
                self.set_collapsed(cx, !self.collapsed);
                let actions = vec![ScopeAction::Reconfigure(self.settings.clone())];
                self.emit(cx, actions);
                return;
            }
            Part::Slope => {
                let choices = [Slope::Rising, Slope::Falling, Slope::Either];
                let i = choices.iter().position(|p| *p == s.trigger.slope).unwrap_or(0);
                s.set_trigger_slope(choices[(i as i64 + i64::from(steps)).rem_euclid(3) as usize])
            }
            Part::Qualifier => s.set_trigger_qualifier(Qualifier::None),
            Part::Zone => s.set_zone(s.trigger.zone.map(|zone| Zone { must: !zone.must, ..zone })),
            Part::LevelMarker => {
                let scale = s.channels.first().map_or(1.0, |c| c.volts_per_div);
                s.set_trigger_level(s.trigger.level + f64::from(steps) * scale * 0.1)
            }
            Part::RuntLevel => {
                let Qualifier::Runt { other } = s.trigger.qualify else { return };
                let scale = s.channels.first().map_or(1.0, |c| c.volts_per_div);
                s.set_runt(Some(s.clamp_runt_level(other + f64::from(steps) * scale * 0.1)))
            }
            Part::PositionMarker => s.set_position(s.position + f64::from(steps) * 0.05),
            Part::Close => vec![ScopeAction::Close],
        };
        self.emit(cx, actions);
    }

    fn trigger_y(&self) -> f64 {
        self.level_y(self.settings.trigger.level)
    }

    fn level_y(&self, level: f64) -> f64 {
        let map = self.settings.map((1, 1));
        self.plot.pos.y + ((map.v_top - level) / map.v_px).clamp(0.0, 1.0) * self.plot.size.y
    }

    fn runt_level(&self) -> Option<f64> {
        self.runt_preview.filter(|_| self.drag.is_some_and(|drag| drag.part == Part::RuntLevel))
            .or_else(|| match self.settings.trigger.qualify { Qualifier::Runt { other } => Some(other), _ => None })
    }

    /// The destination ghost and committed level share this clamp in physical units.
    pub fn clamp_trigger_level(&self, level: f64) -> f64 {
        let map = self.settings.map((1, 1));
        self.settings.runt_clamped_level(ghost_level(level, map.v_top - map.v_px, map.v_top))
    }

    pub fn trigger_level_at(&self, cx: &Cx, pointer: DVec2, grab: f64) -> f64 {
        let y = self.plot.pos.y + self.area.rect(cx).pos.y - self.drawn_at.y;
        let fraction = ((pointer.y - grab - y) / self.plot.size.y.max(1.0)).clamp(0.0, 1.0);
        let map = self.settings.map((1, 1));
        self.clamp_trigger_level(map.v_top - fraction * map.v_px)
    }

    pub fn set_trigger_ghost(&mut self, cx: &mut Cx, level: Option<f64>) {
        let level = level.map(|level| self.clamp_trigger_level(level));
        if self.trigger_ghost != level { self.trigger_ghost = level; self.area.redraw(cx); }
    }

    fn position_x(&self) -> f64 {
        self.plot.pos.x + self.settings.position * self.plot.size.x
    }

    fn trace_channel(&self) -> Option<usize> {
        let s = &self.settings;
        if s.channels.get(s.trigger.channel).is_some_and(|c| c.on) {
            Some(s.trigger.channel)
        } else {
            s.channels.iter().position(|c| c.on)
        }
    }

    /// Only published measurement numbers are used for fits, never the sample record.
    pub fn measured_period(&self) -> Option<f64> {
        self.measurement("freq").filter(|f| *f > 0.0).map(|f| 1.0 / f).filter(|p| p.is_finite())
    }

    pub(super) fn measured_source_period(&self, source: usize) -> Option<f64> {
        if source == self.primary_source { return self.measured_period(); }
        self.secondary_measurements.iter().find(|row| row.lane == 0 && row.name == "freq")
            .map(|row| row.value).filter(|f| f.is_finite() && *f > 0.0).map(|f| 1.0 / f).filter(|p| p.is_finite())
    }

    pub fn set_time_fit_period(&mut self, period: Option<f64>) { self.time_fit_period = period; }

    fn measurement(&self, name: &str) -> Option<f64> {
        self.measurements.iter().find(|row| row.lane == 0 && row.name == name).map(|row| row.value).filter(|v| v.is_finite())
    }

    fn fitted_values(&self) -> Option<(f64, f64)> {
        if let (Some(min), Some(max)) = (self.measurement("min"), self.measurement("max")) {
            if max >= min { return Some(fit_values(min, max)); }
        }
        let mean = self.measurement("mean")?;
        let pp = self.measurement("Vpp").or_else(|| self.measurement("Ipp")).unwrap_or(0.0).max(0.0);
        Some(fit_values(mean - pp * 0.5, mean + pp * 0.5))
    }

    fn update_stretch(&mut self) {
        let Some(pixels) = self.plot_px() else { return };
        let to = self.settings.map(pixels);
        let from = self.phosphor.picture_map().unwrap_or(to);
        self.phosphor.set_stretch(Some(Stretch { from, to }));
    }

    /// One settings snapshot/action, no clear: the old image stays until matching hits arrive.
    fn apply_view(&mut self, cx: &mut Cx, window: Option<Window>, values: Option<(usize, f64, f64)>) {
        let changed = self.settings.set_view(window, values);
        if changed {
            self.update_stretch();
            cx.widget_action(self.uid, ScopeAction::Reconfigure(self.settings.clone()));
            self.area.redraw(cx);
            self.time_area.redraw(cx);
        }
    }

    fn fit_view(&mut self, cx: &mut Cx, part: Part) {
        let values = match part {
            Part::Scale(i) | Part::Plot(i) => self.fitted_values().map(|(scale, offset)| (i, scale, offset)),
            _ => None,
        };
        let window = matches!(part, Part::Time | Part::Frequency | Part::Plot(_)).then(|| {
            let period = if self.shared_time { self.time_fit_period } else { self.measured_period() };
            fit_time(period, self.settings.window())
        });
        self.apply_view(cx, window, values);
    }

    fn time_at(&self, pointer: DVec2, part: Part) -> f64 {
        let rect = if part == Part::Frequency { self.spectrum_plot } else if part == Part::Time {
            self.parts.iter().rev().find(|p| p.part == Part::Time && p.rect.contains(pointer)).map_or(self.plot, |p| p.rect)
        } else { self.plot };
        let fraction = ((pointer.x - rect.pos.x) / rect.size.x.max(1.0)).clamp(0.0, 1.0);
        let window = self.settings.window();
        -window.pre + fraction * (window.pre + window.post)
    }

    fn bracket_x(&self, t: f64) -> f64 {
        let window = self.settings.window();
        self.plot.pos.x + (t + window.pre) / (window.pre + window.post) * self.plot.size.x
    }

    fn bracket_end(&self, pointer: DVec2) -> Option<End> {
        if !self.bracket_visible || self.collapsed || !self.plot.contains(pointer) { return None; }
        let (t0, t1) = self.bracket?;
        bracket_end_at(pointer.x, self.bracket_x(t0), self.bracket_x(t1), 4.0)
    }

    fn bracket_time(&self, pointer: DVec2) -> f64 {
        let t = self.time_at(pointer, Part::Plot(0));
        let window = self.settings.window();
        let radius = (window.pre + window.post) * 8.0 / self.plot.size.x.max(1.0);
        let features = self.features.as_ref().and_then(|capture| capture.lanes.get(self.feature_lane));
        features.and_then(|features| snap(features, t, radius)).map_or(t, |feature| feature.t)
    }

    pub(super) fn prepare_delays(&mut self, count: usize, label_capacity: usize) {
        self.delay_marks.resize_with(count, DelayMark::default);
        for mark in &mut self.delay_marks {
            if mark.label.capacity() < label_capacity { mark.label.reserve(label_capacity - mark.label.len()); }
        }
    }

    pub(super) fn set_delay_enabled(&mut self, enabled: bool) {
        self.delay_enabled = enabled;
        if !enabled { self.delay_drag = false; }
    }

    pub(super) fn cancel_delay_drag(&mut self) { self.delay_drag = false; }

    pub(super) fn set_delay_mark(&mut self, cx: &mut Cx, index: usize, end: Option<End>, feature: Option<Feature>, label: &str) {
        let mark = &mut self.delay_marks[index];
        if mark.end == end && mark.feature == feature && mark.label == label { return; }
        mark.end = end; mark.feature = feature;
        mark.label.clear(); mark.label.push_str(label);
        self.area.redraw(cx);
    }

    /// Account for FLIP and scroll alignment even before the next lane event.
    pub(super) fn delay_plot(&self, cx: &Cx) -> Option<Rect> {
        if !self.delay_enabled || !self.bracket_visible || self.collapsed
            || self.plot.size.x <= 0.0 || self.plot.size.y <= 0.0 { return None; }
        Some(Rect { pos: self.plot.pos + self.area.rect(cx).pos - self.drawn_at, ..self.plot })
    }

    pub(super) fn delay_edge(&self, cx: &Cx, pointer: DVec2) -> Option<Feature> {
        let plot = self.delay_plot(cx)?;
        if !plot.contains(pointer) { return None; }
        let features = self.features.as_ref()?.lanes.get(self.feature_lane)?;
        let window = self.settings.window();
        let t = self.time_at(pointer - (plot.pos - self.plot.pos), Part::Plot(0));
        edge_near(features, None, t, (window.pre + window.post) * 8.0 / plot.size.x).copied()
    }

    pub(super) fn delay_point(&self, cx: &Cx, feature: Feature) -> Option<DVec2> {
        let plot = self.delay_plot(cx)?;
        let channel = self.trace_channel()?;
        Some(dvec2(self.bracket_x(feature.t), self.value_bracket_y(feature.v, channel)) + plot.pos - self.plot.pos)
    }

    pub(super) fn follow_delay(&self, kind: FeatureKind, t: f64, period: &mut f64) -> Option<Feature> {
        let features = self.features.as_ref()?.lanes.get(self.feature_lane)?;
        if let Some(measured) = self.measured_period() { *period = measured; }
        else if let Some(Geometry::Period { t0, t1 }) = self.answer_geometries[0] { *period = t1 - t0; }
        // A single edge has no period yet: keep its exact crossing until a period is known.
        let t = follow_edge(features, kind, t, *period)?;
        edge_near(features, Some(kind), t, 0.0).copied()
    }

    fn delay_end(&self, pointer: DVec2) -> Option<(usize, End, Feature)> {
        if !self.bracket_visible || self.collapsed || !self.plot.contains(pointer) { return None; }
        let mut best = None;
        let mut distance = 4.0;
        for (index, mark) in self.delay_marks.iter().enumerate() {
            if let (Some(end), Some(feature)) = (mark.end, mark.feature) {
                let d = (pointer.x - self.bracket_x(feature.t)).abs();
                if d <= distance { distance = d; best = Some((index, end, feature)); }
            }
        }
        best
    }

    fn edit_bracket(&mut self, cx: &mut Cx, bracket: Option<(f64, f64)>) {
        if self.bracket == bracket { return; }
        self.set_bracket(cx, bracket);
        // No temporary action Vec or label formatting on pointer moves.
        cx.widget_action(self.uid, ScopeAction::Bracket(bracket));
    }

    fn clear_bracket_drag(&mut self) {
        self.bracket_drag = None;
        self.bracket_off = false;
        self.bracket_press = None;
    }

    fn drag_bracket(&mut self, cx: &mut Cx, pointer: DVec2) {
        let Some(fixed) = self.bracket_drag else { return };
        self.bracket_off = pulled_off(self.plot, pointer, true);
        if !self.bracket_off {
            let t = self.bracket_time(pointer);
            self.edit_bracket(cx, Some((fixed.min(t), fixed.max(t))));
        }
        self.area.redraw(cx);
    }

    fn value_bracket_y(&self, v: f64, channel: usize) -> f64 {
        let (lo, hi) = self.value_span(channel);
        self.plot.pos.y + (hi - v) / (hi - lo) * self.plot.size.y
    }

    fn value_bracket_end(&self, pointer: DVec2) -> Option<End> {
        if self.collapsed || !self.plot.contains(pointer) { return None; }
        let (v0, v1) = self.value_bracket?;
        let channel = self.trace_channel()?;
        value_bracket_end_at(pointer.y, self.value_bracket_y(v0, channel), self.value_bracket_y(v1, channel))
    }

    fn bracket_value(&self, pointer: DVec2, channel: usize) -> f64 {
        let span = self.value_span(channel);
        let mut value = self.value_at(pointer, span);
        let points = self.plot.size.y / (span.1 - span.0);
        let mut distance = 6.0;
        if let Some(features) = self.features.as_ref().and_then(|capture| capture.lanes.get(self.feature_lane)) {
            for feature in features {
                if !matches!(feature.kind, FeatureKind::Top | FeatureKind::Base) { continue; }
                let y = self.plot.pos.y + (span.1 - feature.v) * points;
                let d = (pointer.y - y).abs();
                if d <= distance && y >= self.plot.pos.y && y <= self.plot.pos.y + self.plot.size.y {
                    value = feature.v;
                    distance = d;
                }
            }
        }
        value
    }

    fn edit_value_bracket(&mut self, cx: &mut Cx, bracket: Option<(f64, f64)>) {
        if bracket.is_none() { self.clear_value_bracket_drag(); }
        if self.value_bracket == bracket { return; }
        self.value_bracket = bracket;
        self.area.redraw(cx);
    }

    fn clear_value_bracket_drag(&mut self) {
        self.value_bracket_drag = None;
        self.value_bracket_off = false;
        self.value_bracket_axis = false;
        self.value_bracket_press = None;
    }

    fn drag_value_bracket(&mut self, cx: &mut Cx, pointer: DVec2) {
        let Some(fixed) = self.value_bracket_drag else { return };
        self.value_bracket_off = !self.value_bracket_axis && pulled_off(self.plot, pointer, false);
        if !self.value_bracket_off {
            if let Some(channel) = self.trace_channel() {
                let value = self.bracket_value(pointer, channel);
                self.edit_value_bracket(cx, Some((fixed.min(value), fixed.max(value))));
            }
        }
        self.area.redraw(cx);
    }

    fn value_span(&self, channel: usize) -> (f64, f64) {
        let map = self.settings.map_channel(channel, (1, 1)).unwrap_or_else(|| self.settings.map((1, 1)));
        (map.v_top - map.v_px, map.v_top)
    }

    fn value_at(&self, pointer: DVec2, span: (f64, f64)) -> f64 {
        let fraction = ((pointer.y - self.plot.pos.y) / self.plot.size.y.max(1.0)).clamp(0.0, 1.0);
        span.1 - fraction * (span.1 - span.0)
    }

    fn zoomed_time(window: Window, at: f64, factor: f64) -> Window {
        let scale = (window.pre + window.post) / 10.0;
        // Acquisition requires nonnegative pre/post. Stop zooming at the trigger boundary
        // instead of shifting the pointer's instant when it would leave that domain.
        let trigger_limit = if at < 0.0 { -at / (window.post - at) }
            else if at > 0.0 { at / (window.pre + at) } else { 0.0 };
        let factor = factor.clamp(DIV_MIN / scale, DIV_MAX / scale).max(trigger_limit);
        zoom_time_about(window, at, factor)
    }

    fn zoom_view(&mut self, cx: &mut Cx, part: Part, pointer: DVec2, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 || factor == 1.0 { return; }
        match part {
            Part::SpectrumScale => {
                let fft = self.settings.fft;
                let at = fft.ref_dbv - ((pointer.y - self.spectrum_plot.pos.y) / self.spectrum_plot.size.y.max(1.0)).clamp(0.0, 1.0) * 10.0 * fft.db_per_div;
                let (reference, scale) = zoom_db_about(fft.ref_dbv, fft.db_per_div, at, factor);
                let actions = self.settings.set_fft_view(reference, scale);
                self.emit(cx, actions);
            }
            Part::Scale(i) => {
                let span = self.value_span(i);
                let scale = (span.1 - span.0) / 8.0;
                let factor = factor.clamp(DIV_MIN / scale, DIV_MAX / scale);
                let span = zoom_value_about(span, self.value_at(pointer, span), factor);
                let scale = (span.1 - span.0) / 8.0;
                self.apply_view(cx, None, Some((i, scale, (span.0 * 0.5 + span.1 * 0.5) / scale)));
            }
            Part::Time | Part::Frequency => {
                let window = Self::zoomed_time(self.settings.window(), self.time_at(pointer, part), factor);
                self.apply_view(cx, Some(window), None);
            }
            _ => {}
        }
    }

    fn begin_view_drag(&mut self, part: Part, pointer: DVec2, free: bool) {
        let channel = match part { Part::Scale(i) | Part::Plot(i) => i, _ => self.trace_channel().unwrap_or(0) };
        let values = self.value_span(channel);
        let at = if matches!(part, Part::Time | Part::Frequency) { self.time_at(pointer, part) } else { self.value_at(pointer, values) };
        self.view_drag = Some(ViewDrag { press: pointer, last: pointer, window: self.settings.window(), values, at, axis: None, free });
    }

    fn drag_to(&mut self, cx: &mut Cx, pointer: DVec2, free: bool) {
        let Some(drag) = self.drag else { return };
        if drag.part == Part::Resize {
            let outcome = resize(drag.start, pointer.y - drag.grab, self.collapsed);
            if outcome.collapse != self.collapsed {
                self.drag = Some(Drag { grab: pointer.y, start: outcome.height, ..drag });
            }
            self.apply_resize(cx, outcome, false);
            return;
        }
        if let Some(press) = drag.pending {
            if lock_axis(pointer - press, 4.0, true).is_none() { return; }
            self.drag.as_mut().unwrap().pending = None;
        }
        if let Some(mut view) = self.view_drag {
            if pointer == view.last && (!free || view.free) { return; }
            view.last = pointer;
            let delta = pointer - view.press;
            view.free |= free;
            if view.free && view.axis.is_some() { view.axis = Some(Axis::Both); }
            else if view.axis.is_none() { view.axis = lock_axis(delta, 4.0, view.free); }
            self.view_drag = Some(view);
            let (mut window, mut values) = (None, None);
            match drag.part {
                Part::Plot(i) => {
                    let Some(axis) = view.axis else { return };
                    if matches!(axis, Axis::Horizontal | Axis::Both) {
                        let span = view.window.pre + view.window.post;
                        let pre = (view.window.pre + delta.x / self.plot.size.x.max(1.0) * span).clamp(0.0, span);
                        window = Some(Window { pre, post: span - pre });
                    }
                    if matches!(axis, Axis::Vertical | Axis::Both) {
                        let scale = (view.values.1 - view.values.0) / 8.0;
                        let offset = (view.values.0 * 0.5 + view.values.1 * 0.5) / scale + 8.0 * delta.y / self.plot.size.y.max(1.0);
                        values = Some((i, scale, offset));
                    }
                }
                Part::Scale(i) => {
                    let scale = (view.values.1 - view.values.0) / 8.0;
                    let factor = (delta.y / DRAG_ZOOM).clamp((DIV_MIN / scale).ln(), (DIV_MAX / scale).ln()).exp();
                    let span = zoom_value_about(view.values, view.at, factor);
                    let scale = (span.1 - span.0) / 8.0;
                    values = Some((i, scale, (span.0 * 0.5 + span.1 * 0.5) / scale));
                }
                Part::Time | Part::Frequency => {
                    let scale = (view.window.pre + view.window.post) / 10.0;
                    let factor = (-delta.x / DRAG_ZOOM).clamp((DIV_MIN / scale).ln(), (DIV_MAX / scale).ln()).exp();
                    window = Some(Self::zoomed_time(view.window, view.at, factor));
                }
                _ => return,
            }
            self.apply_view(cx, window, values);
            return;
        }
        let actions = match drag.part {
            Part::CellDivider => self.settings.set_cell_share(drag.start + (drag.grab - pointer.x) / drag.extent.max(1.0)),
            Part::Divider => {
                let share = drag_waterfall_share(drag.start, drag.grab - pointer.y, drag.extent);
                self.settings.set_waterfall_share(share)
            }
            Part::SpectrumOffset | Part::SpectrumScale => {
                let fft = self.settings.fft;
                let reference = drag_db_reference(drag.start, fft.db_per_div, pointer.y - drag.grab, self.spectrum_plot.size.y);
                self.settings.set_fft_view(reference, fft.db_per_div)
            }
            Part::Offset(channel) => {
                let offset = self.settings.offset_dragged(channel, drag.start, pointer.y - drag.grab, self.plot.size.y);
                let scale = self.settings.channels[channel].volts_per_div;
                self.apply_view(cx, None, Some((channel, scale, offset)));
                return;
            }
            Part::LevelMarker if self.plot.size.y > 0.0 => {
                let fraction = ((pointer.y - drag.grab - self.plot.pos.y) / self.plot.size.y).clamp(0.0, 1.0);
                let map = self.settings.map((1, 1));
                self.settings.set_trigger_level(map.v_top - fraction * map.v_px)
            }
            Part::RuntLevel if self.plot.size.y > 0.0 => {
                let fraction = ((pointer.y - drag.grab - self.plot.pos.y) / self.plot.size.y).clamp(0.0, 1.0);
                let map = self.settings.map((1, 1));
                let other = map.v_top - fraction * map.v_px;
                if self.runt_preview.is_some() { self.runt_preview = Some(other); return; }
                self.settings.set_runt(Some(self.settings.clamp_runt_level(other)))
            }
            Part::PositionMarker if self.plot.size.x > 0.0 => self.settings.set_position((pointer.x - drag.grab - self.plot.pos.x) / self.plot.size.x),
            _ => return,
        };
        self.emit(cx, actions);
    }

    fn part_at(&self, point: DVec2) -> Option<Part> {
        self.parts.iter().rev().find(|p| p.rect.contains(point)).map(|p| p.part).or_else(|| self.wrapped_answer_at(point))
    }

    fn pointer_cursor(&self, point: DVec2) -> MouseCursor {
        if self.bracket_drag.is_some() || self.value_bracket_drag.is_some() { return MouseCursor::Hand; }
        if self.zone_drag.is_some_and(|drag| drag.original.is_none()) { return MouseCursor::Crosshair; }
        let part = self.drag.map(|drag| drag.part).or_else(|| self.part_at(point));
        if part == Some(Part::Resize) { MouseCursor::RowResize }
        else if part == Some(Part::CellDivider) { MouseCursor::EwResize }
        else if part == Some(Part::RuntLevel) { MouseCursor::RowResize }
        else if part == Some(Part::Divider) { MouseCursor::NsResize }
        else if part.is_some() || self.plot.contains(point) || self.spectrum().is_some() && self.spectrum_plot.contains(point) { MouseCursor::Hand }
        else { MouseCursor::Default }
    }

    fn hovered_answer(&self) -> Option<usize> {
        let pointer = self.pointer?;
        match self.part_at(pointer) { Some(Part::Answer(index)) => Some(index), _ => None }
    }

    fn channel_color(&self, channel: usize) -> [f32; 4] {
        let index = if channel == 0 && self.overlay_pair() { self.sources[self.primary_source].channel.unwrap() }
            else { self.settings.channels.get(channel).map_or(0, |c| c.color_index) };
        InstrumentLook::for_theme(self.dark).channel(index)
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text(&mut self, cx: &mut Cx2d, pos: DVec2, text: &str, color: [f32; 4], numeric: bool) {
        let (draw, cache) = if self.axis_mode { (&mut self.draw_axis, &mut self.axis_cache) }
            else if numeric { (&mut self.draw_text, &mut self.text_cache) } else { (&mut self.draw_label, &mut self.label_cache) };
        let layout = cache.get(cx, draw, text).full.clone();
        draw.color = vec4(color[0], color[1], color[2], color[3]);
        let size = dvec2(f64::from(layout.size_in_lpxs.width * draw.font_scale), f64::from(layout.size_in_lpxs.height * draw.font_scale));
        draw.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos, size }), &layout);
    }

    /// Repack only when the displayed answers, preview, font, lane width or head controls change.
    fn layout_answers(&mut self, cx: &mut Cx2d, width: f64) {
        let preview = self.answer_drag_preview();
        let key = (self.answers_revision, width, self.collapsed, self.closable, preview);
        if self.answers_layout_key == Some(key) { return; }
        let class = AnswerWidthClass::from_width(width);
        let answers = std::mem::take(&mut self.answers);
        // Reserve every available harmonic's width before hovering; neither wrapping nor the
        // following answers change when the pointer selects a different order.
        self.harmonic_width = 0.0;
        let harmonic_answers = std::mem::take(&mut self.harmonic_answers);
        for answer in harmonic_answers.iter().flatten() { self.harmonic_width = self.harmonic_width.max(self.answer_width(cx, answer)); }
        self.harmonic_answers = harmonic_answers;
        let indices: Vec<_> = (0..answers.items.len()).filter(|&i| preview != Some((i, true))).collect();
        let widths: Vec<_> = indices.iter().map(|&i| self.answer_width(cx, &answers.items[i]) + self.distribution_space(i)).collect();
        let more_labels = std::mem::take(&mut self.more_labels);
        let more_width = more_labels.iter().skip(1).map(|text| self.text_width(cx, text, true)).fold(0.0, f64::max);
        self.more_labels = more_labels;
        let unjudged = self.unjudged_answers.len();
        let handles = if self.closable { 110.0 } else { 88.0 };
        let reserve = if unjudged > 0 || !widths.is_empty() && (self.collapsed || class == AnswerWidthClass::Medium) { 8.0 + more_width + MORE_PAD * 2.0 } else { 0.0 };
        self.answers_tag_width = (width * if self.sources.len() == 2 { 0.4 } else { 0.28 }).clamp(22.0, if self.sources.len() == 2 { 280.0 } else { 180.0 }).min((width - handles - reserve).max(0.0));
        let room = width - handles - self.answers_tag_width - 8.0
            - if unjudged > 0 { ANSWER_GAP + more_width + MORE_PAD * 2.0 } else { 0.0 };
        self.answers_spark = true;
        self.answers_layout = layout_answers(&widths, room - if self.collapsed { 104.0 } else { 0.0 }, class, self.collapsed, more_width);
        if self.collapsed && (self.answers_layout.more > 0 || unjudged > 0) {
            // The spark yields before any whole answer is moved into the popover.
            self.answers_spark = false;
            self.answers_layout = layout_answers(&widths, room, class, true, more_width);
        }
        if self.settings.domain == Domain::Fft && !self.collapsed && self.settings.answers.is_none() {
            // Automatic secondary peaks stay in the popover; explicit choices use normal packing.
            let primary_end = answers.items.iter().position(|a| a.name == "2nd ").unwrap_or(answers.items.len());
            let primary = indices.iter().take_while(|&&i| i < primary_end).count();
            let extra = widths.len() - primary;
            let reserve = if extra > 0 { ANSWER_GAP + more_width + MORE_PAD * 2.0 } else { 0.0 };
            self.answers_layout = layout_answers(&widths[..primary], room - reserve, class, false, more_width);
            self.answers_layout.more += extra;
            self.answers_layout.chip = self.answers_layout.more > 0;
        }
        self.answers_layout.more += unjudged;
        if unjudged > 0 { self.answers_layout.chip = true; }
        self.answers = answers;
        self.answer_widths = widths;
        self.answer_indices = indices;
        self.answers_layout_key = Some(key);
    }

    fn answer_width(&mut self, cx: &mut Cx2d, answer: &Answer) -> f64 {
        let width = self.text_width(cx, &answer.name, true) + self.text_width(cx, &answer.value, true)
            + self.text_width(cx, "\u{2009}", true) + self.text_width(cx, &answer.unit, true)
            + answer.marks.iter().map(|(_, mark)| self.text_width(cx, " ", true) + self.text_width(cx, mark, true)).sum::<f64>();
        if answer.unit == "dBc" { width.max(self.harmonic_width) } else { width }
    }

    fn text_width(&mut self, cx: &mut Cx2d, text: &str, numeric: bool) -> f64 {
        if self.axis_mode { self.axis_cache.get(cx, &self.draw_axis, text).width }
        else if numeric { self.text_cache.get(cx, &self.draw_text, text).width }
        else { self.label_cache.get(cx, &self.draw_label, text).width }
    }

    fn text_box(&mut self, cx: &mut Cx2d, rect: Rect, text: &str, color: [f32; 4], numeric: bool) {
        if rect.size.x <= 0.0 || text.is_empty() { return; }
        let (draw, cache) = if self.axis_mode { (&mut self.draw_axis, &mut self.axis_cache) }
            else if numeric { (&mut self.draw_text, &mut self.text_cache) } else { (&mut self.draw_label, &mut self.label_cache) };
        let layout = cache.get(cx, draw, text).fit(cx, draw, rect.size.x);
        if f64::from(layout.size_in_lpxs.width * draw.font_scale) > rect.size.x { return; }
        draw.color = vec4(color[0], color[1], color[2], color[3]);
        cx.push_clip_rect(rect);
        draw.draw_walk_laidout(cx, Walk::abs_rect(rect), &layout);
        cx.pop_clip_rect();
    }

    fn draw_part_feedback(&mut self, cx: &mut Cx2d, part: Part, rect: Rect) {
        let tk = Tokens::for_theme(self.dark);
        let hovered = self.pointer.is_some_and(|p| rect.contains(p));
        if hovered {
            self.quad(cx, rect, tk.part_fill);
        }
        let border = if self.focused == Some(part) {
            tk.selection
        } else if hovered {
            tk.hover
        } else {
            tk.block_frame
        };
        self.quad(
            cx,
            Rect {
                pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - 1.0),
                size: dvec2(rect.size.x, 1.0),
            },
            border,
        );
    }

    fn lane_zone(&self) -> Option<Zone> {
        let lane = if self.shared_time { self.zone_lane } else { self.trace_channel().unwrap_or(0) };
        self.settings.trigger.zone.filter(|zone| zone.valid() && (zone.lane == lane
            || self.overlay_pair() && self.sources.iter().any(|source| source.channel == Some(zone.lane))))
    }

    fn zone_rect(&self, zone: Zone) -> Option<Rect> {
        let channel = self.trace_channel()?;
        let a = dvec2(self.bracket_x(zone.t0), self.value_bracket_y(zone.v1, channel));
        let b = dvec2(self.bracket_x(zone.t1), self.value_bracket_y(zone.v0, channel));
        let size = b - a;
        [a.x, a.y, size.x, size.y].into_iter().all(f64::is_finite).then_some(Rect { pos: a, size })
    }

    fn begin_zone(&mut self, cx: &mut Cx, pointer: DVec2, original: Option<Zone>) {
        let Some(channel) = self.trace_channel() else { return };
        self.drag = None;
        self.view_drag = None;
        self.tag_press = None;
        self.trigger_press = None;
        self.focused = Some(Part::Zone);
        let lane = original.map(|zone| zone.lane).unwrap_or_else(|| if self.shared_time {
            self.sources.get(self.primary_source).and_then(|source| source.channel).unwrap_or(self.zone_lane)
        } else { channel });
        self.zone_drag = Some(ZoneDrag { press: pointer, pointer, original, lane,
            plot: self.plot, window: self.settings.window(), values: self.value_span(channel) });
        cx.set_cursor(if original.is_some() { MouseCursor::Hand } else { MouseCursor::Crosshair });
        self.area.redraw(cx);
    }

    fn draw_zone(&mut self, cx: &mut Cx2d) {
        if self.collapsed { return; }
        let Some(channel) = self.trace_channel() else { return };
        let ghost = self.zone_drag;
        let zone = ghost.and_then(ZoneDrag::preview).or_else(|| self.lane_zone());
        let Some(zone) = zone else { return };
        let Some(rect) = self.zone_rect(zone) else { return };
        let Some(shown) = super::semantics::intersect(rect, self.plot) else { return };
        let hue = self.channel_color(channel);
        let active = ghost.is_some() || self.focused == Some(Part::Zone) || self.pointer.is_some_and(|p| shown.contains(p));
        let ink = [hue[0], hue[1], hue[2], if active { 1.0 } else { 0.65 }];
        let px = 1.0 / cx.current_dpi_factor();
        cx.push_clip_rect(self.plot);
        for (origin, vertical) in [(rect.pos, false), (rect.pos + dvec2(0.0, rect.size.y), false),
            (rect.pos, true), (rect.pos + dvec2(rect.size.x, 0.0), true)] {
            // Step in visible screen coordinates, independent of the offscreen box's size.
            if vertical && (origin.x < self.plot.pos.x || origin.x > self.plot.pos.x + self.plot.size.x)
                || !vertical && (origin.y < self.plot.pos.y || origin.y > self.plot.pos.y + self.plot.size.y) { continue; }
            let base = if vertical { origin.y } else { origin.x };
            let start = if vertical { shown.pos.y } else { shown.pos.x };
            let end = if vertical { shown.pos.y + shown.size.y } else { shown.pos.x + shown.size.x };
            let mut at = if zone.must { start } else { start - (start.rem_euclid(8.0) - base.rem_euclid(8.0)).rem_euclid(8.0) };
            while at < end {
                let lo = at.max(start);
                let hi = if zone.must { end } else { (at + 4.0).min(end) };
                if hi > lo {
                    self.quad(cx, Rect { pos: if vertical { dvec2(origin.x - px * 0.5, lo) } else { dvec2(lo, origin.y - px * 0.5) },
                        size: if vertical { dvec2(px, hi - lo) } else { dvec2(hi - lo, px) } }, ink);
                }
                if zone.must { break; }
                at += 8.0;
            }
        }
        if ghost.is_none() {
            let label = if zone.must { "must cross" } else { "must not cross" };
            let label_rect = Rect { pos: rect.pos + dvec2(4.0, 3.0), size: dvec2((rect.size.x - 8.0).max(0.0).min(self.text_width(cx, label, true)), (rect.size.y - 6.0).clamp(0.0, 19.0)) };
            if super::semantics::intersect(label_rect, self.plot).is_some() { self.text_box(cx, label_rect, label, InstrumentLook::for_theme(self.dark).text_tertiary, true); }
            self.parts.push(PartRect { part: Part::Zone, rect: shown, selected: zone.must });
        }
        cx.pop_clip_rect();
    }

    fn draw_bracket(&mut self, cx: &mut Cx2d) {
        if !self.bracket_visible || self.collapsed { return; }
        let Some((t0, t1)) = self.bracket else { return };
        let Some(channel) = self.trace_channel() else { return };
        let p = self.plot;
        let (x0, x1) = (self.bracket_x(t0), self.bracket_x(t1));
        if x1 < p.pos.x || x0 > p.pos.x + p.size.x { return; }
        let color = self.channel_color(channel);
        let alpha = if self.bracket_off { 0.3 } else { 1.0 };
        let look = InstrumentLook::for_theme(self.dark);
        cx.push_clip_rect(p);
        let (left, right) = (x0.max(p.pos.x), x1.min(p.pos.x + p.size.x));
        self.quad(cx, Rect { pos: dvec2(left, p.pos.y), size: dvec2(right - left, p.size.y) }, [color[0], color[1], color[2], 0.08 * alpha]);
        for x in [x0, x1] {
            self.quad(cx, Rect { pos: dvec2(x - 0.5, p.pos.y), size: dvec2(1.0, p.size.y) }, [color[0], color[1], color[2], 0.55 * alpha]);
        }
        let unit = self.settings.channels.get(channel).map_or("V", |c| c.unit.as_str());
        self.bracket_label.update(t1 - t0, self.span_stats, self.span_answered, unit);
        let label = std::mem::take(&mut self.bracket_label.text);
        let size = dvec2((self.text_width(cx, &label, true) + GAP * 2.0).min(p.size.x), 22.0f64.min(p.size.y));
        let rect = Rect { pos: dvec2(left.clamp(p.pos.x, p.pos.x + p.size.x - size.x), p.pos.y), size };
        let bg = look.canvas.canvas;
        self.draw_more.color = vec4(bg[0], bg[1], bg[2], 0.92);
        self.draw_more.draw_abs(cx, rect);
        self.text_box(cx, Rect { pos: rect.pos + dvec2(GAP, 3.0), size: dvec2((size.x - GAP * 2.0).max(0.0), (size.y - 3.0).max(0.0)) }, &label, look.text_secondary, true);
        self.bracket_label.text = label;
        cx.pop_clip_rect();
    }

    fn draw_value_bracket(&mut self, cx: &mut Cx2d) {
        if self.collapsed { return; }
        let Some((v0, v1)) = self.value_bracket else { return };
        let Some(channel) = self.trace_channel() else { return };
        let p = self.plot;
        let (y0, y1) = (self.value_bracket_y(v1, channel), self.value_bracket_y(v0, channel));
        if y1 < p.pos.y || y0 > p.pos.y + p.size.y { return; }
        let color = self.channel_color(channel);
        let alpha = if self.value_bracket_off { 0.3 } else { 1.0 };
        let line = [color[0], color[1], color[2], 0.55 * alpha];
        let look = InstrumentLook::for_theme(self.dark);
        let x = p.pos.x + (p.size.x - 6.0).max(0.0);
        cx.push_clip_rect(p);
        for y in [y0, y1] {
            self.quad(cx, Rect { pos: dvec2(p.pos.x, y - 0.5), size: dvec2(p.size.x, 1.0) }, line);
        }
        self.quad(cx, Rect { pos: dvec2(x - 0.5, y0), size: dvec2(1.0, y1 - y0) }, line);
        // Small solid arrowheads from the existing quads; no additional shader or geometry.
        let arrow = ((y1 - y0) * 0.5).min(4.0);
        for step in 0..4 {
            let d = step as f64;
            if d >= arrow { break; }
            for y in [y0 + d, y1 - d - 1.0] {
                self.quad(cx, Rect { pos: dvec2(x - d - 0.5, y), size: dvec2(d * 2.0 + 1.0, (arrow - d).min(1.0)) }, line);
            }
        }
        let unit = self.settings.channels.get(channel).map_or("V", |c| c.unit.as_str());
        self.value_bracket_label.update(v0, v1, unit);
        let label = std::mem::take(&mut self.value_bracket_label.text);
        let size = dvec2((self.text_width(cx, &label, true) + GAP * 2.0).min((p.size.x - 16.0).max(0.0)), 22.0f64.min(p.size.y));
        let rect = Rect { pos: dvec2((x - 8.0 - size.x).max(p.pos.x),
            ((y0 + y1 - size.y) * 0.5).clamp(p.pos.y, p.pos.y + p.size.y - size.y)), size };
        let bg = look.canvas.canvas;
        self.draw_more.color = vec4(bg[0], bg[1], bg[2], 0.92);
        self.draw_more.draw_abs(cx, rect);
        self.text_box(cx, Rect { pos: rect.pos + dvec2(GAP, 3.0), size: dvec2((size.x - GAP * 2.0).max(0.0), (size.y - 3.0).max(0.0)) }, &label, look.text_secondary, true);
        self.value_bracket_label.text = label;
        cx.pop_clip_rect();
    }

    fn draw_bracket_remove(&mut self, cx: &mut Cx2d) {
        let removing = self.bracket_off || self.value_bracket_off;
        if !removing && self.bracket_remove_overlay.is_none() { return; }
        let mut overlay = self.bracket_remove_overlay.take().unwrap_or_else(|| DrawList2d::new(cx));
        // The pointer can be outside the card and its parent's clip. Empty once on return/drop.
        overlay.begin_overlay_last(cx);
        if let Some(pointer) = self.pointer.filter(|_| removing) {
            let pass = cx.current_pass_size();
            cx.begin_root_turtle(pass, Layout::default());
            let size = dvec2(self.text_width(cx, "remove", true) + 12.0, 22.0);
            let x = if pointer.x + 12.0 + size.x > pass.x { pointer.x - 12.0 - size.x } else { pointer.x + 12.0 };
            let y = if pointer.y + 12.0 + size.y > pass.y { pointer.y - 12.0 - size.y } else { pointer.y + 12.0 };
            let rect = Rect { pos: dvec2(x, y), size };
            self.quad(cx, rect, Tokens::for_theme(self.dark).block_fill);
            self.text(cx, rect.pos + dvec2(6.0, 3.0), "remove", InstrumentLook::for_theme(self.dark).text, true);
            cx.end_pass_sized_turtle();
        }
        overlay.end(cx);
        if removing { self.bracket_remove_overlay = Some(overlay); }
    }

    fn draw_delays(&mut self, cx: &mut Cx2d) {
        if !self.bracket_visible || self.collapsed { return; }
        let Some(channel) = self.trace_channel() else { return };
        let p = self.plot;
        let color = self.channel_color(channel);
        let look = InstrumentLook::for_theme(self.dark);
        let mut row = usize::from(self.bracket.is_some());
        cx.push_clip_rect(p);
        for index in 0..self.delay_marks.len() {
            let mark = &self.delay_marks[index];
            if mark.end.is_none() { continue; }
            let feature = mark.feature;
            if let Some(feature) = feature {
                let at = dvec2(self.bracket_x(feature.t), self.value_bracket_y(feature.v, channel));
                self.quad(cx, Rect { pos: dvec2(at.x - 0.5, p.pos.y), size: dvec2(1.0, p.size.y) }, [color[0], color[1], color[2], 0.65]);
                self.draw_tag.side = LaneSide::Sim;
                self.draw_tag.color = vec4(color[0], color[1], color[2], color[3]);
                self.draw_tag.draw_abs(cx, Rect { pos: at - dvec2(4.5, 4.5), size: dvec2(9.0, 9.0) });
            }
            let label = std::mem::take(&mut self.delay_marks[index].label);
            if !label.is_empty() {
                let size = dvec2((self.text_width(cx, &label, true) + GAP * 2.0).min(p.size.x), 22.0);
                let x = feature.map_or(p.pos.x, |feature| self.bracket_x(feature.t));
                let rect = Rect { pos: dvec2(x.clamp(p.pos.x, p.pos.x + p.size.x - size.x), p.pos.y + row as f64 * 22.0), size };
                let bg = look.canvas.canvas;
                self.draw_more.color = vec4(bg[0], bg[1], bg[2], 0.92);
                self.draw_more.draw_abs(cx, rect);
                self.text_box(cx, Rect { pos: rect.pos + dvec2(GAP, 3.0), size: dvec2((size.x - GAP * 2.0).max(0.0), 19.0) }, &label, look.text_secondary, true);
                row += 1;
            }
            self.delay_marks[index].label = label;
        }
        cx.pop_clip_rect();
    }

    fn draw_capture_overlay(&mut self, cx: &mut Cx2d) {
        self.draw_bracket(cx);
        self.draw_value_bracket(cx);
        self.draw_delays(cx);
        if self.zone_drag.is_some() || self.collapsed || self.drag.is_some() || self.view_drag.is_some()
            || self.bracket_drag.is_some() || self.value_bracket_drag.is_some() || self.delay_drag
            || self.pointer.is_some_and(|p| self.delay_end(p).is_some())
            || self.pointer.is_some_and(|p| self.value_bracket_end(p).is_some())
            || self.tag_press.is_some() || self.trigger_press.is_some() || self.holdoff_grab.is_some()
            || self.trigger_ghost.is_some() || cx.fingers.any_areas_captured() || cx.drag_visual().is_some() { return; }
        let Some(features) = self.features.as_ref().and_then(|capture| capture.lanes.get(self.feature_lane)) else { return };
        let Some(channel) = self.trace_channel() else { return };
        let p = self.plot;
        let map = self.settings.map_channel(channel, (1, 1)).unwrap_or_else(|| self.settings.map((1, 1)));
        let x = |t: f64| p.pos.x + (t - map.t_left) / map.t_px * p.size.x;
        let y = |v: f64| p.pos.y + (map.v_top - v) / map.v_px * p.size.y;
        let look = InstrumentLook::for_theme(self.dark);
        let color = self.channel_color(channel);
        let line = [color[0], color[1], color[2], 0.55];
        let geometry = self.hovered_answer().filter(|_| self.sources.len() < 2).and_then(|index| self.answer_names.get(index).copied().flatten())
            .and_then(|name| ["freq", "duty", "Vpp", "rise"].iter().position(|&candidate| candidate == name))
            .and_then(|index| self.answer_geometries[index]);
        // A draw uses one search in the shared feature list, never a sample scan or a feed lock.
        let reticle = self.pointer.filter(|pointer| p.contains(*pointer)).map(|pointer| {
            let t = self.time_at(pointer, Part::Plot(channel));
            let v = self.value_at(pointer, self.value_span(channel));
            (pointer, t, v, snap(features, t, map.t_px * 8.0 / p.size.x.max(1.0)).copied())
        });
        cx.push_clip_rect(p);
        if let Some(geometry) = geometry {
            match geometry {
                Geometry::Period { t0, t1 } | Geometry::High { t0, t1 } => {
                    let (x0, x1, y) = (x(t0), x(t1), p.pos.y + 12.0);
                    self.quad(cx, Rect { pos: dvec2(x0, y), size: dvec2(x1 - x0, 1.0) }, line);
                    for x in [x0, x1] { self.quad(cx, Rect { pos: dvec2(x, y - 4.0), size: dvec2(1.0, 9.0) }, line); }
                }
                Geometry::Levels { top, base } => {
                    for v in [top, base] { self.quad(cx, Rect { pos: dvec2(p.pos.x, y(v)), size: dvec2(p.size.x, 1.0) }, line); }
                }
                Geometry::Edge { t10, t90, v10, v90 } => {
                    for (t, v, label) in [(t10, v10, "10 %"), (t90, v90, "90 %")] {
                        let at = dvec2(x(t), y(v));
                        self.quad(cx, Rect { pos: at - dvec2(4.0, 0.5), size: dvec2(9.0, 1.0) }, line);
                        let width = self.text_width(cx, label, true).min(p.size.x);
                        let pos = dvec2((at.x + 7.0).clamp(p.pos.x, p.pos.x + p.size.x - width),
                            (at.y - 8.0).clamp(p.pos.y, p.pos.y + (p.size.y - 16.0).max(0.0)));
                        if p.contains(at) { self.text(cx, pos, label, look.text_secondary, true); }
                    }
                }
            }
        }
        if let Some((pointer, t, v, feature)) = reticle {
            let (t, v) = feature.map_or((t, v), |f| (f.t, f.v));
            let at = dvec2(x(t), y(v));
            self.quad(cx, Rect { pos: dvec2(at.x, p.pos.y), size: dvec2(1.0, p.size.y) }, line);
            // The tag's 4-point rounded outline is a circle at this size; reuse its shader.
            self.draw_tag.side = LaneSide::Sim;
            self.draw_tag.color = vec4(color[0], color[1], color[2], color[3]);
            self.draw_tag.draw_abs(cx, Rect { pos: at - dvec2(4.5, 4.5), size: dvec2(9.0, 9.0) });
            let unit = self.settings.channels.get(channel).map_or("V", |c| c.unit.as_str());
            self.reticle_label.update(feature.as_ref(), t, v, unit);
            let label = std::mem::take(&mut self.reticle_label.text);
            let size = dvec2((self.text_width(cx, &label, true) + GAP * 2.0).min(p.size.x), 22.0f64.min(p.size.y));
            let left = if pointer.x + 12.0 + size.x > p.pos.x + p.size.x { pointer.x - 12.0 - size.x } else { pointer.x + 12.0 };
            let top = if pointer.y + 12.0 + size.y > p.pos.y + p.size.y { pointer.y - 12.0 - size.y } else { pointer.y + 12.0 };
            let rect = Rect { pos: dvec2(left.clamp(p.pos.x, p.pos.x + p.size.x - size.x), top.clamp(p.pos.y, p.pos.y + p.size.y - size.y)), size };
            let bg = look.canvas.canvas;
            self.draw_more.color = vec4(bg[0], bg[1], bg[2], 0.92);
            self.draw_more.draw_abs(cx, rect);
            self.text_box(cx, Rect { pos: rect.pos + dvec2(GAP, 3.0), size: dvec2((size.x - GAP * 2.0).max(0.0), (size.y - 3.0).max(0.0)) }, &label, look.text_secondary, true);
            self.reticle_label.text = label;
        }
        cx.pop_clip_rect();
    }

    fn draw_eye_marks(&mut self, cx: &mut Cx2d) {
        let Some(marks) = self.eye_marks else { return };
        let channel = self.settings.trigger.channel;
        if !self.settings.channels.get(channel).is_some_and(|c| c.on) { return; }
        if !marks.ui.is_finite() || marks.ui <= 0.0
            || !marks.top.is_finite() || !marks.bottom.is_finite() || !marks.threshold.is_finite()
            || !marks.jitter_pp.is_finite() || marks.jitter_pp < 0.0
        {
            return;
        }
        let p = self.plot;
        let map = self.settings.map((1, 1));
        let x = |time: f64| (p.pos.x + (time - map.t_left) / map.t_px * p.size.x).round();
        let y = |value: f64| (p.pos.y + (map.v_top - value) / map.v_px * p.size.y).round();
        let mut k = (map.t_left / marks.ui - 0.5).ceil();
        // Exclude the right edge so a centre on the boundary belongs to just one screen.
        let end = ((map.t_left + map.t_px) / marks.ui - 0.5).ceil();
        if !k.is_finite() || !end.is_finite() || k >= end {
            return;
        }
        let nearest = ((map.t_left + map.t_px * 0.5) / marks.ui - 0.5).round().clamp(k, end - 1.0);
        let top = y(marks.top);
        let bottom = y(marks.bottom);
        let threshold = y(marks.threshold);
        let open = marks.jitter_pp < marks.ui;
        let height_label = std::mem::take(&mut self.eye_height);
        let mut color = self.channel_color(channel);
        color[3] = 0.9;
        cx.push_clip_rect(p);
        while k < end {
            let centre = x((k + 0.5) * marks.ui);
            self.quad(cx, Rect { pos: dvec2(centre, top.min(bottom)), size: dvec2(1.0, (bottom - top).abs()) }, color);
            for edge in [top, bottom] {
                self.quad(cx, Rect { pos: dvec2(centre - 3.0, edge), size: dvec2(7.0, 1.0) }, color);
            }
            if open {
                let left = x(k * marks.ui + marks.jitter_pp * 0.5);
                let right = x((k + 1.0) * marks.ui - marks.jitter_pp * 0.5);
                self.quad(cx, Rect { pos: dvec2(left, threshold), size: dvec2(right - left, 1.0) }, color);
                for edge in [left, right] {
                    self.quad(cx, Rect { pos: dvec2(edge, threshold - 3.0), size: dvec2(1.0, 7.0) }, color);
                }
            }
            let next = k + 1.0;
            if next <= k {
                break;
            }
            k = next;
        }
        let centre = x((nearest + 0.5) * marks.ui);
        let height = ROW - 6.0;
        let width = self.text_width(cx, &height_label, true) + GAP * 2.0;
        self.draw_eye_label(
            cx,
            // A third of the way down the height line, clear of the width label above the threshold.
            Rect { pos: dvec2(centre + GAP, top.min(bottom) + (bottom - top).abs() / 3.0 - height * 0.5), size: dvec2(width, height) },
            &height_label,
            color,
        );
        if open {
            let label = std::mem::take(&mut self.eye_width);
            let width = self.text_width(cx, &label, true) + GAP * 2.0;
            self.draw_eye_label(
                cx,
                Rect { pos: dvec2(centre - width * 0.5, threshold - height - 3.0), size: dvec2(width, height) },
                &label,
                color,
            );
            self.eye_width = label;
        }
        self.eye_height = height_label;
        cx.pop_clip_rect();
    }

    fn draw_eye_label(&mut self, cx: &mut Cx2d, rect: Rect, label: &str, color: [f32; 4]) {
        let mut backdrop = Tokens::for_theme(self.dark).canvas;
        backdrop[3] = 0.75;
        self.quad(cx, rect, backdrop);
        self.text(cx, rect.pos + dvec2(GAP, 2.0), label, color, true);
    }

    fn value_axis(&self, height: f64, line_height: f64, spectrum: bool) -> ValueAxis {
        let channel = self.trace_channel().unwrap_or(self.settings.trigger.channel);
        let (lo, hi, scale, unit) = if spectrum {
            let fft = self.settings.fft;
            (fft.ref_dbv - 10.0 * fft.db_per_div, fft.ref_dbv, 1.0, "dBV".into())
        } else {
            let map = self.settings.map_channel(channel, (1, 1)).unwrap_or_else(|| self.settings.map((1, 1)));
            let (lo, hi) = (map.v_top - map.v_px, map.v_top);
            let unit = self.settings.channels.get(channel).map_or("V", |c| c.unit.as_str());
            let (scale, unit) = axis_prefix(lo, hi, unit);
            (lo, hi, scale, unit)
        };
        let ticks = linear_ticks(lo, hi, height, line_height, &|value, step| axis_label(value, step, scale), &mut |_| line_height);
        ValueAxis { ticks, lo, hi, unit }
    }

    fn scale_label(&self, channel: usize) -> String {
        let c = &self.settings.channels[channel];
        let lo = (c.offset_div - 4.0) * c.volts_per_div;
        let hi = (c.offset_div + 4.0) * c.volts_per_div;
        // A stretched scale is continuous, not a 1-2-5 step: four significant figures, each end in its own prefix.
        format!("{} to {}", eng(lo, &c.unit, 4), eng(hi, &c.unit, 4))
    }

    fn ground_label(&self, channel: usize) -> String {
        let c = &self.settings.channels[channel];
        format!("ground {}", eng(c.offset_div * c.volts_per_div, &c.unit, 4))
    }

    fn axis_part(&mut self, cx: &mut Cx2d, part: Part, rect: Rect) {
        if self.focused == Some(part) || !matches!(part, Part::Divider | Part::CellDivider) && self.pointer.is_some_and(|p| rect.contains(p)) {
            self.draw_part_feedback(cx, part, rect);
        }
        self.parts.push(PartRect { part, rect, selected: false });
    }

    fn draw_value_axis(&mut self, cx: &mut Cx2d, axis: &ValueAxis, column: Rect, line_height: f64, spectrum: bool) {
        let p = if spectrum { self.spectrum_plot } else { self.plot };
        let look = InstrumentLook::for_theme(self.dark);
        let px = 1.0 / cx.current_dpi_factor();
        let ground_width = if !spectrum { self.settings.channels.len().max(1) as f64 * 12.0 } else { 0.0 };
        let right = p.pos.x - ground_width - GAP;
        let channel = self.trace_channel().unwrap_or(self.settings.trigger.channel);
        if channel < self.settings.channels.len() {
            self.axis_part(cx, if spectrum { Part::SpectrumScale } else { Part::Scale(channel) }, column);
        }
        let width = self.text_width(cx, &axis.unit, true);
        if spectrum && channel < self.settings.channels.len() {
            self.axis_part(cx, Part::SpectrumOffset, Rect { pos: dvec2(column.pos.x, p.pos.y - line_height - GAP), size: dvec2(column.size.x, line_height + GAP) });
        }
        self.text(cx, dvec2(right - width, p.pos.y - line_height - GAP), &axis.unit, look.text_secondary, true);
        for tick in &axis.ticks {
            let y = p.pos.y + p.size.y - tick.at;
            self.quad(cx, Rect { pos: dvec2(p.pos.x, (y - px * 0.5).clamp(p.pos.y, (p.pos.y + p.size.y - px).max(p.pos.y))), size: dvec2(p.size.x, px) }, if tick.value == 0.0 { look.zero_line } else { look.hairline });
            let width = self.text_width(cx, &tick.label, true);
            let y = (y - line_height * 0.5).clamp(p.pos.y, (p.pos.y + p.size.y - line_height).max(p.pos.y));
            self.text_box(cx, Rect { pos: dvec2((right - width).max(column.pos.x), y), size: dvec2(width.min((right - column.pos.x).max(0.0)), line_height) }, &tick.label, look.text_tertiary, true);
        }
        // Zero is a reference even if the current tick density leaves it without a label.
        if axis.lo <= 0.0 && axis.hi >= 0.0 && !axis.ticks.iter().any(|tick| tick.value == 0.0) {
            let y = p.pos.y + axis.hi / (axis.hi - axis.lo) * p.size.y;
            self.quad(cx, Rect { pos: dvec2(p.pos.x, y - px * 0.5), size: dvec2(p.size.x, px) }, look.zero_line);
        }
    }

    fn frequency_line(&mut self, cx: &mut Cx2d, tick: &Tick, p: Rect) {
        let look = InstrumentLook::for_theme(self.dark);
        let px = 1.0 / cx.current_dpi_factor();
        let x = (p.pos.x + tick.at - px * 0.5).clamp(p.pos.x, (p.pos.x + p.size.x - px).max(p.pos.x));
        self.quad(cx, Rect { pos: dvec2(x, p.pos.y), size: dvec2(px, p.size.y) }, if tick.value == 0.0 { look.zero_line } else { look.hairline });
    }

    /// The time plot's x range in drawing coordinates, as last laid out (None before the first draw). A
    /// vertical scroll leaves it unchanged, so it holds for a card scrolled out of the bench's viewport.
    pub fn time_plot_x(&self) -> Option<(f64, f64)> {
        self.time_plot_drawn.then_some((self.plot.pos.x, self.plot.size.x))
    }

    /// U1's time row, also used once below the bench's scroll view. Tick layout and strings are
    /// retained until axes or width change; drawing an unchanged row allocates nothing.
    pub fn draw_shared_time_axis(&mut self, cx: &mut Cx2d, rect: Rect) {
        self.axis_mode = true;
        cx.add_rect_area(&mut self.time_area, rect);
        let plot = self.plot;
        std::mem::swap(&mut self.time_ticks, &mut self.shared_ticks);
        std::mem::swap(&mut self.time_key, &mut self.shared_key);
        std::mem::swap(&mut self.time_note, &mut self.shared_note);
        self.plot = cell_layout(Rect { pos: rect.pos + dvec2(64.0, 0.0), size: dvec2((rect.size.x - 108.0).max(1.0), 0.0) }, self.spectrum().is_some(), self.collapsed, self.settings.fft.cell_share).0;
        self.draw_time_axis(cx, rect.size.y, false);
        self.draw_holdoff(cx, rect.size.y);
        if self.drag.is_some_and(|drag| drag.part == Part::Time) { self.draw_drag_feedback(cx); }
        std::mem::swap(&mut self.time_ticks, &mut self.shared_ticks);
        std::mem::swap(&mut self.time_key, &mut self.shared_key);
        std::mem::swap(&mut self.time_note, &mut self.shared_note);
        self.plot = plot;
        self.axis_mode = false;
    }

    /// Over the stretch of the newest waveform that repeats a steady source's values: a hairline
    /// along the plot's top and the word "held" at its right end, so a held trace never reads as
    /// observed.
    fn draw_held(&mut self, cx: &mut Cx2d) {
        let Some(from) = self.phosphor.held() else { return };
        let p = self.plot;
        let window = self.settings.window();
        let span = window.pre + window.post;
        if span <= 0.0 || p.size.x <= 0.0 { return; }
        let x0 = (p.pos.x + (window.pre + from) / span * p.size.x).clamp(p.pos.x, p.pos.x + p.size.x);
        let look = InstrumentLook::for_theme(self.dark);
        let ink = look.text_tertiary;
        self.quad(cx, Rect { pos: dvec2(x0, p.pos.y + 1.0), size: dvec2((p.pos.x + p.size.x - x0).max(1.0), 1.0) }, [ink[0], ink[1], ink[2], 0.6]);
        let width = self.text_width(cx, "held", false);
        self.text(cx, dvec2(p.pos.x + p.size.x - width - 4.0, p.pos.y + 4.0), "held", ink, false);
    }

    /// Over the stretch of the newest waveform stepped below the simulation's time resolution: a
    /// hairline along the plot's bottom and the words "below res" at its left end, so a damped or
    /// skipped detail never reads as a confident flat line (zoom in, or Exact, resolves it).
    fn draw_below(&mut self, cx: &mut Cx2d) {
        let Some((from, to)) = self.phosphor.below() else { return };
        let p = self.plot;
        let window = self.settings.window();
        let span = window.pre + window.post;
        if span <= 0.0 || p.size.x <= 0.0 { return; }
        let x = |t: f64| (p.pos.x + (window.pre + t) / span * p.size.x).clamp(p.pos.x, p.pos.x + p.size.x);
        let (x0, x1) = (x(from), x(to));
        let look = InstrumentLook::for_theme(self.dark);
        let ink = look.text_tertiary;
        let y = p.pos.y + p.size.y - 2.0;
        self.quad(cx, Rect { pos: dvec2(x0, y), size: dvec2((x1 - x0).max(1.0), 1.0) }, [ink[0], ink[1], ink[2], 0.6]);
        let width = self.text_width(cx, "below res", false);
        self.text(cx, dvec2(x0.min(p.pos.x + p.size.x - width - 4.0).max(p.pos.x + 4.0), y - 14.0), "below res", ink, false);
    }

    fn draw_holdoff(&mut self, cx: &mut Cx2d, height: f64) {
        let p = self.plot;
        let window = self.settings.window();
        let span = window.pre + window.post;
        let zero = p.pos.x + window.pre / span * p.size.x;
        let edge = zero + self.settings.trigger.holdoff.min(window.post) / span * p.size.x;
        let ink = self.channel_color(0);
        self.holdoff_axis = Rect { pos: dvec2(zero, p.pos.y), size: dvec2(p.size.x, height) };
        // The band and its edge tab keep to the row's top gap, above the labels: a zero holdoff never
        // covers the "0". The edge's hit area is the row's full height.
        let top = GAP.min(height);
        self.quad(cx, Rect { pos: dvec2(zero, p.pos.y), size: dvec2((edge - zero).max(3.0), top - 1.0) }, [ink[0], ink[1], ink[2], 0.35]);
        self.quad(cx, Rect { pos: dvec2(edge - 1.0, p.pos.y), size: dvec2(3.0, top) }, [ink[0], ink[1], ink[2], 0.85]);
        let rect = Rect { pos: dvec2(edge - 4.0, p.pos.y), size: dvec2(11.0, height) };
        cx.add_rect_area(&mut self.holdoff_area, rect);
        self.axis_part(cx, Part::Holdoff, rect);
        if self.holdoff_grab.is_some() {
            let note = std::mem::take(&mut self.holdoff_note);
            let width = self.text_width(cx, &note, true) + 12.0;
            let x = edge.min((p.pos.x + p.size.x - width).max(p.pos.x));
            let rect = Rect { pos: dvec2(x, p.pos.y - ROW), size: dvec2(width, ROW) };
            self.quad(cx, rect, Tokens::for_theme(self.dark).block_fill);
            self.text(cx, rect.pos + dvec2(6.0, 5.0), &note, ink, true);
            self.holdoff_note = note;
        }
    }

    fn drag_holdoff(&mut self, cx: &mut Cx, x: f64) {
        let Some(grab) = self.holdoff_grab else { return };
        let window = self.settings.window();
        let holdoff = holdoff_drag(x - grab - self.holdoff_axis.pos.x, self.holdoff_axis.size.x, window.pre + window.post, window.post);
        self.holdoff_note = format!("holdoff {}", si_label(holdoff, "s"));
        let actions = self.settings.set_holdoff(holdoff);
        self.emit(cx, actions);
        self.time_area.redraw(cx);
    }

    /// A separate edge area leaves the time-axis gesture and its parts untouched.
    fn handle_holdoff(&mut self, cx: &mut Cx, event: &Event) -> bool {
        match event.hits(cx, self.holdoff_area) {
            Hit::Nothing => return false,
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                self.focused = Some(Part::Holdoff);
                cx.set_key_focus_from_user(self.area);
                let window = self.settings.window();
                let edge = self.holdoff_axis.pos.x + self.settings.trigger.holdoff.min(window.post) / (window.pre + window.post) * self.holdoff_axis.size.x;
                self.holdoff_grab = Some(e.abs.x - edge);
                self.drag_holdoff(cx, e.abs.x);
            }
            Hit::FingerMove(e) => self.drag_holdoff(cx, e.abs.x),
            Hit::FingerUp(e) => { self.drag_holdoff(cx, e.abs.x); self.holdoff_grab = None; self.time_area.redraw(cx); }
            _ => {}
        }
        true
    }

    fn draw_time_axis(&mut self, cx: &mut Cx2d, row_height: f64, spectrum: bool) {
        let p = if spectrum { self.spectrum_plot } else { self.plot };
        let part = if spectrum { Part::Frequency } else { Part::Time };
        let look = InstrumentLook::for_theme(self.dark);
        let row = Rect { pos: p.pos + dvec2(0.0, p.size.y), size: dvec2(p.size.x, row_height) };
        if spectrum && self.settings.fft.waterfall {
            // Keep the frequency keyboard stop under the pointer-only divider. Spectrum is
            // added last below, so its window-name target wins the reverse hit lookup.
            if self.focused == Some(part) {
                self.quad(cx, Rect { pos: row.pos + dvec2(0.0, row.size.y - 1.0), size: dvec2(row.size.x, 1.0) }, Tokens::for_theme(self.dark).selection);
            }
            self.parts.push(PartRect { part, rect: row, selected: false });
            self.axis_part(cx, Part::Divider, row);
        } else { self.axis_part(cx, part, row); }
        let window = self.settings.window();
        let fft = if spectrum { self.settings.spectrum() } else { None };
        let pixels = (if spectrum { self.spectrum_view.plot_px() } else { self.plot_px() }).map_or(1, |p| p.0);
        let key = (window, p.size.x, fft, pixels);
        if self.time_key != Some(key) {
            self.time_ticks = if let Some(fft) = fft {
                let record = self.bracket.map_or(window.pre + window.post, |(t0, t1)| t1 - t0);
                let df = 1.0 / if record.is_finite() && record > 0.0 { record } else { window.pre + window.post };
                let n = super::spectrum::fft_size(pixels);
                let span = n as f64 * df * 0.5;
                self.time_note = format!("{} across", si_label(span, "Hz"));
                let (scale, unit) = axis_prefix(0.0, span, "Hz");
                let format = |value, step| if fft.log_freq { si_label(value, "Hz") } else { format!("{} {unit}", axis_label(value, step, scale)) };
                let mut measure = |label: &str| self.text_width(cx, label, true);
                let mut ticks = if fft.log_freq { log_ticks(df, span, p.size.x, GAP * 2.0, &format, &mut measure) }
                    else { linear_ticks(0.0, span, p.size.x, GAP * 2.0, &format, &mut measure) };
                for tick in &mut ticks { tick.at = super::spectrum::bin_x(tick.value / df, n, pixels, fft.log_freq) / pixels.max(1) as f64 * p.size.x; }
                if fft.log_freq { ticks.sort_by_key(|tick| if tick.major { 0 } else if tick.value / 10.0_f64.powf(tick.value.log10().floor()) > 4.0 { 1 } else { 2 }); }
                ticks
            } else {
                self.time_note = format!("{} across", si_label(window.pre + window.post, "s"));
                let (scale, unit) = axis_prefix(-window.pre, window.post, "s");
                linear_ticks(-window.pre, window.post, p.size.x, GAP * 2.0, &|value, step| time_label(value, step, scale, &unit), &mut |label| self.text_width(cx, label, true))
            };
            self.time_key = Some(key);
        }
        let ticks = std::mem::take(&mut self.time_ticks);
        let note = std::mem::take(&mut self.time_note);
        let mut right = p.pos.x + p.size.x;
        if spectrum {
            let label = self.settings.fft.window_label();
            let width = self.text_width(cx, label, true).min(p.size.x);
            let rect = Rect { pos: dvec2(right - width, row.pos.y), size: dvec2(width, row_height) };
            self.axis_part(cx, Part::Spectrum, rect);
            self.text_box(cx, Rect { pos: rect.pos + dvec2(0.0, GAP), size: dvec2(width, row_height - GAP) }, label, look.text_tertiary, true);
            right = (rect.pos.x - GAP * 2.0).max(p.pos.x);
        }
        let width = self.text_width(cx, &note, true).min(right - p.pos.x);
        let note_x = right - width;
        self.occupied.clear();
        for tick in &ticks {
            if p.size.y > 0.0 { self.frequency_line(cx, tick, p); }
            if tick.label.is_empty() { continue; }
            let width = self.text_width(cx, &tick.label, true);
            let x = (p.pos.x + tick.at - width * 0.5).clamp(p.pos.x, (p.pos.x + p.size.x - width).max(p.pos.x));
            if x + width + GAP > note_x || self.occupied.iter().any(|&(left, right)| x < right + GAP && x + width + GAP > left) { continue; }
            self.text(cx, dvec2(x, row.pos.y + GAP), &tick.label, look.text_tertiary, true);
            self.occupied.push((x, x + width));
        }
        self.text_box(cx, Rect { pos: dvec2(note_x, row.pos.y + GAP), size: dvec2(width, row_height - GAP) }, &note, look.text_secondary, true);
        self.time_ticks = ticks;
        self.time_note = note;
    }

    fn draw_ground_markers(&mut self, cx: &mut Cx2d) {
        let p = self.plot;
        let px = 1.0 / cx.current_dpi_factor();
        let active = self.trace_channel().unwrap_or(self.settings.trigger.channel);
        // Separate short tags stay selectable even when several channels share the same ground.
        for i in 0..self.settings.channels.len() {
            let map = self.settings.map_channel(i, (1, 1)).unwrap();
            let fraction = map.v_top / map.v_px;
            let y = p.pos.y + fraction.clamp(0.0, 1.0) * p.size.y;
            let x = p.pos.x - (i + 1) as f64 * 12.0;
            let rect = Rect { pos: dvec2(x, (y - MARKER * 0.5).clamp(p.pos.y, (p.pos.y + p.size.y - MARKER).max(p.pos.y))), size: dvec2(12.0, MARKER.min(p.size.y)) };
            if i != active {
                // Retain each channel's scale slider and actions on its tag for keyboard/a11y.
                // The offset tag owns pointer hits; Shift-wheel there adjusts that channel's scale.
                self.parts.push(PartRect { part: Part::Scale(i), rect, selected: false });
            }
            self.axis_part(cx, Part::Offset(i), rect);
            let color = self.channel_color(i);
            if !(0.0..=1.0).contains(&fraction) {
                let up = fraction < 0.0;
                // A four-point chevron, rasterised with device-pixel horizontal strokes.
                let mut dy = 0.0;
                while dy < 4.0 {
                    let yy = if up { p.pos.y + dy } else { p.pos.y + p.size.y - px - dy };
                    for xx in [x + 6.0 - dy, x + 6.0 + dy] {
                        self.quad(cx, Rect { pos: dvec2(xx, yy), size: dvec2(px, px) }, color);
                    }
                    dy += px;
                }
            } else {
                self.quad(cx, Rect { pos: dvec2(x + 1.0, (y - px * 0.5).clamp(p.pos.y, (p.pos.y + p.size.y - px).max(p.pos.y))), size: dvec2(10.0, px) }, color);
            }
        }
    }

    fn draw_trigger_ghost(&mut self, cx: &mut Cx2d, p: Rect) {
        if let Some(level) = self.trigger_ghost {
            let map = self.settings.map((1, 1));
            let color = self.channel_color(0);
            let y = p.pos.y + (((map.v_top - level) / map.v_px).clamp(0.0, 1.0) * p.size.y).min((p.size.y - 1.0).max(0.0));
            self.quad(cx, Rect { pos: dvec2(p.pos.x, y.round()), size: dvec2(p.size.x, 1.0) }, [color[0], color[1], color[2], 0.3]);
        }
    }

    fn draw_markers(&mut self, cx: &mut Cx2d) {
        let p = self.plot;
        let color = self.channel_color(0);
        let x = self.position_x();
        let point = Rect { pos: dvec2(x - MARKER * 0.5, p.pos.y - MARKER), size: dvec2(MARKER, MARKER) };
        self.text(cx, point.pos + dvec2(3.0, 0.0), "▼", color, true);
        self.parts.push(PartRect { part: Part::PositionMarker, rect: point, selected: false });
        if self.drag.is_some_and(|d| d.part == Part::PositionMarker) {
            self.quad(cx, Rect { pos: dvec2(x.round(), p.pos.y), size: dvec2(1.0, p.size.y) }, [color[0], color[1], color[2], 0.55]);
        }
        self.draw_trigger_ghost(cx, p);
        if !self.trigger_lane { return; }
        let y = self.trigger_y();
        let waiting = self.trigger_state == super::acquire::TriggerState::Waiting;
        let free = self.trigger_state == super::acquire::TriggerState::Automatic;
        let line = [color[0], color[1], color[2], if free { 0.55 / 3.0 } else { 0.55 }];
        if waiting {
            let mut x = p.pos.x;
            while x < p.pos.x + p.size.x {
                self.quad(cx, Rect { pos: dvec2(x, y.round()), size: dvec2(4.0f64.min(p.pos.x + p.size.x - x), 1.0) }, line);
                x += 8.0;
            }
        } else { self.quad(cx, Rect { pos: dvec2(p.pos.x, y.round()), size: dvec2(p.size.x, 1.0) }, line); }
        let level = Rect { pos: dvec2(p.pos.x, y - 2.0), size: dvec2(p.size.x, 4.0) };
        self.parts.push(PartRect { part: Part::LevelMarker, rect: level, selected: false });
        let qualifier = self.settings.trigger_qualifier_label();
        let zone_width = if self.settings.trigger.zone.is_some() { self.text_width(cx, "zone", true) + 10.0 } else { 0.0 };
        let qualifier_width = if qualifier.is_empty() { 0.0 } else { self.text_width(cx, &qualifier, true) + 10.0 };
        let tag = Rect { pos: dvec2((p.pos.x + p.size.x - qualifier_width - zone_width - if self.spectrum().is_some() { 42.0 } else { 0.0 }).max(p.pos.x), (y - 10.0).clamp(p.pos.y, p.pos.y + (p.size.y - 20.0).max(0.0))), size: dvec2(22.0, 20.0) };
        self.trigger_tag = tag;
        if !self.trigger_note.is_empty() {
            let note = std::mem::take(&mut self.trigger_note);
            let width = self.text_width(cx, &note, true);
            self.text(cx, dvec2((tag.pos.x - width - 6.0).max(p.pos.x), tag.pos.y + 3.0), &note, color, true);
            self.trigger_note = note;
        }
        self.text(cx, tag.pos + dvec2(1.0, 3.0), "‹T", color, true);
        self.parts.push(PartRect { part: Part::LevelMarker, rect: tag, selected: false });
        let slope = match self.settings.trigger.slope { Slope::Rising => "↑", Slope::Falling => "↓", Slope::Either => "↕" };
        self.glyph(cx, Part::Slope, Rect { pos: tag.pos + dvec2(22.0, 0.0), size: dvec2(20.0, 20.0) }, slope, true);
        if !qualifier.is_empty() {
            self.glyph(cx, Part::Qualifier, Rect { pos: tag.pos + dvec2(42.0, 0.0), size: dvec2(qualifier_width, 20.0) }, &qualifier, true);
        }
        if zone_width > 0.0 {
            self.text(cx, tag.pos + dvec2(47.0 + qualifier_width, 3.0), "zone", color, true);
        }
        if let Some(other) = self.runt_level() {
            let y = self.level_y(other);
            let mut x = p.pos.x;
            while x < p.pos.x + p.size.x {
                self.quad(cx, Rect { pos: dvec2(x, y.round()), size: dvec2(4.0f64.min(p.pos.x + p.size.x - x), 1.0) }, line);
                x += 8.0;
            }
            self.parts.push(PartRect { part: Part::RuntLevel, rect: Rect { pos: dvec2(p.pos.x, y - 2.0), size: dvec2(p.size.x, 4.0) }, selected: false });
            let marker = Rect { pos: dvec2((tag.pos.x - 24.0).max(p.pos.x), (y - 10.0).clamp(p.pos.y, p.pos.y + (p.size.y - 20.0).max(0.0))), size: dvec2(22.0, 20.0) };
            self.text(cx, marker.pos + dvec2(1.0, 3.0), "‹R", color, true);
            self.parts.push(PartRect { part: Part::RuntLevel, rect: marker, selected: false });
        }
    }

    fn glyph(&mut self, cx: &mut Cx2d, part: Part, rect: Rect, glyph: &str, selected: bool) {
        let look = InstrumentLook::for_theme(self.dark);
        let hovered = self.pointer.is_some_and(|p| rect.contains(p));
        let ink = if hovered || self.focused == Some(part) { look.text } else if selected { self.channel_color(0) } else { look.text_tertiary };
        self.text_box(cx, Rect { pos: rect.pos + dvec2(5.0, 3.0), size: rect.size - dvec2(5.0, 3.0) }, glyph, ink, true);
        self.parts.push(PartRect { part, rect, selected });
    }

    fn draw_answer(&mut self, cx: &mut Cx2d, pos: DVec2, answer: &Answer, index: usize) -> f64 {
        let look = InstrumentLook::for_theme(self.dark);
        let alpha = self.answer_alpha(index);
        let fade = |mut ink: [f32; 4]| { ink[3] *= alpha; ink };
        let secondary = fade(look.text_secondary);
        // Explicit and expected harmonics keep their identity while the automatic one follows hover.
        if answer.unit == "dBc" && answer.marks.is_empty() && self.settings.answers.is_none() && !self.answer_required(answer.name.trim()) {
            let width = self.answer_width(cx, answer);
            let cached = std::mem::take(&mut self.harmonic_answers);
            let answer = self.spectrum_hover.and_then(|i| i.checked_sub(1)).and_then(|i| cached[i].as_ref()).unwrap_or(answer);
            self.text(cx, pos, &answer.name, secondary, true);
            let x = pos.x + self.text_width(cx, &answer.name, true);
            self.text(cx, dvec2(x, pos.y), &answer.value, fade(self.channel_color(0)), true);
            let x = pos.x + width - self.text_width(cx, &answer.unit, true);
            self.text(cx, dvec2(x, pos.y), &answer.unit, secondary, true);
            self.harmonic_answers = cached;
            return width;
        }
        let mut x = pos.x;
        let ink = fade(if answer.failing() { look.canvas.violation } else { self.channel_color(0) });
        let dim_ink = fade(if answer.failing() { look.canvas.violation } else { look.text_tertiary });
        self.text(cx, dvec2(x, pos.y), &answer.name, secondary, true);
        x += self.text_width(cx, &answer.name, true);
        let start = x;
        let mut from = 0;
        for range in &answer.dim {
            for (span, ink) in [(from..range.start, ink), (range.clone(), dim_ink)] {
                let offset = self.text_width(cx, &answer.value[..span.start], true);
                self.text(cx, dvec2(start + offset, pos.y), &answer.value[span], ink, true);
            }
            from = range.end;
        }
        let offset = self.text_width(cx, &answer.value[..from], true);
        self.text(cx, dvec2(start + offset, pos.y), &answer.value[from..], ink, true);
        x += self.text_width(cx, &answer.value, true);
        self.text(cx, dvec2(x, pos.y), "\u{2009}", secondary, true);
        x += self.text_width(cx, "\u{2009}", true);
        let at = answer.unit.find(" (was ").unwrap_or(answer.unit.len());
        self.text(cx, dvec2(x, pos.y), &answer.unit[..at], secondary, true);
        let offset = self.text_width(cx, &answer.unit[..at], true);
        self.text(cx, dvec2(x + offset, pos.y), &answer.unit[at..], fade(look.text_tertiary), true);
        x += self.text_width(cx, &answer.unit, true);
        for (pass, mark) in &answer.marks {
            x += self.text_width(cx, " ", true);
            self.text(cx, dvec2(x, pos.y), mark, fade(if *pass { look.canvas.ok } else { look.canvas.violation }), true);
            x += self.text_width(cx, mark, true);
        }
        // The harmonic slot still reserves every preview's width, independent of hover.
        if answer.unit == "dBc" { (x - pos.x).max(self.harmonic_width) } else { x - pos.x }
    }

    fn spectrum_ticks(&self) -> [Option<f64>; 5] {
        let mut ticks = [None; 5];
        if self.collapsed || self.settings.domain != Domain::Fft || self.measurement_note.is_some() { return ticks; }
        let Some(spectrum) = self.spectrum_measurements else { return ticks };
        let df = spectrum.rate / spectrum.n as f64;
        for (i, tick) in ticks.iter_mut().enumerate() {
            let frequency = if i == 0 { Some(spectrum.peak_f) } else { spectrum.harmonics[i - 1].map(|h| h.0) };
            if let Some(f) = frequency {
                // Width one gives the same normalized bin/log coordinate as the worker raster.
                *tick = Some(self.spectrum_plot.pos.x + bin_x(f / df, spectrum.n, 1, self.settings.fft.log_freq) * self.spectrum_plot.size.x);
            }
        }
        ticks
    }

    fn hover_spectrum(&self, ticks: &[Option<f64>; 5]) -> Option<usize> {
        let pointer = self.pointer?;
        let dy = pointer.y - pointer.y.clamp(self.spectrum_plot.pos.y, self.spectrum_plot.pos.y + 5.0);
        let mut nearest = None;
        let mut distance = 16.0;
        for (i, x) in ticks.iter().enumerate() {
            if let Some(x) = x {
                let d = (pointer.x - x).powi(2) + dy * dy;
                if d <= distance { nearest = Some(i); distance = d; }
            }
        }
        nearest
    }

    fn draw_spectrum_ticks(&mut self, cx: &mut Cx2d, ticks: &[Option<f64>; 5]) {
        let look = InstrumentLook::for_theme(self.dark);
        let hue = self.channel_color(0);
        self.axis_mode = true;
        for (i, x) in ticks.iter().enumerate() {
            let Some(x) = x else { continue };
            let active = self.spectrum_hover == Some(i);
            let ink = [hue[0], hue[1], hue[2], if active { 1.0 } else { 0.35 }];
            self.quad(cx, Rect { pos: dvec2(*x - 0.5, self.spectrum_plot.pos.y), size: dvec2(1.0, 5.0) }, ink);
            if i > 0 {
                let label = ["2", "3", "4", "5"][i - 1];
                let width = self.text_width(cx, label, true);
                self.text(cx, dvec2(*x - width * 0.5, self.spectrum_plot.pos.y - 14.0), label, if active { look.text } else { look.text_tertiary }, true);
            }
        }
        self.axis_mode = false;
    }

    /// The same cached text and whole-answer packing serve the head and the extra rows.
    fn draw_answer_rows(&mut self, cx: &mut Cx2d, pos: DVec2) {
        let answers = std::mem::take(&mut self.answers);
        for row in 0..self.answers_layout.rows.len() {
            let range = self.answers_layout.rows[row].clone();
            let mut x = pos.x;
            for i in range {
                let i = self.answer_indices[i];
                let at = dvec2(x, pos.y + row as f64 * ROW + 4.0);
                let width = self.draw_answer(cx, at, &answers.items[i], i);
                x += width + self.draw_answer_distribution(cx, i, at + dvec2(width, 0.0)) + ANSWER_GAP;
            }
        }
        self.answers = answers;
    }

    fn draw_tag_row(&mut self, cx: &mut Cx2d, view: Rect) {
        let look = InstrumentLook::for_theme(self.dark);
        let color = self.channel_color(0);
        let right = view.pos.x + view.size.x;
        let handles = if self.closable { 4.0 } else { 3.0 } * 22.0;
        let tag_width = self.answers_tag_width;
        let tag = Rect { pos: view.pos, size: dvec2(tag_width, 22.0) };
        let kind = TagKind::from_unit(self.settings.channels.first().map_or("V", |channel| channel.unit.as_str()));
        let hue = vec4(color[0], color[1], color[2], color[3]);
        let name = self.settings.channels.first_mut().map(|c| std::mem::take(&mut c.name)).unwrap_or_default();
        let net = std::mem::take(&mut self.net);
        let identity = ProbeTag::from_label(&name, &net);
        let state = |rect: Rect| if self.tag_ghost { TagState::Ghost }
            else if self.pointer.is_some_and(|p| rect.contains(p)) { TagState::Hover } else { TagState::Rest };
        if self.sources.is_empty() {
            let style = TagStyle { hue, kind, side: LaneSide::Sim, state: state(tag) };
            identity.draw(cx, &mut self.draw_tag, &mut self.draw_label, tag, style);
            self.parts.push(PartRect { part: Part::Tag, rect: tag, selected: false });
        } else {
            let labels = std::mem::take(&mut self.source_labels);
            let count = self.sources.len();
            let chip_width = ((tag_width - (count - 1) as f64 * 4.0) / count as f64).max(0.0);
            for slot in 0..count {
                let index = if slot == 0 { self.primary_source } else { 1 - self.primary_source };
                let side = self.sources[index].side;
                let chip = Rect { pos: tag.pos + dvec2(slot as f64 * (chip_width + 4.0), 0.0), size: dvec2(chip_width, 22.0) };
                let color = self.sources[index].channel.map(|channel| look.channel(channel));
                let hue = color.map_or(hue, |c| vec4(c[0], c[1], c[2], c[3]));
                let style = TagStyle { hue, kind, side: side.into(), state: if self.overlay_preview && index == 1 { TagState::Ghost } else { state(chip) } };
                let tag = if count == 1 { ProbeTag { title: identity.title, pin: identity.pin } }
                    else if self.overlay_pair() { ProbeTag::from_label(&labels[index], "") }
                    else { ProbeTag { title: &labels[index], pin: identity.pin } };
                tag.draw(cx, &mut self.draw_tag, &mut self.draw_label, chip, style);
                let part = if slot == 0 { Part::Tag } else { Part::Secondary };
                self.parts.push(PartRect { part, rect: chip, selected: slot == 0 });
            }
            self.source_labels = labels;
        }
        if let Some(channel) = self.settings.channels.first_mut() { channel.name = name; }
        self.net = net;
        let mut x = tag.pos.x + tag_width + 8.0;
        let limit = right - handles - 22.0 - if self.collapsed && self.answers_spark { 104.0 } else { 0.0 };
        let answers = std::mem::take(&mut self.answers);
        if answers.items.is_empty() && self.unjudged_answers.is_empty() {
            self.text_box(cx, Rect { pos: dvec2(x, view.pos.y + 4.0), size: dvec2((limit - x).max(0.0), 18.0) }, &answers.text, look.text_tertiary, true);
            if !answers.text.is_empty() { x += self.text_width(cx, &answers.text, true) + ANSWER_GAP; }
        } else {
            for slot in 0..self.answers_layout.head {
                let index = self.answer_indices[slot];
                let at = dvec2(x, view.pos.y + 4.0);
                let width = self.draw_answer(cx, at, &answers.items[index], index);
                let width = width + self.draw_answer_distribution(cx, index, at + dvec2(width, 0.0));
                self.parts.push(PartRect { part: Part::Answer(index), rect: Rect { pos: dvec2(x, view.pos.y), size: dvec2(width, 22.0) }, selected: false });
                x += width + ANSWER_GAP;
            }
            if self.answers_layout.more > 0 {
                let more_labels = std::mem::take(&mut self.more_labels);
                let more = more_labels.get(self.answers_layout.more).map_or("+", String::as_str);
                let width = self.text_width(cx, more, true);
                if self.answers_layout.chip {
                    let rect = Rect { pos: dvec2(x, view.pos.y), size: dvec2(width + MORE_PAD * 2.0, 22.0) };
                    let tk = Tokens::for_theme(self.dark);
                    self.draw_more.color = vec4(tk.block_fill[0], tk.block_fill[1], tk.block_fill[2], tk.block_fill[3]);
                    self.draw_more.draw_abs(cx, rect);
                    if self.focused == Some(Part::More) {
                        self.quad(cx, Rect { pos: rect.pos + dvec2(4.0, 20.0), size: dvec2((rect.size.x - 8.0).max(0.0), 1.0) }, tk.selection);
                    }
                    self.parts.push(PartRect { part: Part::More, rect, selected: false });
                    self.text(cx, rect.pos + dvec2(MORE_PAD, 4.0), more, look.text_tertiary, true);
                } else {
                    self.text_box(cx, Rect { pos: dvec2(x, view.pos.y + 4.0), size: dvec2(width.min((limit - x).max(0.0)), 18.0) }, more, look.text_tertiary, true);
                }
                x += width + if self.answers_layout.chip { MORE_PAD * 2.0 } else { 0.0 } + ANSWER_GAP;
                self.more_labels = more_labels;
            }
        }
        self.answers = answers;
        let add = Rect { pos: dvec2(x.min(limit).max(view.pos.x), view.pos.y), size: dvec2(22.0, 22.0) };
        self.draw_part_feedback(cx, Part::AddAnswer, add);
        self.text(cx, add.pos + dvec2(6.0, 4.0), "+", look.text_tertiary, true);
        self.parts.push(PartRect { part: Part::AddAnswer, rect: add, selected: false });
        let mut x = right - handles;
        for (part, glyph, selected) in [
            (Part::TriggerChannel, if self.trigger_lane { "●" } else { "○" }, self.trigger_lane),
            (Part::Collapse, if self.collapsed { "▸" } else { "▾" }, self.collapsed),
            (Part::Domain, "ƒ", self.settings.domain == Domain::Fft),
        ] {
            self.glyph(cx, part, Rect { pos: dvec2(x, view.pos.y), size: dvec2(22.0, 22.0) }, glyph, selected);
            x += 22.0;
        }
        if self.closable { self.glyph(cx, Part::Close, Rect { pos: dvec2(x, view.pos.y), size: dvec2(22.0, 22.0) }, "×", false); }
    }

    fn draw_drag_feedback(&mut self, cx: &mut Cx2d) {
        let p = if self.drag.is_some_and(|drag| matches!(drag.part, Part::SpectrumScale | Part::SpectrumOffset | Part::Frequency)) { self.spectrum_plot } else { self.plot };
        if let (Some(drag), Some(pointer)) = (self.drag, self.pointer) {
            let label = match drag.part {
                Part::LevelMarker => {
                    let unit = self.settings.channels.get(self.settings.trigger.channel).map_or("V", |c| c.unit.as_str());
                    format!("T {}", eng(self.settings.trigger.level, unit, 4))
                }
                Part::RuntLevel => format!("runt {}", eng(self.runt_level().unwrap_or(self.settings.trigger.level), "V", 4)),
                Part::PositionMarker => format!("pre {}", eng(self.settings.position * 100.0, "%", 3)),
                Part::SpectrumScale | Part::SpectrumOffset => format!("ref {:.1} dBV", self.settings.fft.ref_dbv),
                Part::Scale(i) => self.scale_label(i),
                Part::Frequency => self.frequency_note.clone(),
                Part::Time => format!("{} across", eng(10.0 * self.settings.time_per_div, "s", 4)),
                Part::Offset(i) => self.ground_label(i),
                _ => return,
            };
            let color = match drag.part {
                Part::Offset(i) | Part::Scale(i) => self.channel_color(i),
                Part::Time => InstrumentLook::for_theme(self.dark).text,
                _ => self.channel_color(self.settings.trigger.channel),
            };
            let size = dvec2(self.text_width(cx, &label, true) + 12.0, ROW);
            // Beside the pointer, on its left where the right side would leave the card.
            let x = if pointer.x + 12.0 + size.x > p.pos.x + p.size.x + MARKER {
                pointer.x - 12.0 - size.x
            } else {
                pointer.x + 12.0
            };
            let rect = Rect {
                pos: dvec2(x, if self.shared_time && drag.part == Part::Time { pointer.y - ROW - 12.0 } else { pointer.y + 12.0 }),
                size,
            };
            self.quad(cx, rect, Tokens::for_theme(self.dark).block_fill);
            self.text(cx, rect.pos + dvec2(6.0, 5.0), &label, color, true);
        }
    }

}

impl WidgetNode for SignalLane {
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
        self.phosphor.redraw(cx);
        if self.spectrum().is_some() { self.spectrum_view.redraw(cx); }
        if let Some(widget) = &self.answers_popover_widget { widget.redraw(cx); }
        if let Some(widget) = &self.measure_popover_widget { widget.redraw(cx); }
        if let Some(widget) = &self.spectrum_popover_widget { widget.redraw(cx); }
        if let Some(widget) = &self.value_popover_widget { widget.redraw(cx); }
    }
    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) {
        if let Some(widget) = &self.answers_popover_widget { visit(id!(answers_popover), widget.clone()); }
        if let Some(widget) = &self.measure_popover_widget { visit(id!(measure_popover), widget.clone()); }
        if let Some(widget) = &self.spectrum_popover_widget { visit(id!(spectrum_popover), widget.clone()); }
        if let Some(widget) = &self.value_popover_widget { visit(id!(value_popover), widget.clone()); }
    }
    fn find_widgets_from_point(&self, cx: &Cx, point: DVec2, found: &mut dyn FnMut(&WidgetRef)) {
        if let Some(widget) = &self.answers_popover_widget { widget.find_widgets_from_point(cx, point, found); }
        if let Some(widget) = &self.measure_popover_widget { widget.find_widgets_from_point(cx, point, found); }
        if let Some(widget) = &self.spectrum_popover_widget { widget.find_widgets_from_point(cx, point, found); }
        if let Some(widget) = &self.value_popover_widget { widget.find_widgets_from_point(cx, point, found); }
    }
}

impl Widget for SignalLane {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, mut walk: Walk) -> DrawStep {
        let theme = base_theme(cx);
        self.dark = theme != BaseTheme::Light;
        self.label_cache.begin_frame(&self.draw_label, theme);
        if self.axis_cache.begin_frame(&self.draw_axis, theme) {
            self.axes_key = None;
            self.spectrum_axes_key = None;
            self.frequency_key = None;
            self.time_key = None;
            self.shared_key = None;
        }
        self.update_look(cx);
        self.sync_sources(cx);
        let look = InstrumentLook::for_theme(self.dark);
        let bg = look.canvas.canvas;
        self.phosphor.screen = vec4(bg[0], bg[1], bg[2], bg[3]);
        self.phosphor.set_channel(self.channel_color(0));
        self.spectrum_view.screen = vec4(bg[0], bg[1], bg[2], bg[3]);
        self.spectrum_view.set_channel(self.channel_color(0));
        let twin = self.overlay_pair().then(|| look.channel(self.sources[1 - self.primary_source].channel.unwrap()));
        self.phosphor.set_twin_channel(twin);
        self.spectrum_view.set_twin_channel(twin);
        walk = self.layout_walk(cx, walk);
        let wrapped_height = self.answers_layout.rows.len() as f64 * ROW;
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.drawn_at = view.pos;
        self.parts.clear();
        self.trigger_tag = Rect::default();
        cx.begin_turtle(Walk::abs_rect(view), Layout::flow_overlay());
        cx.push_clip_rect(view);
        self.quad(cx, view, bg);
        let fft = self.spectrum().is_some();
        let row_height = if self.shared_time { 0.0 } else { 22.0 };
        // The collapsed time view retains its expanded raster for the meter's live capture.
        let height = if self.collapsed { 160.0 } else { view.size.y - wrapped_height };
        let (time, divider, cell, spectrum, waterfall_height) = self.plot_layout(Rect { size: dvec2(view.size.x, height), ..view });
        self.plot = time;
        self.time_plot_drawn = true;
        self.spectrum_cell = cell;
        self.spectrum_plot = spectrum;
        let waterfall = fft && self.settings.fft.waterfall;
        let plot_height = spectrum.size.y;
        let axis_width = spectrum.pos.x - cell.pos.x;
        let cell_drag = self.drag.is_some_and(|drag| drag.part == Part::CellDivider);
        self.phosphor.set_raster_size(if cell_drag { self.cell_rasters.map(|sizes| sizes.0) } else { None });
        self.spectrum_view.set_raster_size(if cell_drag { self.cell_rasters.map(|sizes| sizes.1) }
            else { self.drag.filter(|drag| drag.part == Part::Divider).and(self.divider_raster) });
        let spectrum_ticks = self.spectrum_ticks();
        self.spectrum_hover = self.hover_spectrum(&spectrum_ticks);
        self.draw_tag_row(cx, view);
        cx.begin_turtle(Walk::abs_rect(self.plot), Layout::flow_overlay());
        self.update_stretch();
        let _ = self.phosphor.draw_walk(cx, scope, Walk::abs_rect(self.plot));
        cx.end_turtle();
        if fft {
            cx.begin_turtle(Walk::abs_rect(self.spectrum_plot), Layout::flow_overlay());
            let _ = self.spectrum_view.draw_walk(cx, scope, Walk::abs_rect(self.spectrum_plot));
            cx.end_turtle();
            if waterfall {
                self.spectrum_view.draw_waterfall(cx, Rect { pos: self.spectrum_plot.pos + dvec2(0.0, plot_height + 22.0), size: dvec2(self.spectrum_plot.size.x, waterfall_height) });
            }
        }
        if self.collapsed {
            self.draw_trigger_ghost(cx, Rect { pos: view.pos + dvec2(64.0, 3.0), size: dvec2((view.size.x - 108.0).max(1.0), 22.0) });
            if self.answers_spark {
                let handles = if self.closable { 88.0 } else { 66.0 };
                let spark = Rect { pos: dvec2(view.pos.x + (view.size.x - handles - 100.0).max(0.0), view.pos.y + 3.0), size: dvec2(96.0, 20.0) };
                let _ = self.spark.draw_walk(cx, scope, Walk::abs_rect(spark));
            }
        } else {
            self.axis_mode = true;
            let line_height = 12.0;
            let key = (self.settings.map((1, 1)), Domain::Time, FftSettings::default(), self.plot.size.y, line_height);
            if self.axes_key != Some(key) { self.axes = Some(self.value_axis(self.plot.size.y, line_height, false)); self.axes_key = Some(key); }
            if let Some(axis) = self.axes.take() {
                self.draw_value_axis(cx, &axis, Rect { pos: dvec2(view.pos.x, self.plot.pos.y), size: dvec2(64.0, self.plot.size.y) }, line_height, false);
                self.axes = Some(axis);
            }
            if row_height > 0.0 { self.draw_time_axis(cx, row_height, false); }
            if fft {
                let key = (self.settings.fft, plot_height, line_height);
                if self.spectrum_axes_key != Some(key) { self.spectrum_axes = Some(self.value_axis(plot_height, line_height, true)); self.spectrum_axes_key = Some(key); }
                if let Some(axis) = self.spectrum_axes.take() {
                    self.draw_value_axis(cx, &axis, Rect { pos: self.spectrum_cell.pos, size: dvec2(axis_width, plot_height) }, line_height, true);
                    self.spectrum_axes = Some(axis);
                }
                std::mem::swap(&mut self.time_ticks, &mut self.frequency_ticks);
                std::mem::swap(&mut self.time_key, &mut self.frequency_key);
                std::mem::swap(&mut self.time_note, &mut self.frequency_note);
                self.draw_time_axis(cx, 22.0, true);
                std::mem::swap(&mut self.time_ticks, &mut self.frequency_ticks);
                std::mem::swap(&mut self.time_key, &mut self.frequency_key);
                std::mem::swap(&mut self.time_note, &mut self.frequency_note);
            }
            self.axis_mode = false;
            self.draw_eye_marks(cx);
            self.draw_ground_markers(cx);
            self.draw_capture_overlay(cx);
            self.draw_markers(cx);
            self.draw_zone(cx);
            self.draw_spectrum_ticks(cx, &spectrum_ticks);
            if let Some(divider) = divider {
                self.quad(cx, divider, look.hairline);
                self.axis_part(cx, Part::CellDivider, Rect { pos: divider.pos - dvec2(3.0, 0.0), size: divider.size + dvec2(6.0, 0.0) });
            }
            self.draw_held(cx);
            self.draw_below(cx);
            let ink = self.channel_color(0);
            for (clipped, y) in [(self.clipped.high, self.plot.pos.y), (self.clipped.low, self.plot.pos.y + self.plot.size.y - 3.0)] {
                if clipped { self.quad(cx, Rect { pos: dvec2(self.plot.pos.x + self.plot.size.x * 0.5 - 5.0, y), size: dvec2(10.0, 3.0) }, ink); }
            }
            if !self.shared_time || self.drag.is_none_or(|drag| drag.part != Part::Time) { self.draw_drag_feedback(cx); }
            self.draw_answer_rows(cx, view.pos + dvec2(PAD, height));
        }
        let handle = Rect { pos: dvec2(view.pos.x, view.pos.y + view.size.y - 4.0), size: dvec2(view.size.x, 4.0) };
        let active = self.drag.is_some_and(|drag| drag.part == Part::Resize) || self.pointer.is_some_and(|p| handle.contains(p));
        self.quad(cx, Rect { pos: handle.pos + dvec2(0.0, 3.0), size: dvec2(handle.size.x, 1.0) }, if active { look.text_secondary } else { look.hairline });
        self.parts.push(PartRect { part: Part::Resize, rect: handle, selected: false });
        cx.pop_clip_rect();
        cx.end_turtle();
        // Draw even when closed to clear the popover's previous overlay. The anchor takes no
        // stack space; open_at receives its final (aligned/scrolled) rect on the event side.
        let widget = self.answers_popover_widget(cx);
        let popover = widget.as_popover();
        let anchor = self.answer_part_rect(self.answers_anchor_part.unwrap_or(Part::More));
        if anchor.is_none() { popover.close(cx); self.answers_anchor = None; self.answers_anchor_part = None; }
        if popover.is_open() { self.update_answers_popover(cx, &popover); }
        widget.draw_walk_all(cx, scope, Walk::abs_rect(anchor.unwrap_or_default()));
        if fft || self.spectrum_popover_widget.is_some() {
            let widget = self.spectrum_popover_widget(cx);
            let popover = widget.as_popover();
            let anchor = self.parts.iter().find(|part| part.part == Part::Spectrum).map(|part| part.rect);
            if anchor.is_none() { popover.close(cx); self.spectrum_anchor = None; }
            if popover.is_open() { self.update_spectrum_popover(cx, &popover); }
            widget.draw_walk_all(cx, scope, Walk::abs_rect(anchor.unwrap_or_default()));
        }
        self.draw_measure_palette(cx, scope);
        self.draw_value_entry(cx, scope);
        self.draw_bracket_remove(cx);
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if matches!(event, Event::Drag(_) | Event::Drop(_) | Event::DragEnd) && self.pointer.take().is_some() {
            self.area.redraw(cx);
        }
        if self.answers_timer.is_event(event).is_some() {
            self.answers_timer = Timer::empty();
            if self.answers_dirty { self.redraw(cx); }
        }
        self.phosphor.handle_event(cx, event, scope);
        if self.spectrum().is_some() { self.spectrum_view.handle_event(cx, event, scope); }
        let placed = self.area.rect(cx);
        let shift = placed.pos - self.drawn_at;
        if placed.size.x > 0.0 && shift != dvec2(0.0, 0.0) {
            self.plot.pos += shift;
            self.spectrum_plot.pos += shift;
            self.spectrum_cell.pos += shift;
            self.trigger_tag.pos += shift;
            for part in &mut self.parts {
                if !(self.shared_time && matches!(part.part, Part::Time | Part::Holdoff)) { part.rect.pos += shift; }
            }
            self.drawn_at = placed.pos;
        }
        if self.handle_answer_drag(cx, event) { return; }
        if self.handle_value_entry(cx, event, scope) { return; }
        if self.handle_measure_palette(cx, event, scope) { return; }
        if let Some(widget) = self.spectrum_popover_widget.clone() {
            let popover = widget.as_popover();
            let was_open = popover.is_open();
            if was_open {
                let anchor = self.parts.iter().find(|part| part.part == Part::Spectrum).map(|part| part.rect);
                if anchor != self.spectrum_anchor {
                    if let Some(rect) = anchor { popover.open_at(cx, rect); }
                    else { popover.close(cx); }
                    self.spectrum_anchor = anchor;
                }
            }
            let actions = cx.capture_actions(|cx| widget.handle_event(cx, event, scope));
            if was_open {
                self.spectrum_actions(cx, &popover, &actions);
                if let Event::Actions(actions) = event { self.spectrum_actions(cx, &popover, actions); }
                if !popover.is_open() { self.spectrum_anchor = None; self.pointer = None; self.area.redraw(cx); }
                // Escape and the outside closing press belong solely to the popover.
                return;
            }
        }
        if let Some(widget) = self.answers_popover_widget.clone() {
            let popover = widget.as_popover();
            let was_open = popover.is_open();
            if was_open {
                let anchor = self.answers_anchor_part.and_then(|part| self.answer_part_rect(part));
                if anchor != self.answers_anchor {
                    if let Some(rect) = anchor { popover.open_at(cx, rect); }
                    else { popover.close(cx); }
                    self.answers_anchor = anchor;
                }
            }
            let actions = cx.capture_actions(|cx| widget.handle_event(cx, event, scope));
            if was_open {
                self.answer_pin_actions(cx, &popover, &actions);
                if let Event::Actions(actions) = event {
                    self.answer_pin_actions(cx, &popover, actions);
                    if popover.dismissed(actions) { self.pointer = None; self.area.redraw(cx); }
                }
            }
            // The popover consumes its closing press too; never reopen it through the lane's hit.
            if was_open {
                if !popover.is_open() { self.answers_anchor = None; self.answers_anchor_part = None; self.pointer = None; self.area.redraw(cx); }
                return;
            }
        }
        // A carried unsupported probe keeps its explanation on the existing drag label.
        if let Some(caps) = &self.source_caps {
            if let Event::Drag(drag) = event {
                if let Some(reason) = crate::live::probe::source_refusal(caps, &drag.items) {
                    if let DragHit::Drag(hit) = event.drag_hits(cx, self.area) {
                        if hit.state != DragState::Out {
                            if let Ok(mut response) = hit.response.try_lock() { *response = DragResponse::None; }
                            cx.global::<crate::live::probe::ProbeGesture>().refusal = Some(reason.into());
                            if let Some(tip) = cx.drag_visual() { tip.label(cx, ids!(label)).set_text(cx, reason); }
                        } else { cx.global::<crate::live::probe::ProbeGesture>().refusal = None; }
                        return;
                    }
                }
            }
            if let Event::Drop(drop) = event {
                if crate::live::probe::source_refusal(caps, &drop.items).is_some() {
                    if let DragHit::Drop(_) = event.drag_hits(cx, self.area) { return; }
                }
            }
            if matches!(event, Event::DragEnd) { cx.global::<crate::live::probe::ProbeGesture>().refusal = None; }
        }
        if self.handle_holdoff(cx, event) {
            if self.pointer.take().is_some() { self.area.redraw(cx); }
            return;
        }
        if let Event::KeyDown(e) = event {
            if e.modifiers.shift && self.view_drag.is_some() {
                if let Some(pointer) = self.pointer { self.drag_to(cx, pointer, true); }
            }
        }
        let hit = match event.hits(cx, self.time_area) { Hit::Nothing => event.hits(cx, self.area), hit => hit };
        match hit {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                self.pointer = Some(e.abs);
                let cursor = self.pointer_cursor(e.abs);
                cx.set_cursor(if self.bracket_drag.is_none() && self.value_bracket_drag.is_none() && cursor != MouseCursor::RowResize && e.modifiers.alt && self.plot.contains(e.abs) { MouseCursor::Crosshair } else { cursor });
                self.area.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                if self.zone_drag.is_none() && self.drag.is_none() && self.bracket_drag.is_none() && self.value_bracket_drag.is_none() && !self.delay_drag {
                    self.pointer = None;
                    cx.set_cursor(MouseCursor::Default);
                }
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                self.pointer = Some(e.abs);
                cx.set_key_focus_from_user(self.area);
                self.view_drag = None;
                self.clear_bracket_drag();
                self.clear_value_bracket_drag();
                self.delay_drag = false;
                let cell_divider = self.part_at(e.abs) == Some(Part::CellDivider);
                self.zone_drag = None;
                if self.part_at(e.abs) == Some(Part::Resize) {
                    self.drag = None;
                    self.tag_press = None;
                    self.trigger_press = None;
                    self.focused = Some(Part::Resize);
                    if e.tap_count == 2 { self.apply_resize(cx, resize(DEFAULT_LANE_HEIGHT, 0.0, false), true); }
                    else {
                        self.drag = Some(Drag { part: Part::Resize, grab: e.abs.y, start: self.height, pending: None, extent: 0.0 });
                        self.drag_to(cx, e.abs, false);
                    }
                    cx.set_cursor(MouseCursor::RowResize);
                    self.area.redraw(cx);
                    return;
                }
                // A topmost level line/tag hit takes Shift before time brackets and shared trigger transfer.
                let part = self.part_at(e.abs);
                if self.trigger_lane && !self.collapsed
                    && (part == Some(Part::LevelMarker) && e.modifiers.shift && !e.modifiers.alt || part == Some(Part::RuntLevel)) {
                    let create = part == Some(Part::LevelMarker);
                    let other = if create { self.settings.trigger.level } else { self.runt_level().unwrap_or(self.settings.trigger.level) };
                    self.runt_preview = create.then_some(other);
                    self.focused = Some(Part::RuntLevel);
                    self.tag_press = None;
                    self.trigger_press = None;
                    self.drag = Some(Drag { part: Part::RuntLevel, grab: if create { 0.0 } else { e.abs.y - self.level_y(other) }, start: 0.0, pending: None, extent: 0.0 });
                    cx.set_cursor(MouseCursor::RowResize);
                    self.area.redraw(cx);
                    return;
                }
                self.runt_preview = None;
                if !cell_divider && !self.collapsed && self.plot.contains(e.abs) {
                    if e.modifiers.alt { self.begin_zone(cx, e.abs, None); return; }
                    if self.part_at(e.abs) == Some(Part::Zone) { self.begin_zone(cx, e.abs, self.lane_zone()); return; }
                }
                // The value axis: alt + drag a span makes a value bracket (on the plot body it is a ZoneBox).
                if !cell_divider && !self.collapsed && e.modifiers.alt {
                    if let Some(channel) = self.trace_channel().filter(|&channel| self.part_at(e.abs) == Some(Part::Scale(channel))) {
                        self.drag = None;
                        self.tag_press = None;
                        self.trigger_press = None;
                        self.focused = Some(Part::Plot(channel));
                        self.value_bracket_press = self.value_bracket;
                        let value = self.bracket_value(e.abs, channel);
                        self.edit_value_bracket(cx, Some((value, value)));
                        self.value_bracket_drag = Some(value);
                        self.value_bracket_axis = true;
                        cx.set_cursor(MouseCursor::Hand);
                        self.area.redraw(cx);
                        return;
                    }
                }
                if self.delay_enabled && !cell_divider {
                    let start = if e.modifiers.is_primary() { self.delay_edge(cx, e.abs).map(|edge| (edge, None)) }
                        else { self.delay_end(e.abs).map(|(index, end, edge)| (edge, Some((index, end)))) };
                    if let Some((edge, edit)) = start {
                        self.drag = None;
                        self.tag_press = None;
                        self.trigger_press = None;
                        self.delay_drag = true;
                        self.focused = self.trace_channel().map(Part::Plot);
                        cx.widget_action(self.uid, ScopeAction::Delay(DelayGesture::Start { abs: e.abs, edge, edit }));
                        cx.set_cursor(MouseCursor::Hand);
                        self.area.redraw(cx);
                        return;
                    }
                }
                if !cell_divider && !self.collapsed && self.plot.contains(e.abs) {
                    let end = self.value_bracket_end(e.abs);
                    if let Some(channel) = self.trace_channel().filter(|_| !e.modifiers.shift && end.is_some()) {
                        self.drag = None;
                        self.focused = Some(Part::Plot(channel));
                        self.value_bracket_press = self.value_bracket;
                        if let Some((v0, v1)) = self.value_bracket {
                            self.value_bracket_drag = Some(if end == Some(End::Start) { v1 } else { v0 });
                        }
                        cx.set_cursor(MouseCursor::Hand);
                        self.area.redraw(cx);
                        return;
                    }
                }
                if !cell_divider && self.bracket_visible && !self.collapsed && self.plot.contains(e.abs) {
                    let end = self.bracket_end(e.abs);
                    if e.modifiers.shift || end.is_some() {
                        self.drag = None;
                        self.focused = self.trace_channel().map(Part::Plot);
                        self.bracket_press = self.bracket;
                        if e.modifiers.shift {
                            let t = self.bracket_time(e.abs);
                            self.edit_bracket(cx, Some((t, t)));
                            self.bracket_drag = Some(t);
                        } else if let Some((t0, t1)) = self.bracket {
                            self.bracket_drag = Some(if end == Some(End::Start) { t1 } else { t0 });
                        }
                        cx.set_cursor(MouseCursor::Hand);
                        self.area.redraw(cx);
                        return;
                    }
                }
                let part = self.part_at(e.abs).or_else(|| {
                    (self.plot.contains(e.abs) && !self.collapsed).then(|| self.trace_channel().map(Part::Plot)).flatten()
                });
                if let Some(part) = part {
                    if let Part::Answer(index) = part {
                        self.answer_drag = self.answer_names.get(index).copied().flatten().map(|name| AnswerDrag { index, name, press: e.abs, started: false, outside: false });
                        self.toggle_answers(cx, part);
                        return;
                    }
                    self.focused = Some(part);
                    if e.tap_count == 2 && matches!(part, Part::SpectrumScale | Part::SpectrumOffset) {
                        self.open_value_entry(cx, part);
                        return;
                    }
                    if part == Part::CellDivider && e.tap_count == 2 {
                        self.drag = None;
                        let actions = self.settings.set_cell_share(0.38);
                        self.emit(cx, actions);
                        cx.set_cursor(MouseCursor::EwResize);
                        return;
                    }
                    if part == Part::Divider && e.tap_count == 2 {
                        self.drag = None;
                        let actions = self.settings.set_waterfall_share(1.0 / 3.0);
                        self.emit(cx, actions);
                        cx.set_cursor(MouseCursor::NsResize);
                        return;
                    }
                    if matches!(part, Part::Tag | Part::Secondary) {
                        let source = if part == Part::Tag { self.primary_source } else { 1 - self.primary_source };
                        self.tag_press = Some((e.abs, source));
                        return;
                    }
                    if part == Part::LevelMarker {
                        if self.shared_time && self.trigger_tag.contains(e.abs) {
                            self.trigger_press = Some((e.abs, e.abs.y - self.trigger_y()));
                            return;
                        }
                        if e.tap_count == 2 {
                            if let Some(level) = centred_level(&self.measurements, self.settings.trigger.channel) {
                                let actions = self.settings.set_trigger_level(level);
                                self.emit(cx, actions);
                            }
                            self.drag = None;
                            return;
                        }
                    }
                    if matches!(part, Part::Plot(_) | Part::Scale(_) | Part::Time | Part::Frequency) {
                        if e.tap_count == 2 {
                            self.drag = None;
                            self.fit_view(cx, part);
                            return;
                        }
                        self.begin_view_drag(part, e.abs, e.modifiers.shift);
                    }
                    let grab = match part {
                        Part::LevelMarker => Some(e.abs.y - self.trigger_y()),
                        Part::PositionMarker => Some(e.abs.x - self.position_x()),
                        Part::Offset(_) | Part::Divider | Part::SpectrumScale | Part::SpectrumOffset => Some(e.abs.y),
                        Part::CellDivider => Some(e.abs.x),
                        Part::Plot(_) | Part::Scale(_) => Some(e.abs.y),
                        Part::Time | Part::Frequency => Some(e.abs.x),
                        _ => None,
                    };
                    if let Some(grab) = grab {
                        let start = match part {
                            Part::Divider => self.settings.fft.waterfall_share,
                            Part::CellDivider => self.settings.fft.cell_share,
                            Part::SpectrumOffset | Part::SpectrumScale => self.settings.fft.ref_dbv,
                            Part::Offset(i) => self.settings.channels[i].offset_div,
                            Part::Scale(i) => self.settings.channels[i].volts_per_div,
                            Part::Time | Part::Frequency => self.settings.time_per_div,
                            _ => 0.0,
                        };
                        let pending = matches!(part, Part::Divider | Part::CellDivider).then_some(e.abs);
                        if part == Part::Divider { self.divider_raster = Some(self.spectrum_plot.size); }
                        if part == Part::CellDivider { self.cell_rasters = Some((self.plot.size, self.spectrum_plot.size)); }
                        let extent = if part == Part::Divider { self.spectrum_plot.size.y / waterfall_split(1.0, start).0 }
                            else if part == Part::CellDivider { self.spectrum_cell.pos.x + self.spectrum_cell.size.x - self.plot.pos.x } else { 0.0 };
                        self.drag = Some(Drag { part, grab, start, pending, extent });
                        if self.view_drag.is_none() { self.drag_to(cx, e.abs, e.modifiers.shift); }
                        cx.set_cursor(self.pointer_cursor(e.abs));
                    } else {
                        self.adjust(cx, part, if e.modifiers.shift { -1 } else { 1 });
                    }
                    self.area.redraw(cx);
                }
            }
            Hit::FingerMove(e) => {
                if let Some(drag) = &mut self.zone_drag {
                    drag.pointer = e.abs;
                    self.pointer = Some(e.abs);
                    cx.set_cursor(self.pointer_cursor(e.abs));
                    self.area.redraw(cx);
                    return;
                }
                if self.delay_drag {
                    self.pointer = Some(e.abs);
                    cx.widget_action(self.uid, ScopeAction::Delay(DelayGesture::Move(e.abs)));
                    cx.set_cursor(MouseCursor::Hand);
                    return;
                }
                if self.value_bracket_drag.is_some() {
                    self.pointer = Some(e.abs);
                    self.drag_value_bracket(cx, e.abs);
                    cx.set_cursor(MouseCursor::Hand);
                    return;
                }
                if self.bracket_drag.is_some() {
                    self.pointer = Some(e.abs);
                    self.drag_bracket(cx, e.abs);
                    cx.set_cursor(MouseCursor::Hand);
                    return;
                }
                if let Some((start, grab)) = self.trigger_press {
                    if (e.abs - start).length() > 4.0 {
                        self.trigger_press = None;
                        cx.widget_action(self.uid, ScopeAction::DragTrigger { abs: e.abs, grab });
                    }
                    return;
                }
                if let Some((start, source)) = self.tag_press {
                    if (e.abs - start).length() > super::bench::DRAG_SLOP {
                        self.tag_press = None;
                        let channel = self.sources.get(source).and_then(|source| source.channel);
                        cx.widget_action(self.uid, ScopeAction::DragTag { abs: e.abs, channel });
                    }
                    return;
                }
                self.pointer = Some(e.abs);
                self.drag_to(cx, e.abs, e.modifiers.shift);
                if self.drag.is_some() {
                    cx.set_cursor(self.pointer_cursor(e.abs));
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(e) => {
                if let Some(mut drag) = self.zone_drag.take() {
                    drag.pointer = e.abs;
                    let zone = drag.finish();
                    if zone.is_some() || drag.original.is_some() {
                        let actions = self.settings.set_zone(zone);
                        self.emit(cx, actions);
                    }
                    self.pointer = Some(e.abs);
                    cx.set_cursor(self.pointer_cursor(e.abs));
                    self.area.redraw(cx);
                    return;
                }
                if self.delay_drag {
                    self.delay_drag = false;
                    self.pointer = Some(e.abs);
                    cx.widget_action(self.uid, ScopeAction::Delay(DelayGesture::End(e.abs)));
                    cx.set_cursor(if self.plot.contains(e.abs) { MouseCursor::Hand } else { MouseCursor::Default });
                    self.area.redraw(cx);
                    return;
                }
                self.drag_value_bracket(cx, e.abs);
                if self.value_bracket_off { self.edit_value_bracket(cx, None); }
                self.clear_value_bracket_drag();
                self.drag_bracket(cx, e.abs);
                if self.bracket_off { self.edit_bracket(cx, None); }
                self.clear_bracket_drag();
                self.trigger_press = None;
                if let Some((_, source)) = self.tag_press.take() {
                    if source != self.primary_source && self.part_at(e.abs) == Some(Part::Secondary) {
                        let action = self.swap_sources(cx);
                        self.emit(cx, vec![action]);
                    }
                }
                self.pointer = Some(e.abs);
                self.drag_to(cx, e.abs, e.modifiers.shift);
                if self.drag.is_some_and(|drag| drag.part == Part::Resize) {
                    cx.widget_action(self.uid, ScopeAction::Resize { height: self.height, done: true });
                }
                if let Some(other) = self.runt_preview.take() {
                    let actions = self.settings.set_runt(Some(other));
                    self.emit(cx, actions);
                    if !matches!(self.settings.trigger.qualify, Qualifier::Runt { .. }) { self.focused = Some(Part::LevelMarker); }
                }
                self.drag = None;
                self.view_drag = None;
                cx.set_cursor(self.pointer_cursor(e.abs));
                self.area.redraw(cx);
            }
            Hit::FingerScroll(e) => {
                if self.zone_drag.is_some() {
                    if let Event::Scroll(scroll) = event { scroll.handled_x.set(true); scroll.handled_y.set(true); }
                    return;
                }
                let part = match self.part_at(e.abs) {
                    Some(Part::Scale(i)) => Some(Part::Scale(i)),
                    Some(Part::Offset(i)) if e.modifiers.shift => Some(Part::Scale(i)),
                    Some(Part::Time) => Some(Part::Time),
                    Some(Part::Frequency | Part::Divider) => Some(Part::Frequency),
                    Some(Part::SpectrumScale | Part::SpectrumOffset) => Some(Part::SpectrumScale),
                    _ if self.spectrum().is_some() && self.spectrum_plot.contains(e.abs) => Some(if e.modifiers.shift { Part::SpectrumScale } else { Part::Frequency }),
                    _ if self.plot.contains(e.abs) && !self.collapsed => {
                        if e.modifiers.shift { self.trace_channel().map(Part::Scale) } else { Some(Part::Time) }
                    }
                    _ => None,
                };
                let delta = if e.scroll.y != 0.0 { e.scroll.y } else { e.scroll.x };
                if let Some(part) = part.filter(|_| delta.is_finite() && delta != 0.0) {
                    if let Event::Scroll(scroll) = event {
                        // Keep the bench's scroll container still while zooming a lane.
                        if e.scroll.y != 0.0 { scroll.handled_y.set(true); } else { scroll.handled_x.set(true); }
                    }
                    // Wheel up is a negative delta: lengthen the FFT record, narrowing its
                    // frequency span (fft_size / (2 * record)). Keep the shared time path.
                    self.zoom_view(cx, part, e.abs, (-delta * WHEEL_ZOOM).clamp(-60.0, 60.0).exp());
                }
            }
            Hit::FingerPinch(e) if self.zone_drag.is_none() && !self.collapsed && (self.plot.contains(e.abs) || self.spectrum().is_some() && self.spectrum_plot.contains(e.abs)) => {
                let part = if self.plot.contains(e.abs) { Part::Time } else { Part::Frequency };
                if e.scale.is_finite() && e.scale > 0.0 { self.zoom_view(cx, part, e.abs, 1.0 / e.scale); }
            }
            Hit::KeyDown(e) => match e.key_code {
                KeyCode::Escape if self.bracket_drag.is_some() => {
                    let bracket = self.bracket_press;
                    self.clear_bracket_drag();
                    self.edit_bracket(cx, bracket);
                    cx.set_cursor(self.pointer.map_or(MouseCursor::Default, |p| self.pointer_cursor(p)));
                    self.area.redraw(cx);
                }
                KeyCode::Escape if self.value_bracket_drag.is_some() => {
                    let bracket = self.value_bracket_press;
                    self.clear_value_bracket_drag();
                    self.edit_value_bracket(cx, bracket);
                    cx.set_cursor(self.pointer.map_or(MouseCursor::Default, |p| self.pointer_cursor(p)));
                    self.area.redraw(cx);
                }
                KeyCode::Escape if self.runt_preview.is_some() => {
                    self.runt_preview = None;
                    self.drag = None;
                    self.focused = Some(Part::LevelMarker);
                    cx.set_cursor(self.pointer.map_or(MouseCursor::Default, |p| self.pointer_cursor(p)));
                    self.area.redraw(cx);
                }
                KeyCode::Delete if self.focused == Some(Part::RuntLevel) => {
                    self.runt_preview = None;
                    self.drag = None;
                    self.focused = Some(Part::LevelMarker);
                    let actions = self.settings.set_runt(None);
                    self.emit(cx, actions);
                }
                KeyCode::KeyR if self.focused == Some(Part::LevelMarker) && !e.modifiers.control && !e.modifiers.logo && !e.modifiers.alt => {
                    let scale = self.settings.channels.get(self.settings.trigger.channel).map_or(1.0, |c| c.volts_per_div);
                    let other = self.settings.trigger.level + if self.settings.trigger.slope == Slope::Falling { -scale } else { scale };
                    let actions = self.settings.set_runt(Some(other));
                    self.emit(cx, actions);
                }
                KeyCode::Escape if self.zone_drag.is_some() => {
                    self.zone_drag = None;
                    cx.set_cursor(self.pointer.map_or(MouseCursor::Default, |p| self.pointer_cursor(p)));
                    self.area.redraw(cx);
                }
                KeyCode::Delete if self.focused == Some(Part::Zone) => {
                    self.zone_drag = None;
                    let actions = self.settings.set_zone(None);
                    self.emit(cx, actions);
                }
                KeyCode::Delete | KeyCode::Escape if self.delay_drag || self.delay_marks.iter().any(|mark| mark.end.is_some()) => {
                    // A hovered end selects its bracket; otherwise delete the newest on this lane.
                    let index = if self.delay_drag { None } else {
                        self.pointer.and_then(|pointer| self.delay_end(pointer)).map(|(index, ..)| index)
                            .or_else(|| self.delay_marks.iter().rposition(|mark| mark.end.is_some()))
                    };
                    self.delay_drag = false;
                    cx.widget_action(self.uid, ScopeAction::Delay(DelayGesture::Remove(index)));
                }
                KeyCode::Delete | KeyCode::Escape if self.value_bracket.is_some() => {
                    self.edit_value_bracket(cx, None);
                }
                KeyCode::Delete | KeyCode::Escape if self.bracket.is_some() => {
                    self.edit_bracket(cx, None);
                }
                KeyCode::Tab => {
                    if !self.parts.is_empty() {
                        let current = self.focused.and_then(|p| self.parts.iter().position(|r| r.part == p));
                        let mut next = match current {
                            Some(i) if e.modifiers.shift => (i + self.parts.len() - 1) % self.parts.len(),
                            Some(i) => (i + 1) % self.parts.len(),
                            None if e.modifiers.shift => self.parts.len() - 1,
                            None => 0,
                        };
                        // Numeric dividers and holdoff are keyboard stops for value entry too.
                        for _ in 0..self.parts.len() {
                            if !matches!(self.parts[next].part, Part::Answer(_)) { self.focused = Some(self.parts[next].part); break; }
                            next = if e.modifiers.shift { (next + self.parts.len() - 1) % self.parts.len() } else { (next + 1) % self.parts.len() };
                        }
                        self.area.redraw(cx);
                    }
                }
                KeyCode::KeyM if !e.modifiers.control && !e.modifiers.logo && !e.modifiers.alt => self.toggle_measure_palette(cx),
                KeyCode::Space if matches!(self.focused, Some(Part::More | Part::AddAnswer | Part::Secondary | Part::Spectrum | Part::Zone)) => self.adjust(cx, self.focused.unwrap(), 1),
                KeyCode::Space => { let actions = self.settings.set_running(!self.settings.running); self.emit(cx, actions); }
                KeyCode::ReturnKey | KeyCode::NumpadEnter => {
                    if let Some(part) = self.focused {
                        if !self.open_value_entry(cx, part) { self.adjust(cx, part, 1); }
                    }
                }
                KeyCode::ArrowUp | KeyCode::ArrowRight => {
                    if let Some(part) = self.focused.filter(|part| !matches!(part, Part::More | Part::AddAnswer | Part::Spectrum)) {
                        self.adjust(cx, part, 1);
                    }
                }
                KeyCode::ArrowDown | KeyCode::ArrowLeft => {
                    if let Some(part) = self.focused.filter(|part| !matches!(part, Part::More | Part::AddAnswer | Part::Spectrum)) {
                        self.adjust(cx, part, -1);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // Standalone lane hosts retain their one node. The bench draws this struct directly, so
    // none of these callbacks run for its children; it owns their lookup answers instead.
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, self.settings.channels.first().map_or("signal", |c| c.name.as_str()));
        makepad_platform::a11y::set_string(&mut node.value, &self.answers.text);
    }

    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let id_key = match lookup {
            SemanticLookup::Terms(q) => q.id_term().filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0).and_then(|id| cx.a11y.action_part_key(id)),
            SemanticLookup::Key(_) => None,
        };
        if let Some(lookup) = super::semantics::Lookup::new(lookup, id_key) { self.lookup(cx, &lookup, out); }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        let Some(key) = cx.a11y.action_part_key(request.node) else { return a11y_tree::ActionResult::Unhandled };
        self.lookup_action(cx, key, request)
    }

}
