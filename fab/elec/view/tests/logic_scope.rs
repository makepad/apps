//! The Scope tab's model: one timebase for every lane, logic levels with the undefined band, a
//! trigger found through any trace source, and which of ledavr's probes draw as logic.

use fab_elec_bench::trigger::Slope;
use fab_elec_circuit::NetKind;
use fab_elec_view::instruments::logic::{
    cursor_for_click, cursor_readout, edge_runs, find_logic_trigger, find_trigger, follow_trigger, hold, last_crossing, last_edge, logic_runs, move_to_gap,
    nearest_edge, nearest_logic_edge, net_facts, next_kind, next_trigger, step_125, swings_like_logic,
    Anchor, LaneKind, Level, Run, Thresholds, Timebase, Trigger, TrigMode, Which, AUTO_SPANS,
};
use fab_elec_design::{BenchOp, DesignOp, ProbeOf};
use fab_elec_sim::spill::LogicSpill;
use fab_elec_sim::trace::{LogicEdge, LogicTrace, SignalId, TraceStore, LOGIC_CHUNK, LOGIC_KEEP};
use fab_elec_view::{live::probe::{click_probe_op, rename_keeps, rename_probe_op}, snapshot::ViewAction};
use fab_elec_view::trace::{columns::Columns, cursor::Cursors, view::ArraySource};
use std::sync::Arc;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-3)
}

fn window_eq(got: (f64, f64), want: (f64, f64), what: &str) {
    assert!(close(got.0, want.0) && close(got.1, want.1), "{what}: window {got:?}, expected {want:?}");
}

/// Every lane shares one window: it rolls with the head (filling from the left before the head
/// has crossed a window), centres a trigger event, zooms in 1-2-5 steps about the pointer (a
/// playing, rolling window keeps its head at the right edge), pans, and is a function of the anchor
/// alone, so a paused head freezes it.
#[test]
fn one_timebase_rolls_centres_the_trigger_zooms_about_the_pointer_and_pans() {
    assert_eq!([step_125(1e-3, 1), step_125(1e-3, 2), step_125(1e-3, 3), step_125(1e-3, -1), step_125(20e-3, -2)],
        [2e-3, 5e-3, 1e-2, 5e-4, 5e-3], "time per division moves in 1-2-5 steps");
    let mut tb = Timebase::new(20e-3);
    let run = Anchor { head: 0.5, start: 0.0, trigger: None };
    window_eq(tb.window(&run), (0.3, 0.5), "a rolling window ends at the head");
    window_eq(tb.window(&Anchor { head: 0.05, ..run }), (0.0, 0.2), "before the head crosses a window, the record fills it from the left");
    window_eq(tb.window(&Anchor { trigger: Some(0.42), ..run }), (0.32, 0.52), "a trigger event sits at the window's middle");
    window_eq(tb.window(&Anchor { head: 0.669, start: 0.537, trigger: None }), (0.469, 0.669),
        "a record that dropped its oldest stretch still ends the window at the head");
    assert_eq!(tb.window(&run), tb.window(&run), "the same anchor (a paused head) gives the same window");

    let mut rolling = tb;
    rolling.zoom(&run, -1, 0.25, true);
    window_eq(rolling.window(&run), (0.4, 0.5), "zooming a playing, rolling window keeps the head at its end");

    tb.zoom(&run, -1, 0.25, false);
    assert!(close(tb.per_div, 10e-3), "zooming in one step halves 20 ms/div");
    window_eq(tb.window(&run), (0.325, 0.425), "a paused zoom keeps the instant under the pointer (0.35 s at a quarter) in place");
    let later = Anchor { head: 0.6, ..run };
    let (a, b) = (tb.window(&run), tb.window(&later));
    assert!(close(b.0 - a.0, 0.1), "a zoomed window still follows the head: {a:?} then {b:?}");

    tb.pan(-0.05);
    window_eq(tb.window(&run), (0.275, 0.375), "a pan moves the shared window");
    tb.lag = 0.0;
    window_eq(tb.window(&run), (0.4, 0.5), "following again ends the window at the head");
}

fn runs_of(t: &[f64], y: &[f32], t0: f64, t1: f64, cols: u32, th: Thresholds) -> Vec<Run> {
    let mut runs = Vec::new();
    logic_runs(&Columns::reduce(t, y, t0, t1, cols), th, &mut runs);
    runs
}

