//! How fast the MCU emulators run, for the emulator speed campaign (EMUFAST). Release builds, run
//! natively and alone on one stated box; MIPS are emulated instructions retired per wall second.
//!
//! - `s3 [secs] [plain|marks|both] [decoded|ref]`: the S3 emulator alone, in its default tier
//!   (decoded) or the reference interpreter: a plain compute loop in `setup`, built
//!   through the firmware path, on `S3Machine` with an empty netlist, run in 0.2 ms slices (the
//!   co-simulation's stretch) and marked at each barrier, as `CoMachine` would. `marks` passes the
//!   image's step map as the co-simulation does. The loop is rerun from a fresh copy of the
//!   machine until `secs` of wall time have passed (default 3), so a profiler can attach.
//! - `sand [window]`: the handheld's Sand on the live circuit, as the fixture test runs it, timed
//!   over `window` simulated seconds of falling sand (default 0.3): wall per simulated second and
//!   the device's share of `LiveSim::advance`, in the default tier.
//!
//! `cargo run --release -p fab-elec-fixtures --example emuspeed -- s3 3 plain`

use fab_elec_bench::{lower, ModelLibs};
use fab_elec_fw::{compile, maker, s3_handoff, Built, Chip, FwInput};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_mcu_cosim::FwFrame;
use fab_elec_mcu_emit_s3::{wiring, BoardTime, Wiring};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Tier, Until};
use fab_elec_mcu_parts::screen::ScreenFrame;
use fab_elec_mcu_parts::{BoardDesc, Netlist};
use fab_elec_mcu_s3::S3Machine;
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::Sink;
use fab_elec_simhost::events::{EventLog, EventSource};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The S3 images' CPU clock (the runtime's PLL setting) and the chip's top clock.
const S3_HZ_NOW: f64 = fab_elec_mcu_emit_s3::CPU_HZ as f64;
const S3_HZ_PLL: f64 = 240e6;
/// `CoMachine`'s stretch for the S3: 0.2 ms on the 480 MHz tick grid.
const STRETCH: u64 = 96_000;

const LOOP: &str = r#"
let n = 1000000
var sum: i32 = 0
fn setup() {
    for i in 0..n { sum = (sum + i) & 65535 }
}
"#;

fn s3_loop(secs: f64, with_marks: bool, tier: Tier) {
    let handoff = s3_handoff(0);
    let time = BoardTime { rc_slow_hz: handoff.time.rc_slow_hz, wdt_delay_sel: handoff.time.wdt_delay_sel };
    let none = Wiring { i2c: None, motion: None, panel: None, touch: None, audio: None };
    let fw = fab_elec_mcu_emit_s3::firmware(LOOP, &[("n", 2_000_000.0)], &none, time, None, &[])
        .unwrap_or_else(|d| panic!("the loop firmware builds: {d:?}"));
    let netlist = Netlist::new(BoardDesc { seed: 0x5300, parts: Vec::new(), nets: Vec::new() })
        .expect("an empty netlist");
    let mut fresh = S3Machine::with_board(Arc::new(fw.flash.clone()), &handoff, netlist).expect("the empty board binds");
    fresh.set_tier(tier);
    let marks = if with_marks { fw.debug.map.as_ref().map(|m| m.marks()).unwrap_or_default() } else { PcSet::default() };
    let (mut insns, mut barriers, mut runs, mut rounds) = (0u64, 0u64, 0u64, 0u64);
    let (mut trace, mut drives) = (Vec::new(), Vec::new());
    let start = Instant::now();
    while rounds == 0 || start.elapsed().as_secs_f64() < secs {
        let mut m = fresh.clone();
        m.mark();
        loop {
            let to = m.now() + STRETCH;
            let ran = m.run(&Until { tick: to, marks: &marks, stops: &[], stops_after: 0, on_drive: true });
            runs += 1;
            m.take_drives(&mut drives);
            drives.clear();
            match ran {
                Ran::Barrier => {
                    m.take_trace(&mut trace);
                    trace.clear();
                    m.mark();
                    barriers += 1;
                }
                Ran::Tick | Ran::Drive { .. } => {}
                other => panic!("the loop firmware stopped: {other:?} at pc {:#x}", m.pc()),
            }
            // Setup has returned once the core sleeps in its idle wait.
            if m.cpu().sleeping() { break; }
        }
        insns += m.cpu().instret;
        rounds += 1;
    }
    let wall = start.elapsed().as_secs_f64();
    let mips = insns as f64 / wall / 1e6;
    println!("s3 {tier:?} {}: {rounds} rounds, {insns} instructions in {wall:.3} wall s: {mips:.1} MIPS, {:.1} ns per instruction; \
              {runs} runs, {barriers} barriers ({:.0} instructions per barrier); x real time {:.3} at {:.0} MHz, {:.3} at 240 MHz",
        if with_marks { "marks" } else { "plain" }, 1e9 / (mips * 1e6), insns as f64 / barriers.max(1) as f64,
        mips * 1e6 / S3_HZ_NOW, S3_HZ_NOW / 1e6, mips * 1e6 / S3_HZ_PLL);
    jit_totals();
}

