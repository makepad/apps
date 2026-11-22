//! A live phosphor scope fed by PWM or PRBS acquisition workers.
//! Eye mode measures the PRBS eye height, width and crossing jitter on a reference worker.
//!
//! ELEC_SOURCE=bench selects the restricted eight-bit block scope.
//! Environment: ELEC_MEMBERS, ELEC_PACE, ELEC_LIGHT=1, ELEC_CLEAN=1, ELEC_EYE=1, ELEC_TWIN=1, and ELEC_SECONDS.

use fab_elec_instr::{Signal, prbs::Prbs, synth::{Spread, Synth}};
use fab_elec_bench::{measure, trigger, source::{resolve, ChanId, ChannelInfo, ChannelSetup, ClockKind, Coupling, DepthWant, ModeWant, Probe, Quantity, RateWant, Setup, Side, Source, SourceId, SourceInfo, TimeSetup, TriggerRun, TriggerSpec}};
use fab_elec_instr::{Limits, Synth as DriverSynth, SynthConfig, SynthChannel};
use std::sync::Arc;
use fab_elec_view::{
    instruments::{
        feed::{FeedConfig, FeedStats, PhosphorFeed, ScopeFeed},
        device_feed::DeviceFeed,
        acquire::{LaneFrame, LaneSetup, TriggerState},
        source_bar::{simulation_caps, block_blind, Answers as SourceAnswers, SourceBar, SourceBarAction, SourceChip, SourceState},
        scope::{Channel, EyeMarks, LaneSource, Measurement, Persistence, ScopeAction, SignalLane, ScopeSettings, Trigger},
        scope_host::{volts, ScopeHost},
        spectrum::{SpectrumMeasurements, SpectrumResult},
    },
    trace::{
        columns::Columns,
        spark::SPARK_COLS,
        eye::{eye_metrics, EyeMetrics},
    },
};
use makepad_widgets_core::*;
use std::time::{Duration, Instant};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.title: "Oscilloscope"
                window.inner_size: vec2(1400, 900)
                body +: {
                    flow: Down
                    source_bar := SourceBar {}
                    scope_card := SignalLane {width: Fill height: Fill}
                }
            }
        }
    }
}

const RECORD_PERIODS: usize = 256;
const EYE_PATTERNS: usize = 32;
const EYE_SAMPLES: usize = 65_536;

fn eye_source(clean: bool) -> Prbs {
    if clean {
        Prbs { jitter: 0.0, noise: 0.0, ..Prbs::gigabit() }
    } else {
        Prbs::gigabit()
    }
}

fn initial_settings() -> ScopeSettings {
    ScopeSettings {
        channels: vec![Channel {
            name: "PWM".into(),
            unit: "V".into(),
            volts_per_div: 1.0,
            offset_div: 2.0,
            on: true,
            color_index: 0,
        }],
        time_per_div: 100e-9,
        trigger: Trigger {
            channel: 0,
            level: 1.65,
            slope: trigger::Slope::Rising,
            // Lightly damped members ring back up through the level after the falling edge; a holdoff past that
            // edge and short of the next period keeps the view on the rising edges, as on a bench scope.
            holdoff: 20e-6,
            auto: true,
            qualify: trigger::Qualifier::None,
            zone: None,
        },
        position: 0.2,
        persistence: Persistence::Seconds(0.2),
        intensity: 1000.0,
        running: true,
        single: false,
        ..ScopeSettings::default()
    }
}

fn twin_signal(side: Side) -> Synth {
    Synth { freq: if side == Side::Real { 10_020.0 } else { 10_000.0 },
        // Independent voltage noise and pulse variation move the threshold crossings a little.
        noise: if side == Side::Real { 0.015 } else { 0.0 },
        wobble: if side == Side::Real { 0.003 } else { 0.0 },
        ..Synth::pwm() }
}

fn eye_settings() -> ScopeSettings {
    ScopeSettings {
        channels: vec![Channel {
            name: "DATA".into(),
            unit: "V".into(),
            volts_per_div: 0.2,
            offset_div: 2.0,
            on: true,
            color_index: 0,
        }],
        time_per_div: 200e-12,
        trigger: Trigger {
            channel: 0,
            level: 0.4,
            slope: trigger::Slope::Either,
            holdoff: 0.0,
            auto: true,
            qualify: trigger::Qualifier::None,
            zone: None,
        },
        position: 0.25,
        persistence: Persistence::Seconds(5.0),
        intensity: 1000.0,
        running: true,
        single: false,
        ..ScopeSettings::default()
    }
}

fn positive_env(name: &str) -> Option<f64> {
    let text = std::env::var(name).ok()?;
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() && value > 0.0 => Some(value),
        _ => {
            log!("elec_scope: ignoring {name}; expected a finite positive number");
            None
        }
    }
}