/// Sparse samples split exactly where a straight piece crosses a threshold, so an edge sits at its
/// interpolated place and a level between the thresholds (a slow edge, a glitch, a floating
/// input) is its own run; a pixel holding whole transitions is busy; before the first sample and
/// after the last there is nothing.
#[test]
fn logic_runs_place_edges_and_show_the_undefined_band() {
    let th = Thresholds::of_rail(5.0);
    assert_eq!((th.low, th.high), (1.5, 3.5), "CMOS thresholds at 30 % and 70 % of the rail");
    let t: Vec<f64> = (0..=10).map(f64::from).collect();
    let y = [0.0, 0.0, 0.0, 5.0, 5.0, 5.0, 2.5, 2.5, 0.0, 0.0, 0.0];
    let runs = runs_of(&t, &y, 0.0, 10.0, 10, th);
    let want = [(Level::Low, 0.0, 2.3), (Level::Between, 2.3, 2.7), (Level::High, 2.7, 5.6), (Level::Between, 5.6, 7.4), (Level::Low, 7.4, 10.0)];
    assert_eq!(runs.len(), want.len(), "low, edge, high, a slow fall through the band, low: {runs:?}");
    for (run, (level, x0, x1)) in runs.iter().zip(want) {
        assert!(run.level == level && (f64::from(run.x0) - x0).abs() < 1e-4 && (f64::from(run.x1) - x1).abs() < 1e-4,
            "{run:?}, expected {level:?} from {x0} to {x1}");
    }

    let late = runs_of(&t, &y, -5.0, 5.0, 10, th);
    assert!(late.first().is_some_and(|r| r.x0 == 5.0), "before the record's first sample the lane is empty: {late:?}");

    let dense_t: Vec<f64> = (0..1000).map(|i| f64::from(i) * 1e-3).collect();
    let dense_y: Vec<f32> = (0..1000).map(|i| if (i / 3) % 2 == 0 { 0.0 } else { 5.0 }).collect();
    let busy = runs_of(&dense_t, &dense_y, 0.0, 1.0, 10, th);
    assert!(busy.iter().all(|r| r.level == Level::Busy), "a pixel of whole transitions is busy: {busy:?}");
}

/// A square wave of 2000 periods, 100 samples a period, each edge between two samples: the
/// newest crossing of either slope is found through the source's columns at its interpolated
/// time, within a bounded range. A playing run shows an event once what follows it has arrived
/// (half a window, or half a record shorter than that). Without a new event, normal mode holds its
/// capture and auto holds it a few windows, then rolls.
#[test]
fn the_trigger_finds_the_newest_edge_through_any_source() {
    let n = 200_000;
    let dt = 1e-6;
    let t: Vec<f64> = (0..n).map(|i| i as f64 * dt).collect();
    let y: Vec<f32> = (0..n).map(|i| if (i / 50) % 2 == 0 { 0.0 } else { 5.0 }).collect();
    let source = ArraySource { t: Arc::from(t), ys: vec![Arc::from(y)] };
    let end = (n - 1) as f64 * dt;
    // Rising edges between samples 100k + 49 and 100k + 50 (k odd counts from the first at 49..50).
    let rise = last_crossing(&source, 0, 0.0, end, 2.5, Slope::Rising).expect("a rising edge");
    let want_rise = (199_949.0 + 0.5) * dt;
    assert!(close(rise, want_rise), "the newest rising edge at its interpolated time: {rise} vs {want_rise}");
    let fall = last_crossing(&source, 0, 0.0, end, 2.5, Slope::Falling).expect("a falling edge");
    assert!(close(fall, (199_899.0 + 0.5) * dt), "the newest falling edge: {fall}");
    let either = last_crossing(&source, 0, 0.0, end, 2.5, Slope::Either).expect("an edge");
    assert!(close(either, want_rise), "either slope takes the newest edge");
    let bounded = last_crossing(&source, 0, 0.0, 150e-3, 2.5, Slope::Rising).expect("an earlier edge");
    assert!(close(bounded, (149_949.0 + 0.5) * dt), "a bounded search takes the newest edge before its end: {bounded}");
    assert_eq!(last_crossing(&source, 0, 0.0, end, 9.0, Slope::Rising), None, "no crossing of a level above the signal");

    let anchor = Anchor { head: end, start: 0.0, trigger: None };
    let span = 1e-3;
    let shown = find_trigger(&source, 0, &anchor, span, 2.5, Slope::Rising, TrigMode::Auto, true).expect("auto finds an event");
    assert!(shown <= end - 0.5 * span && shown > end - 4.0 * span, "a playing run takes an event whose window has arrived: {shown}");
    assert_eq!(find_trigger(&source, 0, &anchor, span, 9.0, Slope::Rising, TrigMode::Auto, true), None, "no event in the record");
    let paused = find_trigger(&source, 0, &anchor, span, 2.5, Slope::Rising, TrigMode::Auto, false);
    assert!(paused.is_some_and(|t| close(t, want_rise)), "a paused run takes the newest event: {paused:?}");
    let short = Anchor { head: end, start: end - 1e-3, trigger: None };
    let late = find_trigger(&source, 0, &short, 0.2, 2.5, Slope::Rising, TrigMode::Auto, true).expect("an event in a short record");
    assert!(late <= end - 0.5e-3 && late >= end - 1e-3, "a record shorter than a window shows its event with half the record after it: {late}");

    let (capture, head) = (Some(("first", 0.1)), 0.6);
    assert_eq!(hold(capture, Some(("newest", 0.45)), TrigMode::Auto, head, 0.1), Some(("newest", 0.45)), "a new event takes the newest record");
    assert_eq!(hold(capture, None, TrigMode::Normal, head, 0.1), capture, "normal holds its capture without a new event");
    assert_eq!(hold(capture, None, TrigMode::Auto, 0.1 + AUTO_SPANS * 0.1, 0.1), capture, "auto holds it a few windows");
    assert_eq!(hold(capture, None, TrigMode::Auto, head, 0.1), None, "then rolls");
}

