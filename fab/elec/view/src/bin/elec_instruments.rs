//! The instruments harness: five trace views of one long synthetic record (a ringing PWM and its
//! low-passed current, at a solver's non-uniform steps) in the solver's own trace store, each at
//! another zoom from the whole record down to single solver steps, all read through the pyramid.
//! For the first frames after the record is built every view pans, so each frame reduces anew,
//! and the frame times are logged.
//!
//! A time strip under them, full width and compact, shows the record's span. Eight net sparks
//! below the strips share the hovered trace as a shadow.
//!
//! Environment: `ELEC_POINTS` (record length, default 10 000 000), `ELEC_LIGHT=1` (light theme),
//! `ELEC_QUIT=1` (quit after the timed frames' report, for a timed run).

use fab_elec_instr::synth::Synth;
use fab_elec_design::{CursorSource, TimeCursor};
use fab_elec_sim::trace::TraceStore;
use fab_elec_view::live::{
    strip::{GhostState, StripModel, TimeStrip},
    LiveLook,
};
use fab_elec_view::trace::{
    spark::{NetSpark, NetSparkAction, SparkShadow},
    view::{HoverTrace, StoreSource, TraceSource, TraceView},
};
use makepad_widgets_core::*;
use std::{sync::Arc, time::Instant};

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    load_all_resources() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(1600, 1000)
                body +: {
                    flow: Down
                    spacing: 2
                    a11y: {role: "group" label: "Trace zooms"}
                    zoom0 := TraceView {}
                    zoom1 := TraceView {}
                    zoom2 := TraceView {}
                    zoom3 := TraceView {}
                    zoom4 := TraceView {}
                    strips := View {
                        width: Fill height: Fit flow: Right spacing: 8
                        strip_full := TimeStrip {}
                        strip_compact := TimeStrip { width: 480 }
                    }
                    sparks := View {
                        flow: Right spacing: 8 height: 24
                        spark0 := NetSpark {}
                        spark1 := NetSpark {}
                        spark2 := NetSpark {}
                        spark3 := NetSpark {}
                        spark4 := NetSpark {}
                        spark5 := NetSpark {}
                        spark6 := NetSpark {}
                        spark7 := NetSpark {}
                    }
                }
            }
        }
    }
}

/// The five windows, seconds: the whole record, 40 periods, two periods, one edge's ringing, the
/// solver steps of its first overshoot. The edge is the second period's (t = 50 us).
const ZOOMS: [Option<(f64, f64)>; 5] = [None, Some((0.0, 2e-3)), Some((0.0, 100e-6)), Some((49.9e-6, 51.0e-6)), Some((49.99e-6, 50.12e-6))];
/// Frames that pan after the record arrives, timed.
const TIMED_FRAMES: u32 = 600;

/// The record as the solver would store it: signal 0 the PWM node, signal 1 its low-passed current.
fn build_record(points: usize) -> Arc<TraceStore> {
    let started = Instant::now();
    let (t, y) = Synth::pwm().record(points);
    let mut store = TraceStore::new(1, &[false, false]);
    let mut closed = Vec::new();
    let tau = 20e-6;
    let mut lp = 0.0f64;
    for k in 0..t.len() {
        if k > 0 {
            lp += (y[k] as f64 - lp) * (1.0 - (-(t[k] - t[k - 1]) / tau).exp());
        }
        store.push(t[k], &[y[k] as f64, lp * 6e-3], &mut closed);
        closed.clear();
    }
    log!("elec_instruments: {} points ({:.3} s simulated) built in {:.2} s", store.len(), t.last().copied().unwrap_or(0.0), started.elapsed().as_secs_f64());
    Arc::new(store)
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    build: Option<TaskHandle<Arc<TraceStore>>>,
    #[rust]
    store: Option<Arc<TraceStore>>,
    #[rust]
    source: Option<Arc<StoreSource>>,
    /// The spark under the pointer and its trace.
    #[rust]
    hovered: Option<(WidgetUid, usize)>,
    /// Panned frames drawn so far and their draw times, seconds.
    #[rust]
    timed: Vec<f64>,
}