fn member_count() -> usize {
    let default = std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1));
    let Ok(text) = std::env::var("ELEC_MEMBERS") else { return default };
    match text.parse::<usize>() {
        Ok(value) if value > 0 => value,
        _ => {
            log!("elec_scope: ignoring ELEC_MEMBERS; expected a positive integer");
            default
        }
    }
}

struct Reference {
    twin: Option<Box<Reference>>,
    spark: Columns,
    measurements: Vec<Measurement>,
    eye: Option<EyeMetrics>,
    window_points: f64,
    window_seconds: f64,
}

/// Measure one clean PWM period and count samples inside complete trigger windows.
fn build_reference(settings: ScopeSettings, synth: Synth) -> Result<Reference, &'static str> {
    let period = synth.period();
    let span = RECORD_PERIODS as f64 * period;
    let mut count = RECORD_PERIODS * 256;
    let (times, values) = loop {
        let (mut times, mut values) = synth.record(count);
        if times.last().is_some_and(|&last| last >= span) {
            let end = times.partition_point(|&time| time < span);
            times.truncate(end);
            values.truncate(end);
            break (times, values);
        }
        count = count.checked_mul(2).ok_or("reference record is too large")?;
    };

    let end = times.partition_point(|&time| time <= period);
    let t = &times[..end];
    let y = &values[..end];
    // Frequency and duty need the next rising crossing to close this period.
    let closed_end = times.partition_point(|&time| time <= period + 0.5 / synth.ring_freq());
    let closed_t = &times[..closed_end];
    let closed_y = &values[..closed_end];
    let mut measurements = Vec::new();
    for (name, value, unit, scale) in [
        ("freq", measure::frequency(closed_t, closed_y), "Hz", 1.0),
        ("duty", measure::duty(closed_t, closed_y), "%", 100.0),
        ("Vpp", measure::vpp(y), "V", 1.0),
        ("mean", measure::mean(t, y), "V", 1.0),
        ("rise", measure::rise_time(t, y), "s", 1.0),
        ("overshoot", measure::overshoot(t, y), "%", 100.0),
    ] {
        measurements.push(Measurement::exact(0, name, value.ok_or("a clean PWM measurement is undefined")? * scale, unit));
    }

    let (window_points, window_seconds) = window_stats(&times, &values, &settings)?;
    let mut spark = Columns::default();
    spark.reduce_into(t, y, 0.0, period, SPARK_COLS);
    Ok(Reference { twin: None, spark, measurements, eye: None, window_points, window_seconds })
}

fn build_eye_reference(prbs: Prbs, settings: ScopeSettings) -> Result<Reference, &'static str> {
    let (times, values) = prbs.record(EYE_SAMPLES);
    let metrics = eye_metrics(&times, &values, prbs.ui()).ok_or("the eye record has no open eye to measure")?;
    let mut measurements: Vec<Measurement> = [
        ("eye height", metrics.height, "V"),
        ("eye width", metrics.width, "s"),
        ("jitter rms", metrics.jitter_rms, "s"),
        ("jitter p-p", metrics.jitter_pp, "s"),
    ]
    .into_iter()
    .map(|(name, value, unit)| Measurement::exact(0, name, value, unit))
    .collect();
    let (window_points, window_seconds) = window_stats(&times, &values, &settings)?;
    if let Some(mean) = measure::mean(&times, &values) { measurements.push(Measurement::exact(0, "mean", mean, "V")); }
    if let Some(vpp) = measure::vpp(&values) { measurements.push(Measurement::exact(0, "Vpp", vpp, "V")); }
    let mut spark = Columns::default();
    spark.reduce_into(&times, &values, 0.0, prbs.ui() * 2.0, SPARK_COLS);
    Ok(Reference { twin: None, spark, measurements, eye: Some(metrics), window_points, window_seconds })
}

fn window_stats(times: &[f64], values: &[f32], settings: &ScopeSettings) -> Result<(f64, f64), &'static str> {
    let window = settings.window();
    let mut points = 0usize;
    let mut windows = 0usize;
    for edge in trigger::events_qualified(times, values, &settings.trigger(), settings.trigger.qualify) {
        let left = edge - window.pre;
        let right = edge + window.post;
        if left < times[0] || right > times[times.len() - 1] {
            continue;
        }
        points += times.partition_point(|&time| time <= right) - times.partition_point(|&time| time < left);
        windows += 1;
    }
    if windows == 0 {
        return Err("the reference record contains no complete trigger window");
    }
    Ok((points as f64 / windows as f64, window.pre + window.post))
}