/// ledavr's chase probes the eight LED cathodes on the MCU's pins and the button on PD2: all draw
/// as logic against the MCU's 5 V rail; the USB supply draws as a waveform.
#[test]
fn ledavr_probes_draw_as_logic_and_its_supply_as_a_waveform() {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("ledavr evaluates");
    let circuit = &evaluated.design.units.iter().find(|unit| unit.circuit.net("LED0").is_some()).expect("ledavr's unit").circuit;
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    assert_eq!(chase.probes.len(), 9, "eight LEDs and the button");
    for probe in &chase.probes {
        let net = circuit.net(&probe.of.target).expect("a probed net");
        let facts = net_facts(circuit, net.id);
        assert_eq!(facts.kind(), LaneKind::Digital, "{}: a pin of the MCU is on it", probe.label);
        assert_eq!(facts.rail, Some(5.0), "{}: its logic runs off the 5 V USB rail", probe.label);
    }
    let vbus = circuit.net("VBUS").expect("the USB supply");
    let facts = net_facts(circuit, vbus.id);
    assert_eq!((facts.kind, facts.kind()), (NetKind::Power, LaneKind::Analog), "the supply draws as a waveform");

}

/// drum16's kick rings about the 2.5 V bias of an op-amp's output: an analog part, so its nets
/// draw as waveforms from the start, and its ringing, or its output rising to the bias at power-up,
/// never switches the lane to logic (no D chip pressed).
#[test]
fn an_op_amp_output_draws_as_a_waveform_from_the_start() {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16's documents");
    let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs()).expect("drum16 evaluates");
    let kick = evaluated.benches.iter().find(|bench| bench.name == "kick").expect("drum16's kick bench");
    let circuit = &evaluated.design.unit(kick.scope_id()).expect("the kick bench's unit").circuit;
    let out = circuit.net("OUT").expect("the kick's output");
    let driver = circuit.comps.iter().find(|comp| comp.conns.iter().any(|&(pin, net)| net == out.id
        && comp.part.pins.get(pin.0 as usize).is_some_and(|p| p.name == "OUT"))).expect("an op-amp drives OUT");
    assert_eq!(driver.part.kind, fab_elec_parts::PartKind::Analog, "the std op-amp is an analog part");
    let facts = net_facts(circuit, out.id);
    assert!(!facts.logic && facts.kind() == LaneKind::Analog, "the op-amp's output draws as a waveform: {facts:?}");

    // a net with no logic pin draws as logic only once it pulses rail to rail
    let swing = |y: &dyn Fn(f64) -> f32| {
        let t: Vec<f64> = (0..2000).map(|i| i as f64 * 1e-3).collect();
        let y: Vec<f32> = t.iter().map(|&t| y(t)).collect();
        swings_like_logic(&Columns::reduce(&t, &y, 0.0, 2.0, 200))
    };
    assert!(swing(&|t| if (t * 10.0) as u32 % 2 == 0 { 0.0 } else { 5.0 }), "a 5 Hz square wave swings like logic");
    assert!(!swing(&|t| if t < 0.1 { 0.0 } else { 2.5 + 0.05 * (t * 300.0).sin() as f32 }), "an output stepping to its mid-supply bias at power-up does not");
    assert!(!swing(&|t| 2.5 + 1.2 * (-3.0 * t).exp() as f32 * (t * 400.0).sin() as f32), "a decaying sine about its bias does not");
}