impl App {
    fn views(&self, cx: &mut Cx) -> [WidgetRef; 5] {
        [
            self.ui.widget(cx, ids!(zoom0)),
            self.ui.widget(cx, ids!(zoom1)),
            self.ui.widget(cx, ids!(zoom2)),
            self.ui.widget(cx, ids!(zoom3)),
            self.ui.widget(cx, ids!(zoom4)),
        ]
    }

    fn sparks(&self, cx: &mut Cx) -> [WidgetRef; 8] {
        [
            self.ui.widget(cx, ids!(spark0)),
            self.ui.widget(cx, ids!(spark1)),
            self.ui.widget(cx, ids!(spark2)),
            self.ui.widget(cx, ids!(spark3)),
            self.ui.widget(cx, ids!(spark4)),
            self.ui.widget(cx, ids!(spark5)),
            self.ui.widget(cx, ids!(spark6)),
            self.ui.widget(cx, ids!(spark7)),
        ]
    }

    fn attach(&mut self, cx: &mut Cx, store: Arc<TraceStore>) {
        let source = Arc::new(StoreSource { store: store.clone() });
        for (view, zoom) in self.views(cx).iter().zip(ZOOMS) {
            if let Some(mut view) = view.borrow_mut::<TraceView>() {
                view.set_source(cx, source.clone());
                if let Some((t0, t1)) = zoom {
                    view.set_window(cx, t0, t1);
                }
            }
        }
        if let Some((t0, t1)) = source.span().filter(|_| source.traces() > 0) {
            let look = if base_theme(cx) == BaseTheme::Light { LiveLook::light() } else { LiveLook::dark() };
            for (k, spark) in self.sparks(cx).iter().enumerate() {
                if let Some(mut spark) = spark.borrow_mut::<NetSpark>() {
                    spark.set_trace(cx, source.clone(), k % source.traces());
                    spark.set_window(cx, t0, t1);
                    spark.set_look(cx, look);
                }
            }
        }
        self.source = Some(source);
        // the time strip, full width and compact, over the record's span
        let span = (store.time.at(0), store.time.at(store.time.len() - 1));
        let model = StripModel {
            span,
            activity: (0..512).map(|k| 0.35 + 0.3 * ((k as f32) * 0.07).sin().abs()).collect(),
            failures: vec![span.0 + 0.41 * (span.1 - span.0)],
            markers: vec![(50e-6, "first edge".into()), (span.0 + 0.75 * (span.1 - span.0), "knob turned".into())],
            ratio: Some(0.0005),
            ghost: Some(GhostState::UnderLive),
            live: false,
        };
        let cursor = TimeCursor { t: span.0 + 0.3 * (span.1 - span.0), playing: false, rate: 1.0, source: CursorSource::Sim, fw: None };
        for id in [ids!(strip_full), ids!(strip_compact)] {
            if let Some(mut strip) = self.ui.widget(cx, id).borrow_mut::<TimeStrip>() {
                strip.set_model(cx, model.clone());
                strip.hear(cx, cursor);
            }
        }
        self.store = Some(store);
        cx.new_next_frame();
    }

    /// Pan every zoomed view by a small fraction of its window, so the next frame reduces anew.
    fn pan(&mut self, cx: &mut Cx) {
        for view in self.views(cx).iter().skip(1) {
            if let Some(mut view) = view.borrow_mut::<TraceView>() {
                let (t0, t1) = view.window();
                // back and forth around the start window, so the zooms keep their subject
                let phase = (self.timed.len() / 150) % 4;
                let dir = if phase == 0 || phase == 3 { 1.0 } else { -1.0 };
                let step = (t1 - t0) * 0.002 * dir;
                view.set_window(cx, t0 + step, t1 + step);
            }
        }
    }