/// The native units compiled so far, with their compile time by phase.
fn jit_totals() {
    let t = fab_elec_mcu_emu::jit::totals();
    if t.units + t.failed == 0 { return; }
    let us = |ns: u64, n: u64| ns as f64 / 1e3 / n.max(1) as f64;
    let all = t.build_ns + t.optimise_ns + t.emit_ns;
    let per = |n: u64| n as f64 / t.guest.max(1) as f64;
    println!("jit: {} units ({} failed), {} guest instructions ({} entries), {} RIR blocks ({:.2} per guest instruction; \
              {} after optimisation, {:.2}), {} code bytes; compile {:.1} ms in all, {:.2} ms per unit (longest {:.2} ms), \
              {:.2} us per guest instruction (build {:.2}, optimise {:.2}, emit {:.2})",
        t.units, t.failed, t.guest, t.entries, t.blocks, per(t.blocks), t.opt_blocks, per(t.opt_blocks), t.code_len,
        all as f64 / 1e6, us(all, t.units) / 1e3, t.max_ns as f64 / 1e6,
        us(all, t.guest), us(t.build_ns, t.guest), us(t.optimise_ns, t.guest), us(t.emit_ns, t.guest));
}

/// Sand's colours inside the glass, as the fixture test counts them.
const GLASS: [usize; 4] = [16, 0, 368, 448];
const SAND: [u32; 3] = [0xffff_cf6b, 0xffe7_a642, 0xffc6_7d29];

fn sand_pixels(frame: &ScreenFrame) -> usize {
    if frame.generation == 0 || !frame.on || frame.brightness == 0 { return 0; }
    let [x, y, w, h] = GLASS;
    let mut count = 0;
    for row in y..y + h {
        for pixel in &frame.bgra[row * frame.width + x..row * frame.width + x + w] {
            if SAND.contains(pixel) { count += 1; } else if *pixel != 0xff00_0000 { return 0; }
        }
    }
    count
}

struct Discard;
impl Sink for Discard {}

fn sand(window: f64) {
    const END: f64 = 3.0;
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a stock project");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("handheld evaluates: {d:?}"));
    let bench = evaluated.benches.iter().find(|b| b.name == "sand").expect("the sand bench");
    let lowered = lower(&evaluated.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let binding = &lowered.firmware[0];
    let pins = wiring(&binding.bus.board(0)).expect("handheld bus wiring resolves");
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1,
        text: include_str!("../handheld.fw.splash").into(),
        f_clk: binding.f_clk.expect("the main crystal"), params: binding.params.clone(),
        chip: Chip::S3, wiring: Some(pins), island: None, files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).unwrap_or_else(|d| panic!("Sand compiles: {d:?}"));
    assert!(matches!(built, Built::S3(_)), "Sand builds an S3 image");
    let (mut log, feed) = EventLog::<FwFrame>::new(END, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let readers = Arc::new(Mutex::new(Vec::new()));
    let received = readers.clone();
    let (make, _) = maker(&input, built, feed, false, Arc::new(move |set| {
        *received.lock().expect("screen sink lock") = set;
    }), None);
    let device = make(&lowered, 0).expect("member 0 builds");
    let (_, mut reader) = readers.lock().expect("screen reader lock").pop().expect("one screen reader");
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the operating point converges");
    sim.attach(Some(device));
    let mut advance = |sim: &mut LiveSim, until: f64| {
        log.drain();
        sim.advance((sim.t() + 1e-3).min(until), Some(Duration::from_millis(50)), &mut Discard)
            .unwrap_or_else(|d| panic!("the live circuit advances: {d}"));
        sim.values(0.0);
        log.drain();
    };
    let started = Instant::now();
    while sim.t() < END && sand_pixels(reader.latest()) < 81 * 16 {
        advance(&mut sim, END);
    }
    assert!(sand_pixels(reader.latest()) >= 81 * 16, "setup draws the starting heap by {:.3} s", sim.t());
    let setup_wall = started.elapsed().as_secs_f64();
    let setup_t = sim.t();
    let before = sim.stats().clone();
    let wall = Instant::now();
    let later = sim.t() + window;
    while sim.t() < later {
        advance(&mut sim, later);
    }
    let wall = wall.elapsed().as_secs_f64();
    let simulated = sim.t() - setup_t;
    let after = sim.stats();
    let step = (after.step_wall - before.step_wall).as_secs_f64();
    let co = (after.co_wall - before.co_wall).as_secs_f64();
    println!("sand: heap drawn at {setup_t:.3} sim s after {setup_wall:.3} wall s; falling: {:.3} wall s per sim s \
              ({wall:.3} wall s / {simulated:.3} sim s); advance {:.3} s per sim s, device {:.3} s per sim s ({:.0} %), \
              {} co runs",
        wall / simulated, step / simulated, co / simulated, 100.0 * co / step.max(1e-12), after.co_runs - before.co_runs);
    jit_totals();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        Some("s3") => {
            let secs = arg(1).map_or(3.0, |s| s.parse().expect("seconds"));
            let tier = match arg(3) { Some("ref") => Tier::Ref, Some("native") => Tier::Native, _ => Tier::Decoded };
            match arg(2) {
                Some("marks") => s3_loop(secs, true, tier),
                Some("both") => { s3_loop(secs, false, tier); s3_loop(secs, true, tier); }
                _ => s3_loop(secs, false, tier),
            }
        }
        Some("sand") => sand(arg(1).map_or(0.3, |s| s.parse().expect("seconds"))),
        _ => eprintln!("usage: emuspeed s3 [secs] [plain|marks|both] [decoded|ref|native] | sand [window]"),
    }
}