/// A recorded window as the view reduces it: each column's least and greatest value in mV, fed
/// back as two samples a column.
fn recorded(cols: &[(i16, i16)]) -> Columns {
    let t: Vec<f64> = (0..cols.len()).flat_map(|i| [i as f64 + 0.25, i as f64 + 0.75]).collect();
    let y: Vec<f32> = cols.iter().flat_map(|&(lo, hi)| [f32::from(lo) / 1e3, f32::from(hi) / 1e3]).collect();
    Columns::reduce(&t, &y, 0.0, cols.len() as f64, cols.len() as u32)
}

/// drum16's line out (`LINE`, through its output capacitor) rests within tens of mV of ground
/// between hits, and a hit swings it as far below ground as above. A window whose newest columns
/// catch a hit starting (a pulse inside one column, or two) never makes the lane logic: its peak is
/// passed, not held, and the line rings on below ground. The kick's trigger on the same board, an
/// MCU pin into the voice, is logic at a pulse and zoomed out over the bar. All recorded from the
/// board bench as the view reduced them (its first strike, 0.626 s after power-up).
#[test]
fn a_drum_hit_on_a_line_out_is_no_logic_and_its_trigger_is() {
    // 0.53 to 0.63 s over 50 columns, and 0.43 to 0.63 s over 150
    const LINE_50: [(i16, i16); 50] = [
        (15, 31), (25, 29), (11, 31), (-7, 18), (-17, -2), (-26, -13), (-20, -12), (-13, 5), (1, 20),
        (15, 25), (17, 30), (7, 22), (-4, 11), (-19, -4), (-23, -11), (-19, -4), (-9, 4), (0, 16),
        (11, 26), (14, 25), (3, 18), (-10, 8), (-14, -4), (-21, -8), (-16, -5), (-8, 6), (4, 19), (10, 23),
        (9, 24), (5, 15), (-5, 6), (-16, -4), (-18, -6), (-16, 1), (-4, 7), (2, 16), (8, 21), (10, 21),
        (3, 13), (-6, 4), (-14, -2), (-16, -3), (-10, -2), (-7, 8), (2, 16), (11, 16), (7, 19), (-3, 10),
        (-185, 1436), (-485, 735)
    ];
    const LINE_150: [(i16, i16); 150] = [
        (-7, 26), (-29, 3), (-47, -28), (-70, -42), (-57, -44), (-71, -30), (-32, -7), (-7, 19), (9, 41),
        (38, 57), (47, 61), (44, 54), (22, 49), (5, 27), (-18, 5), (-43, -17), (-48, -35), (-52, -41),
        (-49, -29), (-32, -11), (-16, 11), (9, 30), (30, 46), (43, 49), (40, 53), (29, 44), (5, 29),
        (-12, 15), (-26, -9), (-42, -26), (-40, -35), (-39, -28), (-35, -10), (-16, 3), (3, 22), (20, 35),
        (33, 45), (35, 47), (24, 39), (14, 33), (-4, 15), (-21, -3), (-30, -17), (-38, -28), (-35, -28),
        (-30, -17), (-19, -2), (-4, 16), (14, 28), (24, 41), (30, 38), (28, 40), (16, 30), (1, 16),
        (-13, 2), (-23, -12), (-33, -21), (-33, -22), (-31, -18), (-19, -2), (-6, 9), (8, 26), (19, 30),
        (26, 33), (26, 37), (17, 29), (5, 22), (-7, 9), (-20, -7), (-23, -13), (-28, -21), (-27, -15),
        (-26, -7), (-10, 5), (4, 16), (15, 25), (24, 31), (25, 29), (18, 31), (7, 20), (-7, 9), (-14, -2),
        (-25, -12), (-26, -13), (-20, -16), (-19, -7), (-12, 5), (1, 11), (5, 20), (18, 25), (18, 30),
        (17, 29), (7, 19), (0, 11), (-9, 3), (-19, -7), (-19, -14), (-23, -11), (-19, -4), (-9, -1),
        (-1, 8), (3, 16), (11, 24), (15, 26), (14, 25), (6, 18), (3, 15), (-10, 5), (-12, -4), (-20, -8),
        (-21, -9), (-16, -5), (-10, -2), (-7, 6), (4, 13), (8, 19), (16, 23), (12, 24), (9, 21), (5, 13),
        (-5, 6), (-13, -1), (-16, -4), (-18, -6), (-16, -4), (-14, 1), (-4, 3), (2, 15), (4, 16), (8, 21),
        (10, 21), (10, 16), (4, 13), (0, 7), (-6, 2), (-14, -2), (-16, -5), (-16, -3), (-10, -4), (-7, 4),
        (0, 8), (2, 16), (11, 15), (12, 16), (7, 19), (7, 13), (-3, 8), (-185, 1436), (15, 1390),
        (-485, 275)
    ];
    // the kick's trigger: 0.62 to 0.63 s over 60 columns (a strike's 1 ms pulse), and 0 to 1 s over
    // 50 columns with 0.66 s recorded (the bar's first strikes)
    const KICK_PULSE: [(i16, i16); 60] = [
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108),
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108),
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108),
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 108),
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 108), (108, 3581), (3581, 3589),
        (3589, 3590), (3590, 3590), (3590, 3590), (3590, 3590), (103, 3590), (103, 108), (108, 108),
        (108, 108), (108, 108), (108, 108), (108, 108), (108, 109), (109, 109), (109, 109), (109, 109),
        (109, 109), (109, 109), (109, 109), (109, 109), (109, 109), (109, 109)
    ];
    const KICK_BAR: [(i16, i16); 33] = [
        (-185, 2477), (105, 110), (106, 109), (106, 109), (106, 109), (106, 109), (104, 4887), (106, 109),
        (106, 109), (106, 109), (107, 109), (107, 108), (107, 108), (107, 108), (107, 108), (107, 108),
        (107, 108), (107, 108), (103, 112), (107, 108), (107, 108), (107, 108), (107, 108), (107, 108),
        (107, 108), (107, 108), (107, 108), (107, 108), (107, 108), (107, 108), (108, 108), (103, 3590),
        (106, 109)
    ];
    assert!(!swings_like_logic(&recorded(&LINE_50)), "a hit starting in the newest column of a quiet line out is no logic");
    assert!(!swings_like_logic(&recorded(&LINE_150)), "nor one starting over its newest two columns");
    assert!(swings_like_logic(&recorded(&KICK_PULSE)), "a trigger pulse held between its levels is logic");
    assert!(swings_like_logic(&recorded(&KICK_BAR)), "trigger pulses inside columns, the line low after each, are logic");
}