// Both feeds keep the UI on the same non-blocking ScopeHost path.
enum AppFeed { Simulation(PhosphorFeed), Bench(DeviceFeed<DriverSynth>) }
impl AppFeed {
    fn spectrum_result(&self) -> SpectrumResult {
        match self { Self::Simulation(feed) => feed.spectrum_result(), Self::Bench(feed) => SpectrumResult { measurements: feed.spectrum_measurements(), gate_too_short: false } }
    }
}
impl ScopeFeed for AppFeed {
    fn frame_is_current(&self, frame: &LaneFrame) -> bool { match self { Self::Simulation(f) => f.frame_is_current(frame), Self::Bench(f) => f.frame_is_current(frame) } }
    fn generation_is_current(&self, generation: u64) -> bool { match self { Self::Simulation(f) => f.generation_is_current(generation), Self::Bench(f) => f.generation_is_current(generation) } }
    fn take(&mut self) -> Option<LaneFrame> { match self { Self::Simulation(f) => f.take(), Self::Bench(f) => f.take() } }
    fn give_back(&mut self, frame: LaneFrame) { match self { Self::Simulation(f) => f.give_back(frame), Self::Bench(f) => f.give_back(frame) } }
    fn reconfigure(&mut self, trigger: trigger::EdgeTrigger, qualifier: trigger::Qualifier, channel: usize, window: trigger::Window, lanes: Vec<LaneSetup>, auto: Option<f64>, zone: Option<fab_elec_view::instruments::acquire::Zone>) {
        match self { Self::Simulation(f) => f.reconfigure(trigger, qualifier, channel, window, lanes, auto, zone), Self::Bench(f) => f.reconfigure(trigger, qualifier, channel, window, lanes, auto, zone) }
    }
    fn stats(&self) -> FeedStats { match self { Self::Simulation(f) => f.stats(), Self::Bench(f) => f.stats() } }
    fn trigger_state(&self) -> TriggerState { match self { Self::Simulation(f) => f.trigger_state(), Self::Bench(f) => f.trigger_state() } }
    fn stop(&mut self) { match self { Self::Simulation(f) => f.stop(), Self::Bench(f) => f.stop() } }
}

#[derive(Default)]
struct Report {
    since: Option<Instant>,
    previous: FeedStats,
    pushed: u64,
    uploaded: u64,
    draws: u64,
    draw_seconds: f64,
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    host: ScopeHost<AppFeed>,
    #[rust]
    bracket: Option<(f64, f64)>,
    #[rust]
    bracket_generation: u64,
    #[rust]
    twins: [ScopeHost<DeviceFeed<DriverSynth>>; 2],
    #[rust]
    twin: bool,
    #[rust]
    build: Option<TaskHandle<Result<Reference, &'static str>>>,
    #[rust]
    reference: Option<Reference>,
    #[rust]
    spectrum: SpectrumResult,
    #[rust]
    spectral_rows: Vec<Measurement>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    readout_timer: Timer,
    #[rust]
    report_timer: Timer,
    #[rust]
    quit_timer: Timer,
    #[rust]
    members: usize,
    #[rust]
    pace: Option<f64>,
    #[rust]
    light: bool,
    #[rust]
    clean: bool,
    #[rust]
    eye: bool,
    #[rust]
    report: Report,
    #[rust]
    finished: bool,
    #[rust]
    bench: bool,
    #[rust]
    source_chip: Option<SourceChip>,
    #[rust]
    twin_chips: Vec<SourceChip>,
    #[rust]
    source_stats: FeedStats,
    #[rust]
    source_at: Option<Instant>,
    #[rust]
    source_view: Option<(fab_elec_view::instruments::phosphor::PhosphorMap, Trigger)>,
}