    fn report(&self) {
        // the slowest frames by index: a first-use cost shows as frame 0 or 1, a steady cost spreads
        let mut slow: Vec<(usize, f64)> = self.timed.iter().copied().enumerate().collect();
        slow.sort_by(|a, b| b.1.total_cmp(&a.1));
        let slow: Vec<String> = slow.iter().take(5).map(|(i, t)| format!("#{i} {:.2} ms", t * 1e3)).collect();
        log!("elec_instruments: slowest frames {}", slow.join(", "));
        let mut v = self.timed.clone();
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        let at = |q: f64| v.get(((v.len() as f64 - 1.0) * q).round() as usize).copied().unwrap_or(0.0) * 1e3;
        log!(
            "elec_instruments: {} panned frames of 5 views: draw mean {:.2} ms, p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
            v.len(),
            mean * 1e3,
            at(0.5),
            at(0.95),
            at(1.0)
        );
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // A move between sparks can enter one and leave the other in either order, in one batch or
        // two: only leaving the spark that is hovered now clears the shadow.
        let mut hovered = self.hovered;
        for action in actions.iter().filter_map(|action| action.as_widget_action()) {
            match action.cast::<NetSparkAction>() {
                NetSparkAction::Hovered(k) => hovered = Some((action.widget_uid, k)),
                NetSparkAction::Unhovered(_) if hovered.is_some_and(|(uid, _)| uid == action.widget_uid) => hovered = None,
                _ => {}
            }
        }
        if hovered.map(|(_, k)| k) == self.hovered.map(|(_, k)| k) {
            self.hovered = hovered;
            return;
        }
        self.hovered = hovered;
        let hover = hovered.and_then(|(_, k)| self.source.as_ref().map(|source| HoverTrace { source: source.clone(), k }));
        for view in self.views(cx) {
            if let Some(mut view) = view.borrow_mut::<TraceView>() {
                view.set_hover(cx, hover.clone());
            }
        }
        let shadow = hovered.and_then(|(_, k)| {
            let source = self.source.as_ref()?;
            let window = source.span()?;
            Some(Arc::new(SparkShadow::read(source.as_ref(), k, window)))
        });
        for spark in self.sparks(cx) {
            if let Some(mut spark) = spark.borrow_mut::<NetSpark>() {
                spark.set_shadow(cx, shadow.clone());
            }
        }
    }

    fn handle_startup(&mut self, cx: &mut Cx) {
        let points = std::env::var("ELEC_POINTS").ok().and_then(|v| v.replace(['_', ' '], "").parse().ok()).unwrap_or(10_000_000usize);
        match cx.spawn_worker(move || build_record(points)) {
            Ok(handle) => self.build = Some(handle),
            Err(e) => log!("elec_instruments: no worker thread ({e:?}); building on the UI thread"),
        }
        if self.build.is_none() {
            self.attach(cx, build_record(points));
        } else {
            cx.new_next_frame();
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        if std::env::var("ELEC_LIGHT").is_ok_and(|v| v == "1") {
            set_base_theme(vm.cx_mut(), BaseTheme::Light);
        }
        makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod]);
        fab_elec_view::live::script_mod(vm);
        fab_elec_view::live::strip::script_mod(vm);
        fab_elec_view::trace::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        self.match_event(cx, event);
        if let Event::NextFrame(_) = event {
            if let Some(handle) = &mut self.build {
                match handle.try_take() {
                    Some(Ok(store)) => {
                        self.build = None;
                        self.attach(cx, store);
                    }
                    Some(Err(e)) => {
                        log!("elec_instruments: building the record failed: {e:?}");
                        self.build = None;
                    }
                    None => {
                        cx.new_next_frame();
                    }
                }
            } else if self.store.is_some() && (self.timed.len() as u32) < TIMED_FRAMES {
                self.pan(cx);
                cx.new_next_frame();
            }
        }
        let draw = matches!(event, Event::Draw(_));
        let started = Instant::now();
        self.ui.handle_event(cx, event, &mut Scope::empty());
        if draw && self.store.is_some() && (self.timed.len() as u32) < TIMED_FRAMES {
            self.timed.push(started.elapsed().as_secs_f64());
            if self.timed.len() as u32 == TIMED_FRAMES {
                self.report();
                if std::env::var("ELEC_QUIT").is_ok_and(|v| v == "1") {
                    cx.quit();
                }
            }
        }
    }
}