/// A wire clicked probes its net into the run bench once, named by the net, as one undo step;
/// the probe survives evaluation, and a net probed already (a second click, or an LED the bench
/// probes under its own name) only shows its lane.
#[test]
fn a_wire_click_probes_its_net_once_named_by_the_net() {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let libs = fab_elec_lang::std_libs();
    let evaluated = fab_elec_lang::evaluate(&sources, &libs).expect("ledavr evaluates");
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    assert_eq!(click_probe_op("chase", &chase.probes, "LED0"), None, "an LED the bench probes only shows its lane");
    let Some(ViewAction::Ops { ops, label, .. }) = click_probe_op("chase", &chase.probes, "VBUS") else {
        panic!("a click on the supply probes it");
    };
    assert_eq!(label, "Probe VBUS", "one undo step named after the net");
    assert_eq!(ops, vec![DesignOp::Bench { bench: "chase".into(), op: BenchOp::AddProbe { of: ProbeOf::v("VBUS".to_owned()), label: "VBUS".into() } }],
        "a voltage probe of the net, named by the net");
    let change = fab_elec_lang::splice(&sources, &evaluated.map, &evaluated.design, &ops).expect("the probe splices");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the probe's edit applies");
    let again = fab_elec_lang::evaluate(&edited, &libs).expect("ledavr evaluates with the probe");
    let chase = again.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    assert!(chase.probes.iter().any(|probe| probe.of.target == "VBUS" && probe.label == "VBUS"), "the probe evaluates, named VBUS: {:?}", chase.probes);
    assert_eq!(chase.probes.len(), 10, "one probe more");
    assert_eq!(click_probe_op("chase", &chase.probes, "VBUS"), None, "a second click only shows its lane");
}