impl App {
    fn request_frame(&mut self, cx: &mut Cx) {
        if !self.finished && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    /// Start only after drawing has established the device size.
    fn sync_plot(&mut self, cx: &mut Cx) {
        if self.twin { self.sync_twins(cx); return; }
        let widget = self.ui.widget(cx, ids!(scope_card));
        let Some(card) = widget.borrow::<SignalLane>() else { return };
        let (members, clean, eye, pace, bench) = (self.members, self.clean, self.eye, self.pace, self.bench);
        let bracket = self.bracket;
        let mut bracket_generation = None;
        let result = self.host.sync_plot(cx, &card, |cx, plot, settings| {
            let config = FeedConfig {
                members,
                source: if eye {
                    Signal::Prbs(eye_source(clean))
                } else {
                    Signal::Synth(if clean {
                        Synth::pwm()
                    } else {
                        Synth {
                            noise: 0.015,
                            wobble: 0.01,
                            runt: 1.0 / 200.0,
                            xtalk: 0.12,
                            xtalk_freq: 61_803.0,
                            ..Synth::pwm()
                        }
                    })
                },
                spread: if clean || eye {
                    Spread::default()
                } else {
                    Spread {
                        wn: 0.03,
                        zeta: 0.2,
                        height: 0.015,
                    }
                },
                record_periods: if eye { EYE_PATTERNS } else { RECORD_PERIODS },
                trigger: settings.trigger(),
                window: settings.window(),
                auto: settings.auto_after(),
                plot,
                volts: volts(settings, plot),
                frame_every: Duration::from_millis(8),
                pace,
            };
            if bench {
                let driver = DriverSynth::new(SynthConfig { side: Side::Sim, channels: vec![SynthChannel { signal: config.source, name: "CH1".into() }],
                    seed: 1, member_spread: None, pace: pace.or(Some(1.0)), limits: Some(Limits::bench_scope()) })
                    .map_err(|error| SpawnError::Backend(format!("{error:?}").into()))?;
                let mut feed = DeviceFeed::open(cx, driver)?;
                feed.reconfigure_scope(plot, settings);
                Ok(AppFeed::Bench(feed))
            } else {
                let feed = PhosphorFeed::start(cx, config)?;
                bracket_generation = Some(feed.set_span(bracket));
                Ok(AppFeed::Simulation(feed))
            }
        });
        if let Some(generation) = bracket_generation { self.bracket_generation = generation; }
        if let Err(error) = result {
            log!("elec_scope: could not start acquisition workers: {error:?}");
            self.finish(cx);
            cx.quit();
            return;
        }
        if self.host.wants_frame() || self.twins.iter().any(ScopeHost::wants_frame) {
            self.request_frame(cx);
        }
    }

    fn sync_twins(&mut self, cx: &mut Cx) {
        let widget = self.ui.widget(cx, ids!(scope_card));
        let Some(mut card) = widget.borrow_mut::<SignalLane>() else { return };
        let install_sources = self.twins.iter().any(|host| host.feed().is_none());
        let pace = self.pace;
        for (index, side) in [Side::Real, Side::Sim].into_iter().enumerate() {
            let result = self.twins[index].sync_plot(cx, &card, |cx, plot, settings| {
                let name = if side == Side::Real { "CH1" } else { "sim" };
                let driver = DriverSynth::new(SynthConfig { side,
                    channels: vec![SynthChannel { signal: Signal::Synth(twin_signal(side)), name: name.into() }],
                    seed: index as u64 + 1, member_spread: None, pace, limits: None,
                }).map_err(|e| SpawnError::Backend(format!("twin source: {e:?}").into()))?;
                let mut feed = DeviceFeed::open(cx, driver)?;
                feed.reconfigure_scope(plot, settings);
                Ok(feed)
            });
            if let Err(error) = result {
                log!("elec_scope: could not start twin acquisition: {error:?}");
                self.finish(cx); cx.quit(); return;
            }
        }
        if install_sources && self.twins.iter().all(|host| host.feed().is_some()) {
            let sources: Vec<_> = self.twins.iter().enumerate().map(|(index, host)| LaneSource {
                channel: None,
                side: host.feed().unwrap().info().side, name: if index == 0 { "CH1" } else { "sim" }.into(),
            }).collect();
            card.set_sources(&sources);
            card.redraw(cx);
        }
        if self.twins.iter().any(ScopeHost::wants_frame) { self.request_frame(cx); }
    }

    fn present(&mut self, cx: &mut Cx) {
        let widget = self.ui.widget(cx, ids!(scope_card));
        let Some(mut card) = widget.borrow_mut::<SignalLane>() else { return };
        self.update_span(cx, &mut card);
        if self.twin {
            for (index, host) in self.twins.iter_mut().enumerate() {
                if let Some(uploaded) = host.present_source(cx, &mut card, index) {
                    self.report.pushed += 1; self.report.uploaded += uploaded as u64;
                }
            }
        } else if let Some(uploaded) = self.host.present(cx, &mut card) {
            self.report.pushed += 1;
            self.report.uploaded += uploaded as u64;
        }
        if self.host.wants_frame() || self.twins.iter().any(ScopeHost::wants_frame) {
            self.request_frame(cx);
        }
    }

    fn poll_reference(&mut self, cx: &mut Cx) {
        let Some(build) = &mut self.build else { return };
        let Some(result) = build.try_take() else { return };
        self.build = None;
        match result {
            Ok(Ok(reference)) => {
                if let Some(eye) = reference.eye {
                    if let Some(mut card) = self.ui.widget(cx, ids!(scope_card)).borrow_mut::<SignalLane>() {
                        let threshold = card.settings().trigger.level;
                        card.set_eye_marks(cx, Some(EyeMarks {
                            ui: eye_source(self.clean).ui(),
                            top: eye.top,
                            bottom: eye.bottom,
                            threshold,
                            jitter_pp: eye.jitter_pp,
                        }));
                    }
                }
                self.reference = Some(reference);
            }
            Ok(Err(error)) => {
                log!("elec_scope: reference measurements failed: {error}");
                self.finish(cx);
                cx.quit();
            }
            Err(error) => {
                log!("elec_scope: reference worker failed: {error:?}");
                self.finish(cx);
                cx.quit();
            }
        }
    }

    fn update_span(&self, cx: &mut Cx, card: &mut SignalLane) {
        card.set_bracket(cx, self.bracket);
        if self.bracket.is_none() { return; }
        if let Some(AppFeed::Simulation(feed)) = self.host.feed() {
            if let Some((generation, stats)) = feed.spans() {
                card.set_span_stats(cx, stats.first().copied().flatten(), generation == self.bracket_generation);
            } else { card.set_span_stats(cx, None, false); }
        } else {
            // Device/twin sources do not publish capture spans yet: show the duration alone.
            card.set_span_stats(cx, None, self.twin || self.bench);
        }
    }

    fn update_measurements(&mut self, cx: &mut Cx) {
        if let Some(mut card) = self.ui.widget(cx, ids!(scope_card)).borrow_mut::<SignalLane>() {
            self.update_span(cx, &mut card);
            let features = if self.twin { None } else { self.host.feed().and_then(|feed| match feed {
                AppFeed::Simulation(feed) => feed.features(), AppFeed::Bench(_) => None,
            }) };
            card.set_features(cx, features, 0);
            if self.twin {
                for (index, host) in self.twins.iter().enumerate() {
                    let reference = self.reference.as_ref().and_then(|reference| if index == 0 { Some(reference) } else { reference.twin.as_deref() });
                    let time = reference.map_or(&[][..], |reference| reference.measurements.as_slice());
                    let spectrum = card.spectrum().and_then(|_| host.feed().and_then(DeviceFeed::spectrum_measurements));
                    let rows = spectrum.map(SpectrumMeasurements::rows).unwrap_or_default();
                    card.set_source_measurements_with_spectrum(cx, index, time, &rows);
                    if index == card.primary_source() {
                        card.set_spectrum_measurements(cx, spectrum);
                        if card.collapsed() { if let Some(reference) = reference { card.spark().set_columns(cx, 0, &reference.spark); } }
                    }
                }
                return;
            }
            let spectrum = if card.spectrum().is_some() { self.host.feed().map_or_else(SpectrumResult::default, AppFeed::spectrum_result) } else { SpectrumResult::default() };
            if self.spectrum != spectrum {
                self.spectral_rows = spectrum.rows();
                self.spectrum = spectrum;
            }
            card.set_spectrum_measurements(cx, spectrum.measurements);
            if let Some(AppFeed::Bench(feed)) = self.host.feed() {
                if let Some(rows) = feed.measurements() {
                    let mut rows = (*rows).clone();
                    if let Some(resolved) = feed.source().status().resolved {
                        for row in &mut rows {
                            if let Some(channel) = resolved.channels.get(row.lane) { row.set_resolution(&resolved, channel.id); }
                        }
                    }
                    card.set_source_measurements_with_spectrum(cx, 0, &rows, &self.spectral_rows);
                }
            } else if let Some(reference) = self.reference.as_ref().filter(|_| !self.bench) {
                card.set_source_measurements_with_spectrum(cx, 0, &reference.measurements, &self.spectral_rows);
                if card.collapsed() { card.spark().set_columns(cx, 0, &reference.spark); }
            }
        }
    }

    fn update_source(&mut self, cx: &mut Cx) {
        if self.twin { self.update_twin_sources(cx); return; }
        let lane_widget = self.ui.widget(cx, ids!(scope_card));
        let Some(mut lane) = lane_widget.borrow_mut::<SignalLane>() else { return };
        let Some(feed) = self.host.feed() else { return };
        let chip = match feed {
            AppFeed::Bench(feed) => {
                let status = feed.source().status();
                let state = if lane.settings().running { SourceState::from_status(&status) } else { SourceState::Paused };
                let same = self.source_chip.as_ref().is_some_and(|c| c.state == state && match (&c.resolved, &status.resolved) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b), (None, None) => true, _ => false,
                });
                if same { None } else { Some(SourceChip { info: feed.source().info().clone(), caps: feed.source().caps(), state,
                    resolved: status.resolved, trigger_kind: lane.settings().trigger.qualify.kind(), note: None }) }
            }
            AppFeed::Simulation(_) => {
                let state = if lane.settings().running { SourceState::Live } else { SourceState::Paused };
                let map = lane.settings().map(lane.plot_px().unwrap_or((1, 1)));
                let key = (map, lane.settings().trigger);
                if self.source_chip.as_ref().is_some_and(|c| c.state == state) && self.source_view == Some(key) { None } else {
                    self.source_view = Some(key);
                    let caps = Arc::new(simulation_caps(vec![ChannelInfo { id: ChanId(0), name: if self.eye { "DATA".into() } else { "PWM".into() }, quantity: Quantity::Volt }]));
                    let span = map.t_px * map.w as f64;
                    let trigger = lane.settings().trigger();
                    let resolved = resolve(&caps, &Setup {
                        channels: vec![ChannelSetup { id: ChanId(0), on: true, window: (map.v_top - map.v_px * map.h as f64, map.v_top),
                            coupling: Coupling::Dc, probe: Probe::default(), bw_limit: None, invert: false }],
                        time: TimeSetup { span, pre: -map.t_left, rate: RateWant::Auto { per_px: 2.0 }, depth: DepthWant::Auto, plot_px: map.w },
                        trigger: TriggerSpec { kind: lane.settings().trigger.qualify.kind(), channel: ChanId(0), level: trigger.level, hyst: trigger.hyst, slope: trigger.slope,
                            holdoff: trigger.holdoff, run: lane.settings().auto_after().map_or(TriggerRun::Normal, TriggerRun::Auto) },
                        mode: ModeWant::Stream, drives: Vec::new(),
                    }, None);
                    Some(SourceChip { info: SourceInfo { id: SourceId(0), side: Side::Sim, name: "Simulation".into(), serial: None, clock: ClockKind::Simulated },
                        state, caps, resolved: Some(Arc::new(resolved)), trigger_kind: lane.settings().trigger.qualify.kind(), note: None })
                }
            }
        };
        let bar_widget = self.ui.widget(cx, ids!(source_bar));
        if let Some(mut bar) = bar_widget.borrow_mut::<SourceBar>() {
            if let Some(chip) = chip {
                lane.set_capabilities(cx, chip.caps.clone(), chip.resolved.clone(), ChanId(0));
                lane.set_sources(&[LaneSource { channel: None, side: chip.info.side, name: if self.bench { "CH1" } else { "sim" }.into() }]);
                if bar.set_sources(std::slice::from_ref(&chip)) { bar.redraw(cx); }
                self.source_chip = Some(chip);
            }
            bar.set_look(cx, lane.settings().persistence, lane.settings().intensity, 0);
            let now = Instant::now();
            let elapsed = self.source_at.map_or(0.0, |at| now.duration_since(at).as_secs_f64());
            if self.source_at.is_none() { self.source_at = Some(now); self.source_stats = self.host.stats(); }
            else if elapsed >= 1.0 {
                let stats = self.host.stats();
                let blind = self.source_chip.as_ref().and_then(|c| c.resolved.as_ref().and_then(|r| block_blind(&c.caps, r)));
                let mut answers = SourceAnswers::from_stats(stats, self.source_stats, elapsed, blind, if self.bench { None } else { Some(stats.sim_time_ns.saturating_sub(self.source_stats.sim_time_ns) as f64 / 1e9 / elapsed) });
                if let AppFeed::Bench(feed) = feed {
                    let status = feed.source().status();
                    if let Some(super_blind) = blind {
                        answers.coverage = Some(fab_elec_view::instruments::source_bar::Coverage::Blocks {
                            captures: feed.source().capture_rate(), blind: 1.0 - (1.0 - match answers.coverage {
                                Some(fab_elec_view::instruments::source_bar::Coverage::Blocks { blind, .. }) => blind, _ => super_blind,
                            }) * status.coverage as f64,
                        });
                    }
                }
                bar.set_answers(cx, answers);
                self.source_at = Some(now); self.source_stats = stats;
            }
        };
    }

    fn update_twin_sources(&mut self, cx: &mut Cx) {
        let widget = self.ui.widget(cx, ids!(scope_card));
        let Some(mut lane) = widget.borrow_mut::<SignalLane>() else { return };
        for (index, host) in self.twins.iter().enumerate() {
            let Some(feed) = host.feed() else { return };
            let status = feed.source().status();
            let state = if lane.settings().running { SourceState::from_status(&status) } else { SourceState::Paused };
            let same = self.twin_chips.get(index).is_some_and(|c| c.state == state && match (&c.resolved, &status.resolved) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b), (None, None) => true, _ => false,
            });
            if !same {
                let chip = SourceChip { info: feed.info().clone(), caps: feed.source().caps(), state,
                    resolved: status.resolved, trigger_kind: lane.settings().trigger.qualify.kind(), note: None };
                if index < self.twin_chips.len() { self.twin_chips[index] = chip; } else { self.twin_chips.push(chip); }
            }
        }
        let primary = lane.primary_source();
        let chip = &self.twin_chips[primary];
        lane.set_capabilities(cx, chip.caps.clone(), chip.resolved.clone(), ChanId(0));
        let bar_widget = self.ui.widget(cx, ids!(source_bar));
        if let Some(mut bar) = bar_widget.borrow_mut::<SourceBar>() {
            if bar.set_sources(&self.twin_chips) { bar.redraw(cx); }
            bar.set_look(cx, lane.settings().persistence, lane.settings().intensity, 0);
            let now = Instant::now();
            let stats = self.twins[primary].stats();
            let elapsed = self.source_at.map_or(0.0, |at| now.duration_since(at).as_secs_f64());
            if self.source_at.is_none() { self.source_at = Some(now); self.source_stats = stats; }
            else if elapsed >= 1.0 {
                bar.set_answers(cx, SourceAnswers::from_stats(stats, self.source_stats, elapsed, None, None));
                self.source_at = Some(now); self.source_stats = stats;
            }
        };
    }

    fn scope_action(&mut self, cx: &mut Cx, card: &mut SignalLane, action: ScopeAction) {
        if let ScopeAction::Bracket(bracket) = &action {
            self.bracket = *bracket;
            if let Some(AppFeed::Simulation(feed)) = self.host.feed() { self.bracket_generation = feed.set_span(*bracket); }
            card.set_bracket(cx, *bracket);
            self.request_frame(cx);
            return;
        }
        if self.twin {
            if let ScopeAction::SwapSources { primary } = &action {
                self.source_at = None;
                if let Some(feed) = self.twins[*primary].feed() {
                    card.set_capabilities(cx, feed.source().caps(), feed.source().status().resolved, ChanId(0));
                    card.set_trigger_state(feed.trigger_state());
                }
            }
            for host in &mut self.twins { host.handle_action(cx, card, action.clone(), self.light); }
        } else { self.host.handle_action(cx, card, action, self.light); }
    }

    fn log_report(&mut self, final_line: bool) {
        let now = Instant::now();
        let elapsed = self.report.since.map_or(0.0, |since| now.duration_since(since).as_secs_f64()).max(f64::EPSILON);
        let totals = if self.twin {
            let a = self.twins[0].stats(); let b = self.twins[1].stats();
            FeedStats { trigger_events: a.trigger_events + b.trigger_events, sim_time_ns: a.sim_time_ns + b.sim_time_ns,
                waveforms: a.waveforms + b.waveforms, samples: a.samples + b.samples,
                frames: a.frames + b.frames, dropped: a.dropped + b.dropped, refused: a.refused + b.refused, unresolved: a.unresolved || b.unresolved }
        } else { self.host.stats() };
        let previous = self.report.previous;
        let windows = self.reference.as_ref().map_or_else(
            || "window sample count pending".to_owned(),
            |reference| {
                format!(
                    "{:.1}-point windows on average at startup, {:.3} us",
                    reference.window_points,
                    reference.window_seconds * 1e6
                )
            },
        );
        log!(
            "elec_scope: {:.0} waveforms/s ({}), {:.1} M samples/s, {:.1} frames/s, plot {} device px, upload {:.1} KB/frame, dropped {} (total {}), UI draw {:.3} ms/frame{}",
            totals.waveforms.saturating_sub(previous.waveforms) as f64 / elapsed,
            windows,
            totals.samples.saturating_sub(previous.samples) as f64 / elapsed / 1e6,
            totals.frames.saturating_sub(previous.frames) as f64 / elapsed,
            (if self.twin { self.twins[0].plot() } else { self.host.plot() }).map_or_else(|| "pending".to_owned(), |(w, h)| format!("{w} x {h}")),
            self.report.uploaded as f64 / self.report.pushed.max(1) as f64 / 1000.0,
            totals.dropped.saturating_sub(previous.dropped),
            totals.dropped,
            self.report.draw_seconds * 1000.0 / self.report.draws.max(1) as f64,
            if final_line { " (final)" } else { "" },
        );
        self.report = Report {
            since: Some(now),
            previous: totals,
            ..Report::default()
        };
    }

    fn finish(&mut self, cx: &mut Cx) {
        if self.finished {
            return;
        }
        self.host.stop();
        for host in &mut self.twins { host.stop(); }
        self.log_report(true);
        cx.stop_timer(self.readout_timer);
        cx.stop_timer(self.report_timer);
        cx.stop_timer(self.quit_timer);
        self.finished = true;
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.light = std::env::var("ELEC_LIGHT").is_ok_and(|value| value == "1");
        self.clean = std::env::var("ELEC_CLEAN").is_ok_and(|value| value == "1");
        self.twin = std::env::var("ELEC_TWIN").is_ok_and(|value| value == "1");
        self.eye = !self.twin && std::env::var("ELEC_EYE").is_ok_and(|value| value == "1");
        self.bench = !self.twin && std::env::var("ELEC_SOURCE").is_ok_and(|value| value == "bench");
        self.members = if self.bench { 1 } else { member_count() };
        self.pace = positive_env("ELEC_PACE");
        log!(
            "elec_scope: startup members={}, pace={:?}, ELEC_LIGHT={}, ELEC_CLEAN={}, ELEC_EYE={}, record_periods={}",
            self.members,
            self.pace,
            u8::from(self.light),
            u8::from(self.clean),
            u8::from(self.eye),
            if self.eye { EYE_PATTERNS } else { RECORD_PERIODS },
        );
        self.report.since = Some(Instant::now());
        let mut settings = if self.eye { eye_settings() } else { initial_settings() };
        if self.bench { settings.time_per_div = 10e-6; settings.channels[0].offset_div = 0.0; }
        if self.twin { settings.time_per_div = 20e-6; settings.trigger.holdoff = 40e-6; }
        if let Some(mut card) = self.ui.widget(cx, ids!(scope_card)).borrow_mut::<SignalLane>() {
            card.set_settings(cx, settings.clone());
            if !self.twin && !self.bench { card.set_sources(&[LaneSource { channel: None, side: Side::Sim, name: "sim".into() }]); }
            card.phosphor().set_look(cx, settings.look(self.light));
        }
        let (eye, clean, twin) = (self.eye, self.clean, self.twin);
        if !self.bench {
            match cx.spawn_worker(move || {
                if eye { build_eye_reference(eye_source(clean), settings) }
                else if twin {
                    let mut real = build_reference(settings.clone(), twin_signal(Side::Real))?;
                    real.twin = Some(Box::new(build_reference(settings, twin_signal(Side::Sim))?));
                    Ok(real)
                } else { build_reference(settings, Synth { noise: 0.0, ..Synth::pwm() }) }
            }) {
                Ok(build) => self.build = Some(build),
                Err(error) => {
                    log!("elec_scope: could not start reference worker: {error:?}");
                    self.finish(cx);
                    cx.quit();
                    return;
                }
            }
        }
        self.readout_timer = cx.start_interval(0.25);
        self.report_timer = cx.start_interval(2.0);
        if let Some(seconds) = positive_env("ELEC_SECONDS") {
            self.quit_timer = cx.start_timeout(seconds);
        }
        self.request_frame(cx);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let bar = self.ui.widget(cx, ids!(source_bar));
        let widget = self.ui.widget(cx, ids!(scope_card));
        let uid = widget.widget_uid();
        let Some(mut card) = widget.borrow_mut::<SignalLane>() else { return };
        for action in actions.filter_widget_actions_cast::<SourceBarAction>(bar.widget_uid()) {
            match action {
                SourceBarAction::Look { persistence, intensity } => {
                    let mut settings = card.settings().clone(); settings.persistence = persistence; settings.intensity = intensity;
                    card.set_settings(cx, settings.clone());
                    self.scope_action(cx, &mut card, ScopeAction::Reconfigure(settings));
                }
                SourceBarAction::Clear => self.scope_action(cx, &mut card, ScopeAction::Clear),
                SourceBarAction::SeeAs(_) | SourceBarAction::None => {}
            }
        }
        for action in actions.filter_widget_actions_cast::<ScopeAction>(uid) {
            self.scope_action(cx, &mut card, action);
        }
        if self.host.wants_frame() || self.twins.iter().any(ScopeHost::wants_frame) {
            self.request_frame(cx);
        }
    }

    fn handle_timer(&mut self, cx: &mut Cx, event: &TimerEvent) {
        if self.quit_timer.is_timer(event).is_some() {
            self.finish(cx);
            cx.quit();
        } else if self.readout_timer.is_timer(event).is_some() {
            self.poll_reference(cx);
            if self.finished {
                return;
            }
            self.update_measurements(cx);
            self.update_source(cx);
        } else if self.report_timer.is_timer(event).is_some() {
            self.log_report(false);
        }
    }

    fn handle_shutdown(&mut self, cx: &mut Cx) {
        self.finish(cx);
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        if std::env::var("ELEC_LIGHT").is_ok_and(|value| value == "1") {
            set_base_theme(vm.cx_mut(), BaseTheme::Light);
        }
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        fab_elec_view::instruments::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        let draw_started = matches!(event, Event::Draw(_)).then(Instant::now);
        if self.finished {
            return;
        }
        self.match_event(cx, event);
        if self.finished {
            return;
        }
        if matches!(event, Event::Signal) || self.next_frame.is_event(event).is_some() {
            self.poll_reference(cx);
            if self.finished {
                return;
            }
            self.present(cx);
            if self.build.is_some() || (if self.twin { self.twins[0].plot() } else { self.host.plot() }).is_none() {
                self.request_frame(cx);
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
        if let Some(started) = draw_started {
            self.sync_plot(cx);
            self.report.draws += 1;
            self.report.draw_seconds += started.elapsed().as_secs_f64();
        }
    }
}