/// A lane's trigger chip moves the trigger there rising (keeping its mode), then steps falling,
/// either edge, off. Plain clicks set A, then B, then move the nearer; Shift sets B. A cursor snaps
/// to the nearest edge of any lane within reach, and the readout shows each instant to a hundredth
/// of a division with the difference and its reciprocal.
#[test]
fn the_trigger_chip_cycles_and_cursors_place_snap_and_read() {
    let normal = Some(Trigger { net: "LED0".into(), slope: Slope::Falling, mode: TrigMode::Normal });
    let moved = next_trigger(normal, "BTN").expect("a trigger");
    assert_eq!((moved.net.as_str(), moved.slope, moved.mode), ("BTN", Slope::Rising, TrigMode::Normal), "another lane's chip moves it there, rising");
    let mut steps = Vec::new();
    let mut trigger = Some(moved);
    for _ in 0..3 {
        trigger = next_trigger(trigger, "BTN");
        steps.push(trigger.as_ref().map(|t| t.slope));
    }
    assert_eq!(steps, [Some(Slope::Falling), Some(Slope::Either), None], "its own chip steps falling, either, off");

    let mut cursors = Cursors::default();
    assert_eq!(cursor_for_click(&cursors, 1.0, false), Which::A, "the first click sets A");
    cursors.a = Some(1.0);
    assert_eq!(cursor_for_click(&cursors, 3.0, false), Which::B, "the next sets B");
    cursors.b = Some(3.0);
    assert_eq!((cursor_for_click(&cursors, 1.5, false), cursor_for_click(&cursors, 2.6, false)), (Which::A, Which::B), "then the nearer moves");
    assert_eq!(cursor_for_click(&Cursors::default(), 1.0, true), Which::B, "Shift sets B");

    let th = Thresholds::of_rail(5.0);
    let t: Vec<f64> = (0..=10).map(f64::from).collect();
    let square = runs_of(&t, &[0.0, 0.0, 5.0, 5.0, 5.0, 0.0, 0.0, 0.0, 5.0, 5.0, 5.0], 0.0, 10.0, 10, th);
    let other = runs_of(&t, &[5.0, 5.0, 5.0, 5.0, 5.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.0, 10.0, 10, th);
    let edge = nearest_edge([&square[..], &other[..]], 5.9, 1.0).expect("an edge within a column");
    assert!((edge - 5.7).abs() < 1e-4, "the second lane's fall reaching 0 at 5.7 is nearer than its entry into the band at 5.3 or the square's fall at 4.7: {edge}");
    assert_eq!(nearest_edge([&square[..]], 0.5, 0.5), None, "no edge within reach");

    let mut placed = Cursors { a: Some(35.7925), b: Some(35.8125) };
    follow_trigger(&mut placed, Some(35.80), Some(35.81));
    assert!(close(placed.a.unwrap(), 35.8025) && close(placed.b.unwrap(), 35.8225), "a new capture 10 ms later moves both cursors with it: {placed:?}");
    follow_trigger(&mut placed, None, Some(35.9));
    assert!(close(placed.a.unwrap(), 35.8025), "a trigger just set leaves the cursors where they are");
    let read = cursor_readout(&Cursors { a: Some(35.7925), b: Some(35.8125) }, 20e-3);
    assert_eq!(read, "A 35.7925 s   B 35.8125 s   Δt 20.000 ms   1/Δt 50.000 Hz", "instants to a hundredth of 20 ms/div");
}

/// A lane dragged by its name lands in the gap the insertion line shows; the D/A chip switches a
/// lane away from its default and back; a rename writes the probe's label in the bench source,
/// where a reload (evaluating the edited text) finds it, and an empty or unchanged name writes
/// nothing.
#[test]
fn lanes_reorder_switch_kind_and_rename_into_the_bench_source() {
    let mut lanes = vec!["D1", "D2", "D3", "BTN"];
    move_to_gap(&mut lanes, 3, 0);
    assert_eq!(lanes, ["BTN", "D1", "D2", "D3"], "the last lane dropped above the first");
    move_to_gap(&mut lanes, 0, 2);
    assert_eq!(lanes, ["D1", "BTN", "D2", "D3"], "the first dropped in the gap after the second");
    move_to_gap(&mut lanes, 1, 4);
    assert_eq!(lanes, ["D1", "D2", "D3", "BTN"], "dropped after the last");
    move_to_gap(&mut lanes, 2, 2);
    assert_eq!(lanes, ["D1", "D2", "D3", "BTN"], "dropped where it was");

    assert_eq!(next_kind(LaneKind::Digital, None), Some(LaneKind::Analog), "a logic lane drawn as a waveform");
    assert_eq!(next_kind(LaneKind::Analog, None), Some(LaneKind::Digital), "a waveform drawn as logic");
    assert_eq!(next_kind(LaneKind::Digital, Some(LaneKind::Analog)), None, "and back to its default");

    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr's documents");
    let libs = fab_elec_lang::std_libs();
    let evaluated = fab_elec_lang::evaluate(&sources, &libs).expect("ledavr evaluates");
    let chase = evaluated.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    let button = chase.probes.iter().find(|probe| probe.of.target == "BTN").expect("the button's probe");
    assert_eq!(rename_probe_op("chase", &button.of, &button.label, "  ", None), None, "an empty name writes nothing");
    assert_eq!(rename_probe_op("chase", &button.of, &button.label, &format!(" {} ", button.label), None), None, "an unchanged name writes nothing");
    let keep = rename_keeps(&evaluated.design, button, " SW1 press ");
    assert_eq!(keep, None, "a name that placed nothing pins nothing: the flag goes where the new name says");
    let Some(ViewAction::Ops { ops, label, .. }) = rename_probe_op("chase", &button.of, &button.label, " SW1 press ", keep.as_deref()) else {
        panic!("a new name renames the probe");
    };
    assert_eq!(label, format!("Rename probe {} to SW1 press", button.label), "one undo step named for the rename");
    let change = fab_elec_lang::splice(&sources, &evaluated.map, &evaluated.design, &ops).expect("the rename splices");
    let mut edited = sources.clone();
    edited.apply(&change.edits).expect("the rename's edit applies");
    let reloaded = fab_elec_lang::evaluate(&edited, &libs).expect("ledavr evaluates from the edited text");
    let chase = reloaded.benches.iter().find(|bench| bench.name == "chase").expect("ledavr's chase bench");
    let names: Vec<&str> = chase.probes.iter().map(|probe| probe.label.as_str()).collect();
    assert!(names.contains(&"SW1 press") && names.len() == 9, "the reload finds the new name, the other probes as they were: {names:?}");
}

/// The level `runs` draw at column `x`, None where they draw nothing.
fn run_at(runs: &[Run], x: f64) -> Option<Level> {
    runs.iter().find(|r| f64::from(r.x0) <= x && x < f64::from(r.x1)).map(|r| r.level)
}

/// A digital lane draws a pad's logic trace (its edges, the whole run) by columns: a column with
/// one edge splits at its exact place, with more it is busy, an edgeless stretch is one run; the
/// trigger and the cursors take its edges' exact times, a normal trigger from its whole history.
#[test]
fn a_logic_trace_draws_by_columns_and_gives_exact_edges() {
    // a 1 kHz PWM, high a quarter period, for 5 s (sealed chunks and an open tail), a glitch (two
    // edges in 40 ns) at 1.5006 s
    let (period, high, periods) = (1e-3, 0.25e-3, 5000);
    let mut store = TraceStore::new(1, &[]);
    let mut closed = Vec::new();
    let mut entries = vec![LogicEdge { t: 0.0, high: false }];
    for k in 0..periods {
        let t = 1e-4 + k as f64 * period;
        entries.extend([LogicEdge { t, high: true }, LogicEdge { t: t + high, high: false }]);
        if k == 1500 {
            entries.extend([LogicEdge { t: t + 0.5 * period, high: true }, LogicEdge { t: t + 0.5 * period + 40e-9, high: false }]);
        }
    }
    for &e in &entries {
        store.push_logic(SignalId(2), e, &mut closed);
    }
    let trace: &LogicTrace = store.logic(SignalId(2)).expect("the trace");
    assert!(!trace.chunks.is_empty() && !trace.open.is_empty(), "the trace has sealed chunks and an open tail");
    let head = 5.0;
    let upto = |t: f64| entries.partition_point(|e| e.t <= t);
    let level = |t: f64| upto(t).checked_sub(1).map(|i| if entries[i].high { Level::High } else { Level::Low });
    let mut runs = Vec::new();

    // every column of a window draws as a count of the entries in it says: none, its level; one,
    // the level before it then after it; more, busy
    for (t0, t1, cols) in [(1.49, 1.51, 37u32), (0.0, head, 400), (0.0, head, 3000), (2.0, 2.2, 1000), (4.09, 4.11, 211), (0.7, 0.703, 1000)] {
        edge_runs(trace, t0, t1, cols, head, &mut runs);
        let per = (t1 - t0) / f64::from(cols);
        let x = |t: f64| (t - t0) / per;
        for c in 0..cols {
            let (a, b, cf) = (t0 + f64::from(c) * per, t0 + f64::from(c + 1) * per, f64::from(c));
            let count = upto(b) - upto(a);
            match count {
                0 => assert_eq!(run_at(&runs, cf + 0.5), level(a), "edgeless column {c} of {t0}..{t1} draws its level"),
                1 => {
                    // (a side thinner than the runs' f32 places is not looked at)
                    let at = x(entries[upto(a)].t);
                    let sides = [(at - cf > 1e-3).then(|| (0.5 * (cf + at), level(a))), (cf + 1.0 - at > 1e-3).then(|| (0.5 * (at + cf + 1.0), level(b)))];
                    for (mid, want) in sides.into_iter().flatten() {
                        assert_eq!(run_at(&runs, mid), want, "column {c} of {t0}..{t1} splits at its edge");
                    }
                    assert!(runs.iter().any(|r| (f64::from(r.x0) - at).abs() < 1e-3), "a run starts at the edge's exact place in column {c} of {t0}..{t1}");
                }
                _ => assert_eq!(run_at(&runs, cf + 0.5), Some(Level::Busy), "column {c} of {t0}..{t1} with {count} edges is busy"),
            }
        }
    }
    edge_runs(trace, 0.0, head, 400, head, &mut runs);
    assert_eq!(runs, [Run { level: Level::Busy, x0: 0.0, x1: 400.0 }], "the whole run in 400 columns is one busy run");
    edge_runs(trace, 0.7, 0.703, 1000, head, &mut runs);
    assert_eq!(runs.len(), 7, "three periods zoomed in: one run per level between six edges");

    // nothing before the first entry or after the head
    edge_runs(trace, -5.0, 15.0, 400, head, &mut runs);
    assert_eq!((runs[0].x0, runs.last().unwrap().x1), (100.0, 200.0), "the runs span the first entry to the head");

    // exact edges: the trigger's, a normal one's from the whole history, and the cursors'
    let rise = 1e-4 + 1234.0 * period;
    assert_eq!(last_edge(trace, 0.0, rise + 0.5 * period, Slope::Rising), Some(rise), "the newest rising edge, exact");
    assert_eq!(last_edge(trace, 0.0, rise + 0.5 * period, Slope::Falling), Some(rise + high), "the newest falling edge, exact");
    assert_eq!(last_edge(trace, rise + 0.3 * period, rise + 0.9 * period, Slope::Either), None, "no edge in the span");
    assert_eq!(last_edge(trace, -1.0, 5e-5, Slope::Either), None, "the first entry is no edge");
    let anchor = Anchor { head, start: 4.99, trigger: None };
    let found = find_logic_trigger(trace, &anchor, 1.0, Slope::Rising, TrigMode::Normal, false);
    assert_eq!(found, Some(1e-4 + 4999.0 * period), "a normal trigger searches the logic's whole history");
    let paused_far = Anchor { head: 0.5, start: 0.49, trigger: None };
    assert_eq!(find_logic_trigger(trace, &paused_far, 1e-3, Slope::Falling, TrigMode::Normal, false), Some(1e-4 + 499.0 * period + high), "past the samples' start too");
    assert_eq!(nearest_logic_edge(trace, rise + 1e-6, 1e-5), Some(rise), "a cursor snaps to the exact edge");
    assert_eq!(nearest_logic_edge(trace, rise + 0.5 * period, 1e-5), None, "only within reach");
}

/// A logic trace past its in-memory bound draws from its spilled chunks' summaries where they are
/// dense at the zoom (the same picture as the whole trace), busy over a spilled chunk the window
/// shows in detail, which the window then asks for (`spilled_in`), and once it is back exactly as
/// the whole trace draws.
#[test]
fn a_spilled_logic_trace_draws_from_its_summaries_then_exactly_once_back() {
    let (id, n) = (SignalId(2), LOGIC_CHUNK);
    // a 10 kHz clock past the bound, quiet for 20 ms in its second chunk
    let quiet = n + n / 2;
    let edge = |i: usize| LogicEdge { t: i as f64 * 50e-6 + if i > quiet { 20e-3 } else { 0.0 }, high: i % 2 == 1 };
    let total = LOGIC_KEEP + 2 * n + 5;
    let (mut whole, mut closed) = (TraceStore::new(1, &[]), Vec::new());
    for i in 0..total {
        whole.push_logic(id, edge(i), &mut closed);
    }
    let (mut store, mut spill, mut pages) = (whole.clone(), LogicSpill::new(), Vec::new());
    store.spill_logic(&mut spill, &[], &mut pages);
    let all = whole.logic(id).expect("the whole trace");
    assert!(store.logic(id).is_some_and(|t| t.spilled >= 2 && t.chunks[1].is_none()), "the trace's oldest chunks spilled");
    let head = edge(total - 1).t;
    let runs = |trace: &LogicTrace, t0: f64, t1: f64, cols: u32| {
        let mut out = Vec::new();
        edge_runs(trace, t0, t1, cols, head, &mut out);
        out
    };
    let part = store.logic(id).expect("the spilled trace");
    assert_eq!(runs(part, 0.0, head, 800), runs(all, 0.0, head, 800), "the overview draws the same: every chunk is dense at a column");

    let (t0, t1, cols) = (edge(quiet).t - 1e-3, edge(quiet).t + 21e-3, 220);
    assert_eq!(runs(part, t0, t1, cols), [Run { level: Level::Busy, x0: 0.0, x1: cols as f32 }], "a spilled chunk the window shows in detail draws busy");
    let wanted: Vec<f64> = part.spilled_in(t0, t1, 0.5 * (t1 - t0) / f64::from(cols)).collect();
    assert_eq!(wanted, [edge(n).t], "the window asks for that chunk");
    store.load_logic(&mut spill, &[(id, wanted[0])], &mut pages);
    let part = store.logic(id).expect("the spilled trace");
    let exact = runs(all, t0, t1, cols);
    assert!(exact.iter().any(|run| run.level == Level::Low || run.level == Level::High), "the quiet stretch draws a level");
    assert_eq!(runs(part, t0, t1, cols), exact, "back, it draws as the whole trace does");
}
