//! How far the live bench is from real time on ledavr: the ATmega328P at 16 MHz running the
//! board's own firmware (the chase bench, `chase_ms` 20) against the lowered board, in both of the
//! firmware's modes: the chaser (the PWM outputs at duty 0 or 255, a few edges a second) and
//! breathe (the button pressed once: six PWM outputs ramping, two at 7.8 kHz and four at 490 Hz).
//!
//! - `avr`: the interpreter alone for one simulated second: instructions, cycles and wall time per
//!   instruction through `Cpu::step_until`, then the same second through `AvrMachine::run` with the
//!   image's step map marked and a return on every drive change (what the co-simulation adapter
//!   asks for), counting the drive changes per pin. `drumavr <secs>` runs drum16's firmware on
//!   the interpreter alone the same way: simulated MHz, awake and overall.
//! - `live`: the live worker's own loop (`LiveSim::advance` in 8 ms slices, values every 16 ms,
//!   trace deltas every 8 ms into a `TracePublisher`, the device's state in each publication)
//!   unpaced, measured over a window after the mode is reached, at Exact and at Fast resolutions,
//!   and Fast without the trace. Per run: the realtime factor, the wall split (device, analog,
//!   sinks, publication), the stepper's counts and, with the solver's profile on, device
//!   evaluation, factorisation and solve time.
//! - `take-read [secs]` measures the host recorder's gain from 1 to 16 kHz with no circuit
//!   (default 1 s). It feeds the recorder grid points, free steps (16 a period, so the chords lose
//!   under 0.013 dB to 16 kHz at 48 kHz), watched restarts, points on the grid's chords and
//!   half-period steps at 48, 64 and 96 kHz. It compares each trimmed 48 kHz take with the tones at those instants.
//! - `rc-read [secs]` drives a 4 kHz RC low-pass with the same tones at 48 and 64 kHz (default
//!   1 s), and Exact as board-acc's reference arm runs it (its tolerances, at 96 kHz). Exact's
//!   recorded output is compared with the continuous RC response, Fast's with the trapezoidal
//!   response on its grid. The bar is 0.05 dB in each third octave from 1 to 16 kHz.
//!
//! Run it native and alone on a quiet box:
//! `cargo run --release -p fab-elec-mcu-cosim --example realtime -- [avr|live|all] [window seconds] [chase|breathe|both]`.
//! `compare <window> <mode> <res us,..> 1` runs each case with the dense LU forced against the
//! default layout and prints how far the traces are apart (worst RMS and mean difference per
//! unknown, against its range). `partition` prints the islands' partition report for ledavr and
//! for drum16's benches; `held [shares]` the held nodes (`analysis::held`) on drum16's board live
//! at four instants, its voice benches, the tone box and ledavr. `board <secs> <res us,..> [profile]` runs drum16's whole board (its
//! `board` bench, the MCU and its registers included) from zero, with the stepper's counts and
//! accepted steps by length; `board <secs> <res us,..> attrib` adds row B0's attribution (where
//! the iterations go by step role and restart cause, a ghost bypass over the border view's blocks,
//! the rails' moves, the step ladder), twice; `board <secs> <res us,..> digest` folds every point
//! into a digest, as `ELEC_DIGEST` does. `board-acc <secs> <res us,..> [ref_reltol]` compares selected drum16
//! nets, LINE audio and trigger/metal rising edges against a tight Exact reference (defaults:
//! 0.2 seconds, 0,100 us, reltol 1e-4; its rate `ELEC_REF_RATE`, 96 kHz unless set, so `ELEC_RATE`
//! moves only the run it is compared with), with waveform errors and LINE/NOISE third-octave
//! spectra on a uniform 1 us grid. With `ELEC_TAKE=1`, board-acc also prints spectra for the
//! LINE and NOISE takes the host's recorder delivers at 48 kHz, the samples a person hears
//! and the basis for A-N9b across rates. In rounds it judges past the first plan and the read's
//! settling time, with the start reported separately. A second candidate take checks what the
//! merged points inside a period contribute. With `ELEC_TAKE_SELF=1` as well, it runs the reference
//! twice, applies both judges and compares both nets' delivered frames by bits. With
//! `ELEC_SHIP_PLAN=1` (bar B's split, at the shipping settings) its run in rounds holds the plan
//! in force to the shipping plan's blocks and stages (`SHIP_PLAN`).
//! Fast's bars against that reference (`ELEC_RATE=48000`, res
//! 100 us): the worst pitch (between the triggers) at most 0.28 cents off, the LINE third-octave band furthest off at
//! most 1.682 dB (1.65 dB until the bypass corner fix, #6659, made the bypass correct and moved
//! the band with it; written 1.665 dB then, a reading its own source does not give: it reads
//! 1.684 dB, under the JIT and natively alike). Those are a take at rest. A pressed take's pitch
//! bar adds Fast's lag after a value change. The two backward Euler steps that open the history
//! after a change (a pin the firmware drives) stay full length on purpose: they damp the stiff
//! modes the change excites.
//! Shortened as a crossing's are, the supply current one sample later was 12 times further off.
//! Each change then lags every RC charge by about h^2 / (2 tau), a delay every period in
//! proportion to the change rate, so the pitch goes flat in proportion to change rate times
//! frequency. With a key held, the firmware's scan makes 1016 changes in 0.5 s against 140 at
//! rest: 0.06 cents more on the 1.1 kHz metal, so the bar for drum16 held at 48 kHz is 0.34 cents.
//! A check at another change rate scales that excess with its own rate.
//! `ELEC_PRESSES=<from s>,<for s>,<presses a second>,<hold ms>,<key>/..`
//! presses drum16's keys in every run of it as board-tap does (its tapped and held takes).
//! `ELEC_PATTERN=full` starts drum16 with a saved pattern of every voice and the accent on every
//! step, the board's heaviest load (the firmware loads its EEPROM at power-up).
//! `start [secs]` plays drum16 cold, again and after a resistor edit, paced at 1× (default 4 s).
//! The three plays share the planner's cache. `ELEC_EDIT=<resistor>,<ohms>` changes the third
//! board before its operating point (default `hats.cdis,330000`, the closed hat's decay).
//! `ELEC_RATE` picks the run's Hz; `ELEC_HELPERS=0` steps it whole (default: in rounds on the
//! host's pool, `ELEC_HELPERS=host`).
//! `ELEC_STEPS_A_PERIOD=<n>` caps a Fast board step at 1/n of its sample period for n >= 2,
//! keeping its sample rate and noise spacing; its rounds keep the plan made at the shipping step,
//! so bar A's split at the quarter step plays the plan that ships.
//! `board <secs> <res us,..> snapshot` takes a snapshot of the
//! run (`LiveSim::snapshot`) at each return from `advance` and prints what one costs: microseconds and
//! the bytes it allocates (counted by the example's allocator), and a restore's microseconds.
//! `board-edit <secs> <res us,..> <resistor> <ohms> <at s> <behind s>` edits a resistor's value
//! while the board plays, as the host does while the recording plays (defaults: 0.4 s, 0,100 us,
//! the hats' mix resistor to 10 kohm at 0.2 s, sent 0.05 s past it), against the run with the
//! value on its timeline and the board as it stands, through `run_live` with the jack recorder.
//! `board-tape <secs> <res us> [once]` renders drum16's board as the host renders a take, its
//! hearing recorder taping the output jack alone and then the jack and eight voltage probes,
//! alternating, twice (`once`: the eight once): the worker's speed, the sinks' share of its
//! stepping, and how soon a Listen over the take at rest has it whole (defaults: 100 us); with
//! `once`, whether that Listen is bit for bit a recorder of its channel alone from the start.
//! `board-tap <secs> <res us> <from s> <for s> <presses a second> <hold ms> <keys>` runs drum16 in
//! rounds at rest and with keys pressed (defaults: 2.5 s, 100 us, keys 0, 5 and 10 each pressed 6
//! times a second for 80 ms from 0.3 s for 2 s), two passes interleaved, and times the window on
//! its own: the realtime factor a person pressing pads hears keep up or not.

use fab_elec_bench::{lower, ModelLibs};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_avr::sim::{Cpu, Stop};
use fab_elec_mcu_cosim::{CoMachine, Config, FwDebug, LogicMachine, Stats};
use fab_elec_mcu_emu::{Machine, Ran, Until};
use fab_elec_sim::analysis::partition::Land;
use fab_elec_sim::analysis::tran::Restarts;
use fab_elec_sim::cosim::{Bus, CoAudio, CoRun, CoSim, CoSimDevice, CoSimPin, CoState, StepTo, Watch};
use fab_elec_sim::fft::{fft, Window};
use fab_elec_sim::listen::{AudioTap, ListenBus, ReadCounts, RecordSpec, TapMix, TapSpec, TapVoice, BAND_HALF, BAND_LAG, LOOP_SECONDS};
use fab_elec_sim::live::{Latest, LiveCmd, LiveValues, ValuePlan};
use fab_elec_sim::livesim::{run_live, LiveSim, LiveSpec, LiveStats, SinkCmd, SinkSet, Snapshots, AUDIO_SAMPLE_RATE, FAST_RELTOL};
use fab_elec_sim::recording::{settle_tapes, window_frames, RunContent, RunSlot, TapeShelf, CHUNK};
use fab_elec_sim::stamp::{UnkKind, UnknownInfo};
use fab_elec_sim::trace::{Keep, LogicEdge, TracePublisher, TraceStore};
use fab_elec_sim::{C64, PinMode, Profile, SimCircuit, SimDiag, Sink, SinkState};
use fab_elec_simhost::{host_pool, host_rounds, setup_line, warm_ahead_of, RunPool};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The bytes allocated so far (`board ... snapshot`: what a snapshot takes).
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting the bytes it hands out.
struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(new_size.saturating_sub(layout.size()) as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;
/// Plans made off the shape `ELEC_SHAPE` pins (`Declared`): a planner's panic would only leave its
/// plan untaken, so the run counts them and fails at its end.
static OFF_SHAPE: AtomicU64 = AtomicU64::new(0);

const FW: &str = include_str!("../../../fixtures/ledavr.fw.splash");
const DRUM_FW: &str = include_str!("../../../fixtures/drum16.fw.splash");

/// The button: pressed over [PRESS, RELEASE) for breathe (the firmware polls it every 10 ms).
const PRESS: f64 = 2e-3;
const RELEASE: f64 = 30e-3;
/// Where a window starts: past the press, on the mode's steady pattern.
const SETTLED: f64 = 40e-3;

/// The lowered chase bench and its firmware image.
struct Board {
    circuit: SimCircuit,
    plan: ValuePlan,
    generation: u64,
    pins: Vec<CoSimPin>,
    words: Arc<[u16]>,
    hz: u64,
    debug: Arc<FwDebug>,
}

fn board() -> Board {
    let sources = fab_elec_fixtures::docs("ledavr").expect("ledavr is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("ledavr evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "chase").expect("the chase bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "ledavr lowers clean: {:?}", lowered.diags);
    let binding = &lowered.firmware[0];
    let consts: Vec<(&str, f64)> = binding.params.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let fw = fab_elec_mcu_emit_avr::firmware(FW, binding.f_clk.expect("a clock"), &consts).expect("the firmware builds");
    Board {
        pins: lowered.cosim_pins(&binding.part),
        circuit: lowered.circuit.clone(),
        plan: lowered.plan.clone(),
        generation: lowered.index.generation,
        words: fw.words.into(),
        hz: u64::from(fw.f_clk),
        debug: Arc::new(fw.debug),
    }
}

fn pin_index(name: &str) -> usize {
    AvrMachine::new(&[0], 1).pins().iter().position(|p| *p == name).expect("a package pin")
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The interpreter alone: `secs` simulated seconds from SETTLED, after the mode's press.
fn avr(b: &Board, breathe: bool, secs: f64) {
    let tick = |t: f64| (t * b.hz as f64) as u64;
    let (from, end) = (tick(SETTLED), tick(SETTLED + secs));
    let presses = if breathe { vec![(tick(PRESS), false), (tick(RELEASE), true)] } else { Vec::new() };
    let mode = if breathe { "breathe" } else { "chase" };

    // raw steps
    let mut cpu = Cpu::new(&b.words);
    cpu.set_cosim_timing(true);
    cpu.set_pin('D', 2, true);
    let run_to = |cpu: &mut Cpu, to: u64, count: &mut (u64, u64)| {
        while cpu.cycles < to {
            if cpu.will_execute() {
                count.0 += 1;
            } else {
                count.1 += 1;
            }
            if let Err(stop) = cpu.step_until(to) {
                assert!(matches!(stop, Stop::Cycles | Stop::Sleep), "the firmware stopped: {stop:?}");
            }
        }
    };
    let mut warm = (0, 0);
    for &(at, high) in &presses {
        run_to(&mut cpu, at, &mut warm);
        cpu.set_pin('D', 2, high);
    }
    run_to(&mut cpu, from, &mut warm);
    let mut count = (0u64, 0u64);
    let start = Instant::now();
    run_to(&mut cpu, end, &mut count);
    let raw = start.elapsed();
    let (insns, entries) = count;
    println!("avr {mode}: {secs} s simulated = {} cycles; {insns} instructions ({:.0}/s), {entries} sleeps or interrupt entries",
        end - from, insns as f64 / secs);
    println!("avr {mode}: Cpu::step_until {:.1} ms wall per simulated second, {:.1} ns per instruction, realtime x{:.1}",
        ms(raw) / secs, raw.as_nanos() as f64 / insns.max(1) as f64, secs / raw.as_secs_f64());

    // the machine as the adapter runs it: marks, a return on every drive change
    let mut m = AvrMachine::new(&b.words, b.hz);
    let pd2 = pin_index("PD2");
    m.set_input(pd2, true);
    let marks = b.debug.map.as_ref().map(|m| m.marks()).unwrap_or_default();
    let names: Vec<&str> = m.pins().to_vec();
    let mut per_pin = vec![0u64; names.len()];
    let (mut edges, mut trace) = (Vec::new(), Vec::new());
    let (mut drives, mut runs) = (0u64, 0u64);
    let mut run = |m: &mut AvrMachine, to: u64, counting: bool, per_pin: &mut Vec<u64>| {
        while m.now() < to {
            let ran = m.run(&Until { tick: to, marks: &marks, stops: &[], stops_after: 0, on_drive: true });
            m.take_drives(&mut edges);
            for e in edges.drain(..) {
                if counting {
                    per_pin[e.pin as usize] += 1;
                }
            }
            m.take_trace(&mut trace);
            trace.clear();
            if counting {
                runs += 1;
                if matches!(ran, Ran::Drive { .. }) {
                    drives += 1;
                }
            }
            assert!(matches!(ran, Ran::Drive { .. } | Ran::Tick), "unexpected stop {ran:?}");
        }
    };
    for &(at, high) in &presses {
        run(&mut m, at, false, &mut per_pin);
        m.set_input(pd2, high);
    }
    run(&mut m, from, false, &mut per_pin);
    let start = Instant::now();
    run(&mut m, end, true, &mut per_pin);
    let wall = start.elapsed();
    println!("avr {mode}: AvrMachine::run with marks {:.1} ms wall per simulated second ({:.1} ns per instruction), {runs} runs, {drives} drive changes ({:.0}/s); per pin:",
        ms(wall) / secs, wall.as_nanos() as f64 / insns.max(1) as f64, drives as f64 / secs);
    let line: Vec<String> = names.iter().zip(&per_pin).filter(|(_, n)| **n > 0).map(|(name, n)| format!("{name} {:.0}/s", *n as f64 / secs)).collect();
    println!("       {}", line.join(", "));
}

/// drum16's firmware on the interpreter alone (`drumavr <secs>`), no analog side: its pins as they
/// start, its ADC channels held at 2.5 V. `Cpu::step_until` over `secs` simulated after 0.1 s,
/// with the cycles it executed and those it slept through, as simulated MHz against its clock.
fn drum_avr(secs: f64) {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
    let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let fw = &lowered.firmware[0];
    let params: Vec<(&str, f64)> = fw.params.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let image = fab_elec_mcu_emit_avr::firmware(DRUM_FW, fw.f_clk.expect("a clock"), &params).unwrap_or_else(|d| panic!("the firmware builds: {d:?}"));
    let hz = u64::from(image.f_clk);
    let (from, end) = (hz / 10, hz / 10 + (secs * hz as f64) as u64);
    // standalone: the ADC reads held voltages (the knobs at mid-travel), not the co-simulation's
    let mut cpu = Cpu::new(&image.words);
    cpu.set_avcc(5.0);
    for channel in 0..8 {
        cpu.set_analog(channel, 2.5);
    }
    // (instructions, cycles they took, sleeps or interrupt entries)
    let run_to = |cpu: &mut Cpu, to: u64, count: &mut (u64, u64, u64)| {
        while cpu.cycles < to {
            let (insn, c0) = (cpu.will_execute(), cpu.cycles);
            if let Err(stop) = cpu.step_until(to) {
                assert!(matches!(stop, Stop::Cycles | Stop::Sleep), "the firmware stopped: {stop:?}");
            }
            if insn {
                count.0 += 1;
                count.1 += cpu.cycles - c0;
            } else {
                count.2 += 1;
            }
        }
    };
    run_to(&mut cpu, from, &mut (0, 0, 0));
    let mut count = (0, 0, 0);
    let start = Instant::now();
    run_to(&mut cpu, end, &mut count);
    let wall = start.elapsed().as_secs_f64();
    let (insns, busy, entries) = count;
    let cycles = cpu.cycles - from;
    println!("drumavr: {secs} s simulated at {:.0} MHz = {cycles} cycles in {:.1} ms wall: {insns} instructions taking {busy} cycles ({:.1} % of the clock awake), {entries} sleeps or interrupt entries",
        hz as f64 / 1e6, wall * 1e3, 100.0 * busy as f64 / cycles.max(1) as f64);
    println!("drumavr: {:.0} simulated MHz overall (realtime x{:.1}); awake, {:.0} simulated MHz ({:.1} ns an instruction, {:.0} MIPS)",
        cycles as f64 / wall / 1e6, secs / wall, busy as f64 / wall / 1e6, wall * 1e9 / insns.max(1) as f64, insns as f64 / wall / 1e6);
}

/// What the device wrapper saw: the adapter's counts, and why each run happened.
#[derive(Clone, Copy, Debug, Default)]
struct Seen {
    co: Stats,
    /// Runs that applied a drive change, that took a watch hit, and the others (commits).
    edge_runs: u64,
    hit_runs: u64,
    other_runs: u64,
    writes: u64,
}

struct Probe {
    inner: CoMachine<AvrMachine>,
    seen: Arc<Mutex<Seen>>,
    local: Seen,
}

struct CountBus<'a> {
    bus: &'a mut dyn Bus,
    writes: u64,
}

impl Bus for CountBus<'_> {
    fn t(&self) -> f64 {
        self.bus.t()
    }
    fn v(&self, pin: u32) -> f64 {
        self.bus.v(pin)
    }
    fn supply(&self, pin: u32) -> f64 {
        self.bus.supply(pin)
    }
    fn advance(&mut self, t: f64) -> Result<(), SimDiag> {
        self.bus.advance(t)
    }
    fn write(&mut self, pin: u32, mode: PinMode) -> bool {
        self.writes += 1;
        self.bus.write(pin, mode)
    }
    fn hits(&self) -> &[(u32, f64)] {
        self.bus.hits()
    }
}

impl CoSim for Probe {
    fn next(&self) -> f64 {
        self.inner.next()
    }
    fn run(&mut self, bus: &mut dyn Bus) -> Result<CoRun, SimDiag> {
        let hit = !bus.hits().is_empty();
        let mut cb = CountBus { bus, writes: 0 };
        let r = self.inner.run(&mut cb);
        let writes = cb.writes;
        self.local.writes += writes;
        if hit {
            self.local.hit_runs += 1;
        } else if writes > 0 {
            self.local.edge_runs += 1;
        } else {
            self.local.other_runs += 1;
        }
        self.local.co = self.inner.stats();
        *self.seen.lock().unwrap() = self.local;
        r
    }
    fn watches(&self) -> &[Watch] {
        self.inner.watches()
    }
    fn watch_gen(&self) -> u32 {
        self.inner.watch_gen()
    }
    fn arm(&mut self, t: f64, to: Option<StepTo>) -> bool {
        self.inner.arm(t, to)
    }
    fn state(&mut self, t: f64) -> Option<CoState> {
        self.inner.state(t)
    }
    fn logic_nets(&self) -> &[String] {
        self.inner.logic_nets()
    }
    fn take_logic(&mut self, t: f64, sync: bool, out: &mut Vec<(u32, LogicEdge)>) {
        self.inner.take_logic(t, sync, out)
    }
    fn restart_logic(&mut self, t: f64, out: &mut Vec<(u32, LogicEdge)>) {
        self.inner.restart_logic(t, out)
    }
}

/// Every accepted point's solution once `on` (the measured window), for the accuracy comparison.
#[derive(Default)]
struct Rec {
    on: bool,
    keep: bool,
    t: Vec<f64>,
    x: Vec<f64>,
    n: usize,
    /// Only these solver columns, in this order; `None` keeps the whole solution.
    columns: Option<Vec<usize>>,
    /// With `ELEC_DIGEST` set: every accepted point from the start, its time and solution bits
    /// folded into one hash, and the points counted (a bit-exact comparison between builds).
    digest: Option<(u64, u64)>,
    /// With `ELEC_DIGEST_EVERY=<s>` as well: the digest so far printed at each multiple of s seconds
    /// of simulated time (the next mark), to find where two builds' runs part.
    every: Option<(f64, f64)>,
    /// With `ELEC_DIGEST_DUMP=<from s>,<to s>`: each point in that window printed, its time and
    /// solution as bits.
    dump: Option<(f64, f64)>,
    /// What the run is heard at the sample rate (`Sink::heard`): the line out, as the app's jack
    /// recorder hears it, so a run in rounds keeps its block on the sample period.
    heard: Vec<u32>,
    /// The delivered LINE and NOISE takes when board-acc records them, in `TAKE_NETS` order.
    takes: Option<[BoardTake; 2]>,
    /// The same candidate points, filtered before they reach these recorders.
    control: Option<[BoardTake; 2]>,
    take_clock: TakeClock,
    /// The solver's relative tolerance for the board run being compared.
    reltol: f64,
}

/// Folds `at` and `x` into a running FNV-1a style hash of their bits.
fn fold(h: &mut (u64, u64), at: f64, x: &[f64]) {
    for b in std::iter::once(at.to_bits()).chain(x.iter().map(|v| v.to_bits())) {
        h.0 = (h.0 ^ b).wrapping_mul(0x100000001b3);
    }
    h.1 += 1;
}

impl Sink for Rec {
    fn heard(&mut self, out: &mut Vec<u32>) {
        out.extend_from_slice(&self.heard);
    }
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        if let (Some(h), Some((every, next))) = (&self.digest, &mut self.every) {
            while at >= *next {
                println!("  digest at {:.4} s: {} points {:016x}", *next, h.1, h.0);
                *next += *every;
            }
        }
        if self.dump.is_some_and(|(from, to)| at >= from && at <= to) {
            println!("  point {:016x} {}", at.to_bits(), x.iter().map(|v| format!("{:016x}", v.to_bits())).collect::<Vec<_>>().join(" "));
        }
        if let Some(h) = &mut self.digest {
            fold(h, at, x);
        }
        if self.on && self.keep {
            self.t.push(at);
            if let Some(columns) = &self.columns {
                self.n = columns.len();
                self.x.extend(columns.iter().map(|&i| x[i]));
            } else {
                self.n = x.len();
                self.x.extend_from_slice(x);
            }
        }
    }
}

/// Linear interpolation of unknown `i` of `r` at the times `grid` (ascending).
fn resample(r: &Rec, i: usize, grid: &[f64], out: &mut Vec<f64>) {
    out.clear();
    let mut k = 0;
    for &g in grid {
        while k + 2 < r.t.len() && r.t[k + 1] <= g {
            k += 1;
        }
        let (t0, t1) = (r.t[k], r.t[k + 1]);
        let (a, b) = (r.x[k * r.n + i], r.x[(k + 1) * r.n + i]);
        out.push(if t1 > t0 { a + (b - a) * ((g - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { b });
    }
}

/// How far `cand` is from `base` over their common window, per unknown against the base's range:
/// the worst RMS difference and the worst difference of means (what the eye's glow reads).
fn compare(base: &Rec, cand: &Rec, names: &[String]) -> String {
    let (from, to) = (base.t[0].max(cand.t[0]), base.t[base.t.len() - 1].min(cand.t[cand.t.len() - 1]));
    let samples = 400_000;
    let grid: Vec<f64> = (0..samples).map(|k| from + (to - from) * (k as f64 + 0.5) / samples as f64).collect();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut worst_rms = (0.0, 0usize, 0.0);
    let mut worst_mean = (0.0, 0usize);
    for i in 1..base.n {
        resample(base, i, &grid, &mut a);
        resample(cand, i, &grid, &mut b);
        let (lo, hi) = a.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let range = hi - lo;
        if !(range > 1e-3) {
            continue;
        }
        let ss: f64 = a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum();
        let rms = (ss / samples as f64).sqrt() / range;
        let mean = (a.iter().sum::<f64>() - b.iter().sum::<f64>()).abs() / samples as f64 / range;
        if rms > worst_rms.0 {
            worst_rms = (rms, i, range);
        }
        if mean > worst_mean.0 {
            worst_mean = (mean, i);
        }
    }
    format!("vs baseline over {:.1} ms: worst RMS difference {:.2e} of range ({} range {:.3}), worst mean difference {:.2e} of range ({})",
        (to - from) * 1e3, worst_rms.0, names[worst_rms.1], worst_rms.2, worst_mean.0, names[worst_mean.1])
}

/// Everything a window reports, read before and after it.
#[derive(Clone)]
struct Snap {
    t: f64,
    live: LiveStats,
    st: fab_elec_sim::newton::Stats,
    rs: Restarts,
    profile: Option<Profile>,
    seen: Seen,
}

fn snap(sim: &LiveSim, seen: &Arc<Mutex<Seen>>) -> Snap {
    Snap {
        t: sim.t(),
        live: sim.stats().clone(),
        st: sim.solver().stats,
        rs: sim.restarts(),
        profile: sim.solver().profile.as_deref().cloned(),
        seen: *seen.lock().unwrap(),
    }
}

/// The run's sample rate: `ELEC_RATE` in Hz, else `default` (0: none chosen, the bandwidth step).
fn sample_rate(default: f64) -> f64 {
    std::env::var("ELEC_RATE").ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

/// The band the host plans a heard run in rounds in (`round::take_band`): its takes leave at the
/// heard recording's rate (`REALTIME_RECORDING_RATE`), at most half the run's own `rate`.
fn heard_band(rate: f64) -> f64 {
    fab_elec_sim::analysis::round::take_band(rate, fab_elec_sim::livesim::REALTIME_RECORDING_RATE)
}

/// drum16's sample rate: board-acc's reference runs at `ELEC_REF_RATE` in Hz, else the host's
/// audio rate, so `ELEC_RATE` moves only the candidate and board-acc measures what a rate costs;
/// every other run takes `sample_rate`.
fn drum_rate(reference: bool) -> f64 {
    if reference {
        std::env::var("ELEC_REF_RATE").ok().and_then(|s| s.parse().ok()).unwrap_or(AUDIO_SAMPLE_RATE)
    } else {
        sample_rate(AUDIO_SAMPLE_RATE)
    }
}

fn rest_profile(p0: &Profile, p1: &Profile, t: f64) {
    let ms = |f: fn(&Profile) -> u64| (f(p1) - f(p0)) as f64 / 1e6 / t;
    println!("  rest split: combine {:.1} ms, update {:.1} ms; predict {:.1} ms, crossing {:.1} ms, charge {:.1} ms, estimate {:.1} ms, accept {:.1} ms, emit {:.1} ms; profile overhead {:.1} ms",
        ms(|p| p.combine_ns), ms(|p| p.update_ns), ms(|p| p.predict_ns), ms(|p| p.crossing_ns), ms(|p| p.charge_ns),
        ms(|p| p.estimate_ns), ms(|p| p.accept_ns), ms(|p| p.emit_ns), ms(|p| p.profile_overhead_ns));
    println!("  other rest: clear {:.1} ms, control {:.1} ms", ms(|p| p.clear_ns), ms(|p| p.control_ns));
}

/// The live worker's loop, unpaced: to SETTLED (the mode reached), then a measured window.
fn live(b: &Board, breathe: bool, res: f64, trace: bool, profile: bool, secs: f64, variant: u32, keep: bool) -> (Rec, Vec<String>) {
    let mut circuit = b.circuit.clone();
    circuit.opts.profile = profile;
    if variant & 1 != 0 || std::env::var_os("ELEC_FORCE_DENSE").is_some() {
        circuit.opts.force_dense = Some(true);
    }
    let spec = LiveSpec { rate: f64::INFINITY, trace, trace_pins: trace, res, sample_rate: sample_rate(0.0), ..LiveSpec::default() };
    let record = spec.record;
    let mut sim = LiveSim::new(circuit, b.plan.clone(), b.generation, spec).expect("ledavr's operating point");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut co = CoMachine::new(AvrMachine::new(&b.words, b.hz), Config { stretch: 4000, debug: Some(b.debug.clone()) }, &b.pins).expect("a device");
    let events = Arc::new(Mutex::new(0u64));
    {
        let events = events.clone();
        co.set_events(Some(Box::new(move |void, batch| {
            let mut n = events.lock().unwrap();
            *n = n.saturating_sub(void) + batch.len() as u64;
            batch.clear();
            true
        })), false);
    }
    sim.attach(Some(CoSimDevice { dev: Box::new(Probe { inner: co, seen: seen.clone(), local: Seen::default() }), pins: b.pins.clone() }));
    let press = |sim: &mut LiveSim, on: bool| {
        assert!(sim.apply(LiveCmd::SetParam { generation: b.generation, device: "btn.sw#press".into(), key: "dc".into(), value: if on { 1.0 } else { 0.0 } }));
        assert_eq!(sim.stats().unknown, 0, "the button takes the press");
    };

    let values: Latest<LiveValues> = Latest::new();
    let mut traces = TracePublisher::new(Arc::new(Latest::<TraceStore>::new()));
    let digest = std::env::var_os("ELEC_DIGEST").map(|_| (0xcbf29ce484222325, 0));
    let mut sink = Rec { keep, digest, ..Rec::default() };
    let mut walls = [Duration::ZERO; 3];
    let (mut publications, mut deltas) = (0u64, 0u64);
    let mut run_to = |sim: &mut LiveSim, sink: &mut Rec, to: f64, walls: &mut [Duration; 3], publications: &mut u64, deltas: &mut u64| {
        let every = (Duration::from_millis(16), Duration::from_millis(8));
        let start = Instant::now();
        let (mut last_values, mut last_trace, mut published_t) = (start, start, sim.t());
        while sim.t() < to {
            let a = Instant::now();
            sim.advance(to, Some(Duration::from_millis(8)), sink).expect("ledavr steps");
            let now = Instant::now();
            walls[0] += now - a;
            if now - last_values >= every.0 {
                let ratio = (sim.t() - published_t) / (now - last_values).as_secs_f64();
                values.publish(Arc::new(sim.values(ratio)));
                walls[1] += now.elapsed();
                *publications += 1;
                last_values = now;
                published_t = sim.t();
            }
            let now = Instant::now();
            if now - last_trace >= every.1 {
                if let Some(d) = sim.take_delta() {
                    traces.publish(d, &Keep::last(record));
                    *deltas += 1;
                }
                walls[2] += now.elapsed();
                last_trace = now;
            }
        }
    };
    if breathe {
        let (mut w, mut p, mut d) = ([Duration::ZERO; 3], 0, 0);
        run_to(&mut sim, &mut sink, PRESS, &mut w, &mut p, &mut d);
        press(&mut sim, true);
        run_to(&mut sim, &mut sink, RELEASE, &mut w, &mut p, &mut d);
        press(&mut sim, false);
    }
    {
        let (mut w, mut p, mut d) = ([Duration::ZERO; 3], 0, 0);
        run_to(&mut sim, &mut sink, SETTLED, &mut w, &mut p, &mut d);
    }
    sink.on = true;
    let a = snap(&sim, &seen);
    let start = Instant::now();
    run_to(&mut sim, &mut sink, SETTLED + secs, &mut walls, &mut publications, &mut deltas);
    let wall = start.elapsed();
    let z = snap(&sim, &seen);

    let t = z.t - a.t;
    let per_s = |x: u64| x as f64 / t;
    let d = |f: fn(&Snap) -> u64| f(&z) - f(&a);
    let dd = |f: fn(&Snap) -> Duration| f(&z) - f(&a);
    let (step, co_wall, sink_wall) = (dd(|s| s.live.step_wall), dd(|s| s.live.co_wall), dd(|s| s.live.sink_wall));
    let solver_wall = step.saturating_sub(co_wall).saturating_sub(sink_wall);
    let label = format!("{}, {}{}{}", if breathe { "breathe" } else { "chase" },
        if res == 0.0 { "Exact".to_string() } else { format!("Fast res {:.0} us", res * 1e6) },
        if trace { "" } else { ", no trace" }, if profile { " (profiled)" } else { "" }) + &if variant != 0 { ", dense LU forced".to_string() } else { String::new() };
    println!();
    println!("live {label}: {:.1} ms simulated in {:.1} ms wall: realtime x{:.3}; {} unknowns; per simulated second:",
        t * 1e3, ms(wall), t / wall.as_secs_f64(), sim.solver().n);
    let pms = |x: Duration| ms(x) / t;
    println!("  wall ms: device {:.1}, analog {:.1}, sinks {:.1}, values {:.1} ({}), trace {:.1} ({} deltas), loop {:.1}; total {:.1}",
        pms(co_wall), pms(solver_wall), pms(sink_wall), pms(walls[1]), publications, pms(walls[2]), deltas,
        pms(wall.saturating_sub(walls[0] + walls[1] + walls[2])), pms(wall));
    let (steps, iterations) = (d(|s| s.st.steps), d(|s| s.st.iterations));
    println!("  analog: {:.0} points, {:.0} steps tried, rejected {:.0} (newton {:.0}, crossing {:.0}, provisional {:.0}, error {:.0}), {:.2} Newton iterations a step, {:.0} loads, {:.0} solves",
        per_s(d(|s| s.live.points)), per_s(steps), per_s(d(|s| s.st.rejected)), per_s(d(|s| s.st.rejected_newton)), per_s(d(|s| s.st.rejected_crossing)),
        per_s(d(|s| s.st.rejected_provisional)), per_s(d(|s| s.st.rejected_error)), iterations as f64 / steps.max(1) as f64,
        per_s(d(|s| s.st.loads)), per_s(d(|s| s.st.solves)));
    println!("  restarts: change {:.0}, corner {:.0}, crossing {:.0} (cut ahead {:.0}; cuts ahead short {:.0}); floor steps {:.0}, warm starts {:.0}; {:.2} us a step tried, {:.2} us a Newton iteration",
        per_s(d(|s| s.rs.change)), per_s(d(|s| s.rs.corner)), per_s(d(|s| s.rs.crossing)), per_s(d(|s| s.rs.crossing_ahead)), per_s(d(|s| s.rs.ahead_short)), per_s(d(|s| s.st.floor_steps)),
        per_s(d(|s| s.st.warm_starts)), solver_wall.as_secs_f64() * 1e6 / steps.max(1) as f64, solver_wall.as_secs_f64() * 1e6 / iterations.max(1) as f64);
    let runs = d(|s| s.live.co_runs);
    println!("  device: {:.0} runs (drive changes {:.0}, hits {:.0}, other {:.0}), writes {:.0}, write instants {:.0}, reruns {:.0}, commits {:.0}; {:.2} us a run",
        per_s(runs), per_s(d(|s| s.seen.edge_runs)), per_s(d(|s| s.seen.hit_runs)), per_s(d(|s| s.seen.other_runs)), per_s(d(|s| s.seen.writes)),
        per_s(d(|s| s.live.write_instants)), per_s(d(|s| s.seen.co.reruns)), per_s(d(|s| s.seen.co.commits)), co_wall.as_secs_f64() * 1e6 / runs.max(1) as f64);
    if let (Some(p0), Some(p1)) = (&a.profile, &z.profile) {
        let total = solver_wall.as_nanos() as f64;
        let share = |x: u64| 100.0 * x as f64 / total;
        let (load, refactor, factor, solve) = (p1.load_ns - p0.load_ns, p1.refactor_ns - p0.refactor_ns, p1.factor_ns - p0.factor_ns, p1.solve_ns - p0.solve_ns);
        println!("  analog profile: load {:.1} ms ({:.0} %), factor {:.1} ms ({:.0}, {:.0} %), refactor {:.1} ms ({:.0}, {:.0} %), solve {:.1} ms ({:.0} %), the rest {:.1} ms ({:.0} %)",
            load as f64 / 1e6 / t, share(load), factor as f64 / 1e6 / t, per_s(p1.factors - p0.factors), share(factor),
            refactor as f64 / 1e6 / t, per_s(p1.refactors - p0.refactors), share(refactor), solve as f64 / 1e6 / t, share(solve),
            (total - (load + refactor + factor + solve) as f64) / 1e6 / t, 100.0 - share(load + refactor + factor + solve));
        rest_profile(p0, p1, t);
    }
    if let Some((h, n)) = sink.digest {
        println!("  digest {label}: {n} points {h:016x}");
    }
    let names = sim.solver().unknowns.iter().map(|u| u.name.clone()).collect();
    (sink, names)
}

fn devices(c: &SimCircuit) {
    let d = &c.devices;
    println!("ledavr lowered: {} nodes; r {} c {} v {} i {} sw {} gpio {} d {} q {}",
        c.nodes.len(), d.r.len(), d.c.len(), d.v.len(), d.i.len(), d.sw.len(), d.gpio.len(), d.d.len(), d.q.len());
}

/// The islands' partition report (step I1, no behaviour change): ledavr's chase circuit with every
/// pad in the control set (the image declares no pin directions) and without one, then each of
/// drum16's benches as it stands, at 96 kHz.
/// Accepted steps by length against the run's step bound (`max`): bucket k holds steps in
/// [max / 2^(k+1), max / 2^k), the last everything shorter.
struct Steps {
    max: f64,
    last: Option<f64>,
    hist: [u64; 12],
}

impl Sink for Steps {
    fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) {
        if let Some(last) = self.last.replace(at) {
            let h = at - last;
            let k = if h <= 0.0 { 11 } else { ((self.max / h).log2().floor().max(0.0) as usize).min(11) };
            self.hist[k] += 1;
        }
    }
}

/// The step lengths and the selected solutions/digest together.
struct Both<'a>(&'a mut Steps, &'a mut Rec);

impl Sink for Both<'_> {
    fn heard(&mut self, out: &mut Vec<u32>) {
        self.1.heard(out);
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        self.0.point(at, outer, x);
        self.1.point(at, outer, x);
    }
}

/// The first plan's time and the grid on which the candidate ran.
#[derive(Clone, Copy, Default)]
struct TakeClock {
    newest: f64,
    t_plan: Option<f64>,
    period: f64,
}

#[derive(Clone, Copy)]
struct ControlPoint {
    at: f64,
    outer: Option<f64>,
    mark: bool,
    below_res: bool,
    chosen: bool,
}

/// One point of lookahead. Only the columns the two control taps read are copied per point.
#[derive(Clone, Default)]
struct ControlFilter {
    held: Option<ControlPoint>,
    columns: Vec<u32>,
    x: Vec<f64>,
    previous: Option<f64>,
    previous_was_second: bool,
    last_forwarded: Option<f64>,
}

impl ControlFilter {
    fn begin(&mut self, taps: &mut [AudioTap; 2], unknowns: &[UnknownInfo]) {
        *self = Self::default();
        self.x.resize(unknowns.len(), 0.0);
        for tap in taps {
            tap.heard(&mut self.columns);
        }
        self.columns.sort_unstable();
        self.columns.dedup();
    }

    fn point(&mut self, taps: &mut [AudioTap; 2], period: f64, at: f64, outer: Option<f64>, x: &[f64]) {
        // Never flush a superseded point across a return behind the recorders' front.
        if self.last_forwarded.is_none_or(|last| at >= last) {
            self.flush(taps, period, Some(at));
        }
        if self.last_forwarded.is_some_and(|last| at < last) {
            self.held = None;
            (self.previous, self.previous_was_second, self.last_forwarded) = (Some(at), false, Some(at));
            for tap in taps { tap.point(at, outer, x); }
            return;
        }
        for &col in &self.columns {
            self.x[col as usize] = x[col as usize];
        }
        self.held = Some(ControlPoint { at, outer, mark: false, below_res: false, chosen: false });
    }

    fn flush(&mut self, taps: &mut [AudioTap; 2], period: f64, next: Option<f64>) {
        let Some(p) = self.held.take() else { return };
        // Band::tol compares each pair at its second point's time.
        let equal = |a: f64, b: f64| (b - a).abs() <= 4.0 * f64::EPSILON * b.abs().max(period);
        let second = self.previous.is_some_and(|t| equal(t, p.at));
        let first = next.is_some_and(|t| equal(p.at, t));
        let instant = (p.at - (p.at / period).round() * period).abs() <= 1e-6 * period;
        if instant || p.chosen || first || second || self.previous_was_second {
            for tap in taps {
                tap.point(p.at, p.outer, &self.x);
                if p.mark { tap.mark(p.at); }
                if p.below_res { tap.below_res(p.at); }
                if p.chosen { tap.chosen(p.at); }
            }
            self.last_forwarded = Some(p.at);
        }
        (self.previous, self.previous_was_second) = (Some(p.at), second);
    }
}

/// The run's sinks observing the same steps, plus board-acc's optional filtered pair.
struct TakeSink<'a, const N: usize> {
    sinks: [&'a mut dyn Sink; N],
    control: Option<&'a mut [AudioTap; 2]>,
    filter: ControlFilter,
    clock: TakeClock,
}

impl<const N: usize> TakeSink<'_, N> {
    fn direct(&mut self, mut f: impl FnMut(&mut dyn Sink)) {
        for sink in &mut self.sinks {
            f(&mut **sink);
        }
    }

    fn each(&mut self, mut f: impl FnMut(&mut dyn Sink)) {
        self.direct(&mut f);
        if let Some(taps) = self.control.as_deref_mut() {
            for tap in taps { f(tap); }
        }
    }

    /// A held point's calls wait with it. A point already sent on a return gets its calls now.
    fn point_call(&mut self, call: impl FnMut(&mut dyn Sink), held: impl FnOnce(&mut ControlPoint)) {
        if let Some(p) = &mut self.filter.held {
            held(p);
            self.direct(call);
        } else {
            self.each(call);
        }
    }

    fn flush_control(&mut self) {
        if let Some(taps) = self.control.as_deref_mut() {
            self.filter.flush(taps, self.clock.period, None);
        }
    }
}

/// Each sink gets its own state back at a rewind, in the same order as at the snapshot.
#[derive(Clone)]
struct TakeStates<const N: usize> {
    sinks: [Option<Box<dyn SinkState>>; N],
    control: Option<([Option<Box<dyn SinkState>>; 2], ControlFilter)>,
    clock: TakeClock,
}

impl<const N: usize> Sink for TakeSink<'_, N> {
    fn begin(&mut self, unknowns: &[UnknownInfo]) {
        self.clock = TakeClock::default();
        self.each(|s| s.begin(unknowns));
        if let Some(taps) = self.control.as_deref_mut() {
            self.filter.begin(taps, unknowns);
        }
    }
    fn point(&mut self, at: f64, outer: Option<f64>, x: &[f64]) {
        self.clock.newest = at;
        self.direct(|s| s.point(at, outer, x));
        if let Some(taps) = self.control.as_deref_mut() {
            self.filter.point(taps, self.clock.period, at, outer, x);
        }
    }
    fn values(&mut self, at: f64, net_v: &[f64], pin_i: Option<&[f64]>) {
        self.direct(|s| s.values(at, net_v, pin_i));
    }
    fn mark(&mut self, at: f64) {
        self.point_call(|s| s.mark(at), |p| p.mark = true);
    }
    fn below_res(&mut self, at: f64) {
        self.point_call(|s| s.below_res(at), |p| p.below_res = true);
    }
    fn chosen(&mut self, at: f64) {
        self.point_call(|s| s.chosen(at), |p| p.chosen = true);
    }
    fn sample_period(&mut self, period: f64) {
        self.clock.period = period;
        self.each(|s| s.sample_period(period));
    }
    fn ac_point(&mut self, f: f64, x: &[C64]) {
        self.each(|s| s.ac_point(f, x));
    }
    fn idle(&mut self) {
        self.flush_control();
        self.each(|s| s.idle());
    }
    fn started(&mut self) {
        self.clock.t_plan.get_or_insert(self.clock.newest);
        self.each(|s| s.started());
    }
    fn steady(&mut self, at: f64, x: &[f64]) {
        self.flush_control();
        self.each(|s| s.steady(at, x));
    }
    fn ended(&mut self, at: f64) {
        self.flush_control();
        self.each(|s| s.ended(at));
    }
    fn pending(&self) -> bool {
        self.sinks.iter().fold(false, |pending, s| s.pending() | pending)
    }
    fn progress(&self) -> u64 {
        self.sinks.iter().map(|s| s.progress()).fold(0, u64::wrapping_add)
    }
    fn play_recorded(&mut self, shelf: &TapeShelf, slot: u64, at_rest: bool) -> Result<(), String> {
        let replies = self.sinks.each_mut().map(|s| s.play_recorded(shelf, slot, at_rest));
        replies.into_iter().collect()
    }
    fn live_only(&self) -> bool {
        self.sinks.iter().fold(true, |only, s| s.live_only() & only)
    }
    fn holds(&mut self) -> bool {
        self.sinks.iter_mut().fold(false, |held, s| s.holds() | held)
    }
    fn clock_wait(&mut self, wall: std::time::Duration) {
        for s in &mut self.sinks {
            s.clock_wait(wall);
        }
    }
    fn listened(&self) -> bool {
        self.sinks.iter().any(|s| s.listened())
    }
    fn heard(&mut self, out: &mut Vec<u32>) {
        // A heard net keeps its block on the sample period. Only the board's original sink
        // names nets here so that recording NOISE cannot change the run board-acc judges.
        self.sinks[0].heard(out);
    }
    fn co_audio(&mut self, audio: &CoAudio) {
        self.each(|s| s.co_audio(audio));
    }
    fn save(&self) -> Option<Box<dyn SinkState>> {
        Some(Box::new(TakeStates {
            sinks: self.sinks.each_ref().map(|s| s.save()),
            control: self.control.as_deref().map(|taps| (taps.each_ref().map(|s| s.save()), self.filter.clone())),
            clock: self.clock,
        }))
    }
    fn rewind(&mut self, at: f64, saved: Option<&dyn SinkState>, unknowns: &[UnknownInfo]) {
        let states = saved.and_then(|s| s.downcast_ref::<TakeStates<N>>());
        self.clock = states.map_or(TakeClock { newest: at, t_plan: self.clock.t_plan.filter(|&t| t <= at), ..self.clock }, |s| s.clock);
        for (i, sink) in self.sinks.iter_mut().enumerate() {
            sink.rewind(at, states.and_then(|s| s.sinks[i].as_deref()), unknowns);
        }
        if let Some(taps) = self.control.as_deref_mut() {
            let control = states.and_then(|s| s.control.as_ref());
            for (i, tap) in taps.iter_mut().enumerate() {
                tap.rewind(at, control.and_then(|(s, _)| s[i].as_deref()), unknowns);
            }
            if let Some((_, filter)) = control {
                self.filter.clone_from(filter);
            } else {
                self.filter.begin(taps, unknowns);
            }
        }
    }
}

/// drum16's board planned at the shipping settings (Fast's 100 us at the audio rate, the heard
/// band, the board's own declarations): its blocks and its stages. Every user's run plays this
/// plan, and the planner's tests plan at other steps, so bar B's split holds its plan to it
/// (`ELEC_SHIP_PLAN`): the supplies in block 0, the voices beside the MCU on stage 1, the snare
/// after its noise and the line out last. A planner change that moves it retakes the realtime
/// take and sets it again.
const SHIP_PLAN: (usize, usize) = (8, 4);

#[derive(Default)]
struct DrumRun<'a> {
    res: f64,
    profile: bool,
    reltol: Option<f64>,
    /// Divisors for voltage and current absolute tolerances, respectively.
    tolerance_divisors: Option<(f64, f64)>,
    /// `None` prints the timing report; `Some` records only the named unknowns instead.
    record: Option<&'a [String]>,
    /// Profiled, with the attribution of row B0 (`analysis::attrib`) over the five-net border
    /// view's blocks.
    attrib: bool,
    /// Fold every point into a digest (as `ELEC_DIGEST` does).
    digest: bool,
    /// The three-tier load on or off (`Options::tiers`); `None` keeps the default.
    tiers: Option<bool>,
    /// Fast's device bypass on or off (`Options::bypass`); `None` keeps the default.
    bypass: Option<bool>,
    /// Fast's single pass grade (`Options::single_pass`); `None` keeps the default.
    single: Option<u8>,
    /// A snapshot after every slice, its cost printed.
    snapshot: bool,
    /// Run in rounds from this time on: the board's own round plan (`Declared`), made there.
    rounds: Option<f64>,
    /// The round's length in sample periods (`LiveSpec::round`); `None` keeps the default.
    round: Option<u32>,
    /// The supplies the round plan takes as declared: `None` the board's own, else only these nets.
    declared: Option<Vec<String>>,
    /// Where each plan's time goes, when a plan goes in.
    handovers: Option<Arc<Mutex<Vec<f64>>>>,
    /// board-acc's reference: its sample rate is `ELEC_REF_RATE`'s (`drum_rate`).
    reference: bool,
    /// Record board-acc's LINE and NOISE takes on the delivered grid.
    take: bool,
    /// board-acc's carry source (`ELEC_CARRY`): the board at its stock values, no `ELEC_SET`, its
    /// plan's cut kept in the shared planner memory (`plan_states`) for the candidate to carry.
    carry_source: bool,
}

/// The round planner on the board's own declarations, its pads and declared supplies
/// (`plan_blocks` with the live bars), printing each plan's report without its couplings. `only`
/// declares those nets alone instead of the board's. It learns the run's refusals (`never_cut`) and
/// the chains it lands (`land`) as `BlockPlanner` does. `ELEC_LAND` lands entries from the first
/// plan, separated by `;`. Each entry is `born>to` or just `born` for `born>born`.
/// `ELEC_BAND=<Hz>` sets the top of the band its accuracy bars judge; without it, the drum rig
/// judges the band the host plans a heard run in (`heard_band`), and every point when its run has
/// no sample rate.
#[derive(Clone)]
struct Declared {
    fmax: f64,
    band: f64,
    never_cut: Vec<String>,
    land: Vec<Land>,
    round: f64,
    only: Option<Vec<String>>,
    handovers: Option<Arc<Mutex<Vec<f64>>>>,
    states: fab_elec_sim::analysis::states::StateCache,
    pool: Option<fab_elec_sim::rounds::Helpers>,
    /// board-acc's candidate under `ELEC_CARRY`: its first plan the cut kept for its circuit
    /// (`round::carry_plan`) when it is taken, as a heard start at new part values begins.
    carry: bool,
}

impl Declared {
    /// The planner's bars (`plan_blocks` with the live ones).
    fn opts(&self) -> fab_elec_sim::analysis::partition::PartOpts {
        fab_elec_sim::analysis::partition::PartOpts { never_cut: self.never_cut.clone(), land: self.land.clone(), cut_tol: fab_elec_sim::analysis::round::CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax: self.fmax, band: self.band, per_octave: 4, min_island: 8, control: Vec::new(), supplies: Vec::new(), heard: Vec::new(), round: self.round }
    }

    /// The circuit with `only` declared its supplies instead of the board's.
    fn declared<'a>(&self, ckt: &'a SimCircuit) -> std::borrow::Cow<'a, SimCircuit> {
        let Some(only) = &self.only else { return std::borrow::Cow::Borrowed(ckt) };
        let mut c = ckt.clone();
        for n in &mut c.nodes {
            n.supply = only.contains(&n.name);
        }
        std::borrow::Cow::Owned(c)
    }
}

/// The host's planner for `start` under `ELEC_PLAN_CHECK=1`: each plan it makes, the one made
/// behind a carried cut too, made again by a cold planner (no states kept, no pool) knowing the same
/// plan in force, and whether it is the same; and the cut a start carries, said as `Declared` says it.
#[derive(Clone)]
struct Checked(fab_elec_sim::analysis::round::BlockPlanner);

impl fab_elec_sim::rounds::Planner for Checked {
    fn plan(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64) -> Result<Option<(fab_elec_sim::rounds::RoundPlan, Vec<fab_elec_sim::Solver>)>, SimDiag> {
        self.plan_from(ckt, whole, t, None)
    }

    fn plan_from(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64, in_force: Option<&fab_elec_sim::rounds::RoundPlan>) -> Result<Option<(fab_elec_sim::rounds::RoundPlan, Vec<fab_elec_sim::Solver>)>, SimDiag> {
        let o = self.0.opts.clone();
        let plan = fab_elec_sim::rounds::Planner::plan_from(&mut self.0, ckt, whole, t, in_force)?;
        let cold = fab_elec_sim::analysis::round::plan_blocks_kept(ckt, whole, t, &o, &Default::default(), None, in_force)?;
        let what = |p: Option<&fab_elec_sim::rounds::RoundPlan>| p.map(|p| format!("{p:?}"));
        let same = if what(plan.as_ref().map(|p| &p.0)) == what(cold.as_ref().map(|p| &p.plan)) { "the same" } else { "DIFFERENT" };
        // against the cut in force: the same cut and lands, or the cuts it adds and drops
        let kept = match (in_force, &plan) {
            (None, _) => "on one stepper".to_owned(),
            (Some(_), None) => "in rounds, none".to_owned(),
            (Some(f), Some((p, _))) if f.same_cut(p) && f.lands == p.lands => "in rounds, the cut in force kept".to_owned(),
            (Some(f), Some((p, _))) => {
                let (added, dropped): (Vec<&String>, Vec<&String>) = (p.cuts.iter().filter(|c| !f.cuts.contains(c)).collect(), f.cuts.iter().filter(|c| !p.cuts.contains(c)).collect());
                let (landed, unlanded): (Vec<&(u32, u32)>, Vec<&(u32, u32)>) = (p.lands.iter().filter(|l| !f.lands.contains(l)).collect(), f.lands.iter().filter(|l| !p.lands.contains(l)).collect());
                format!("in rounds, a NEW cut: adds {added:?}, drops {dropped:?}; lands (coupling, reader) {landed:?} more, {unlanded:?} no longer, of {}", p.lands.len())
            }
        };
        println!("round plan at t = {t:.4} s ({kept}): a cold planner's plan: {same}; {}", plan.as_ref().map_or("none".to_owned(), |(p, _)| format!("cut at {}", p.cuts.join(", "))));
        Ok(plan)
    }

    fn refuse_cut(&mut self, node: &str) {
        fab_elec_sim::rounds::Planner::refuse_cut(&mut self.0, node)
    }

    fn land_chain(&mut self, land: &Land) {
        fab_elec_sim::rounds::Planner::land_chain(&mut self.0, land)
    }

    fn warm(&self, ckt: &Arc<SimCircuit>, whole: &fab_elec_sim::Solver, t: f64) -> Option<fab_elec_sim::rounds::Warm> {
        fab_elec_sim::rounds::Planner::warm(&self.0, ckt, whole, t)
    }

    fn can_carry(&self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver) -> bool {
        fab_elec_sim::rounds::Planner::can_carry(&self.0, ckt, whole)
    }

    fn carried(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64, from: Option<&Arc<fab_elec_sim::analysis::round::Carry>>) -> Option<Result<fab_elec_sim::rounds::CarriedPlan, fab_elec_sim::rounds::CarryRefused>> {
        let got = fab_elec_sim::rounds::Planner::carried(&mut self.0, ckt, whole, t, from);
        match &got {
            Some(Ok(c)) => println!("round plan at t = {t:.4} s, carried: {} blocks, {} couplings, {} chains landed by name, cut at {}", c.plan.blocks.len(), c.plan.couplings.len(), c.landed.len(), c.plan.cuts.join(", ")),
            Some(Err(r)) => println!("round plan at t = {t:.4} s, not carried: {}", r.why),
            None => {}
        }
        got
    }
}

impl fab_elec_sim::rounds::Planner for Declared {
    fn plan(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64) -> Result<Option<(fab_elec_sim::rounds::RoundPlan, Vec<fab_elec_sim::Solver>)>, SimDiag> {
        self.plan_from(ckt, whole, t, None)
    }

    fn can_carry(&self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver) -> bool {
        fab_elec_sim::analysis::round::can_carry(&self.declared(ckt), whole, &self.opts(), &self.states)
    }

    fn carried(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64, from: Option<&Arc<fab_elec_sim::analysis::round::Carry>>) -> Option<Result<fab_elec_sim::rounds::CarriedPlan, fab_elec_sim::rounds::CarryRefused>> {
        let got = fab_elec_sim::analysis::round::carry_plan(&self.declared(ckt), whole, t, &self.opts(), &self.states, self.pool.as_ref(), from);
        match &got {
            Some(Ok(c)) => {
                let plan = &c.plan;
                println!("round plan at t = {t:.4} s, carried: {} blocks, {} couplings, {} chains landed by name, cut at {}", plan.blocks.len(), plan.couplings.len(), c.landed.len(), plan.cuts.join(", "));
                let at = |cut: &str| ckt.devices.r.iter().filter(|r| [r.a, r.b].iter().any(|&n| ckt.nodes[n as usize].name == cut)).map(|r| format!("{} {:.3e}", r.name, r.r)).collect::<Vec<_>>();
                println!("  resistors at its cuts: {}", plan.cuts.iter().map(|c| format!("{c} [{}]", at(c).join(", "))).collect::<Vec<_>>().join("; "));
            }
            Some(Err(r)) => println!("round plan at t = {t:.4} s, not carried: {}", r.why),
            None => {}
        }
        got
    }

    fn plan_from(&mut self, ckt: &SimCircuit, whole: &fab_elec_sim::Solver, t: f64, in_force: Option<&fab_elec_sim::rounds::RoundPlan>) -> Result<Option<(fab_elec_sim::rounds::RoundPlan, Vec<fab_elec_sim::Solver>)>, SimDiag> {
        if std::mem::take(&mut self.carry) && in_force.is_none() {
            match fab_elec_sim::rounds::Planner::carried(self, ckt, whole, t, None) {
                Some(Ok(c)) => {
                    for land in c.landed {
                        if !self.land.contains(&land) {
                            self.land.push(land);
                        }
                    }
                    return Ok(Some((c.plan, c.solvers)));
                }
                Some(Err(_)) => {}
                None => println!("round plan at t = {t:.4} s, not carried: no cut kept for its circuit"),
            }
        }
        let o = self.opts();
        let ckt = &*self.declared(ckt);
        let plan = fab_elec_sim::analysis::round::plan_blocks_kept(ckt, whole, t, &o, &self.states, self.pool.as_ref(), in_force)?;
        if let Some(p) = &plan {
            self.land.retain(|land| !p.relay.absent.contains(&land.born));
        }
        // `ELEC_PLAN_CHECK=1`: each plan made again by a cold planner (no states kept, no pool), and
        // whether it is the same: the plan, its round map past its time, the report past its
        // timings and counts, and the held search's islands, merges and cuts with their figures
        if std::env::var_os("ELEC_PLAN_CHECK").is_some() {
            use fab_elec_sim::analysis::round::{plan_blocks_kept, BlockPlan, RoundMap};
            let cold = plan_blocks_kept(ckt, whole, t, &o, &Default::default(), None, in_force)?;
            let what = |p: &Option<BlockPlan>| p.as_ref().map(|p| {
                let h = &p.held;
                (format!("{:?}\n{:?}", p.plan, RoundMap { ms: 0.0, key_maps: 0, kept: 0, ..p.map.clone() }), p.report.lines().skip(2).collect::<Vec<_>>().join("\n"), format!("{:?}\n{:?}\n{:?}", h.islands, h.merges, h.nodes))
            });
            println!("  a cold planner's plan: {}", if what(&plan) == what(&cold) { "the same" } else { "DIFFERENT" });
            // and its cut built again from what it keeps for other part values (`build_carried`),
            // here at its own: the same plan but for its proof, as the carried plan's present map
            // is judged on both bars where the plan's guarded it
            if let Some((p, carry)) = plan.as_ref().and_then(|p| Some((p, p.carry.clone()?))) {
                use fab_elec_sim::analysis::round::{build_carried, Carried};
                use fab_elec_sim::rounds::RoundPlan;
                let bare = |p: &RoundPlan| format!("{:?}", RoundPlan { proof: None, late_tol: Vec::new(), hub_tol: None, ..p.clone() });
                let same = |q: &RoundPlan| if bare(q) == bare(&p.plan) { "the same" } else { "DIFFERENT" };
                let said = match build_carried(ckt, whole, t, &o, &carry, self.pool.as_ref())? {
                    Carried::Plan(q) => same(&q.plan).to_string(),
                    Carried::Refused(q, why) => format!("{}, refused: {why}", same(&q)),
                    Carried::Unbuilt(why) => format!("UNBUILT: {why}"),
                };
                println!("  the plan built again from its carry: {said}");
            }
        }
        if let (Some(times), Some(_)) = (&self.handovers, &plan) {
            times.lock().expect("the handover times").push(t);
        }
        match &plan {
            Some(p) => {
                p.report.lines().filter(|l| !l.starts_with("  coupling")).for_each(|l| println!("{l}"));
                // its shape: what a changed cut direction or stage moves without an act or a sound
                // (`ELEC_SHAPE=<this line>` pins it: a plan of another shape fails the run at its end)
                let h = &p.held;
                let stages = p.plan.blocks.iter().map(|b| b.stage).max().map_or(0, |s| s + 1);
                let first: Vec<&str> = h.nodes.iter().filter(|n| !n.hub && !n.stiff_first && n.consumers.iter().all(|c| c.rc)).map(|n| h.names[n.node as usize].as_str()).collect();
                let shape = format!("{} blocks, {stages} stages, unpipelined {:?}, consumers first [{}]", p.plan.blocks.len(), p.plan.unpipelined, first.join(", "));
                println!("  shape: {shape}");
                if std::env::var_os("ELEC_SHAPE").is_some_and(|pin| pin.to_str() != Some(shape.as_str())) {
                    println!("  the plan is off the pinned shape (ELEC_SHAPE)");
                    OFF_SHAPE.fetch_add(1, Ordering::Relaxed);
                }
            }
            None => println!("round plan at t = {t:.4} s: none, the whole circuit on one stepper"),
        }
        Ok(plan.map(|p| (p.plan, p.solvers)))
    }

    fn refuse_cut(&mut self, node: &str) {
        if !self.never_cut.iter().any(|n| n == node) {
            self.never_cut.push(node.to_string());
        }
    }

    fn land_chain(&mut self, land: &Land) {
        if !self.land.contains(land) {
            self.land.push(land.clone());
        }
    }
}

/// The planner's kept states for a drum run: with `ELEC_TWICE`, one cache for every run in the
/// process, as the host keeps one for every run it starts; else each run its own.
fn plan_states() -> fab_elec_sim::analysis::states::StateCache {
    static SHARED: std::sync::OnceLock<fab_elec_sim::analysis::states::StateCache> = std::sync::OnceLock::new();
    if std::env::var_os("ELEC_TWICE").is_none() && std::env::var_os("ELEC_CARRY").is_none() {
        return Default::default();
    }
    SHARED.get_or_init(Default::default).clone()
}

/// `ELEC_SINGLE=<grade,..>`: the single pass grades to run each case at, in order (`None`, the
/// default, when unset).
fn single_grades() -> Vec<Option<u8>> {
    match std::env::var("ELEC_SINGLE") {
        Ok(list) => list.split(',').map(|g| Some(g.trim().parse().expect("ELEC_SINGLE lists grades 0, 1 or 2"))).collect(),
        Err(_) => vec![None],
    }
}

/// The nets the islands' border view makes the border of drum16's board (I1).
const DRUM_BORDER: [&str; 5] = ["VREF", "VA", "VBUS", "SUM", "ACCENT"];

/// drum16's whole board, its `board` bench, lowered, and its MCU running the sequencer firmware
/// with its three shift registers, to attach.
fn drum_board() -> (fab_elec_bench::Lowered, CoSimDevice) {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
    let mut lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    assert!(lowered.diags.is_empty(), "the board lowers clean: {:?}", lowered.diags);
    // Fast's LU plans compile on the host's pool, as the host's lowerings do (`accept_lowered`)
    lowered.circuit.opts.plan_jit = app_pool().plan_jit();
    let device = drum_device(&lowered);
    (lowered, device)
}

/// drum16's MCU for a lowering of its board: the sequencer firmware built for it, with its three
/// shift registers, to attach (a host's maker makes one for each run, as its glue does).
fn drum_device(lowered: &fab_elec_bench::Lowered) -> CoSimDevice {
    let fw = &lowered.firmware[0];
    let params: Vec<(&str, f64)> = fw.params.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let image = fab_elec_mcu_emit_avr::firmware(DRUM_FW, fw.f_clk.expect("a clock"), &params).unwrap_or_else(|d| panic!("the firmware builds: {d:?}"));
    let pins = lowered.cosim_pins(&fw.part);
    let mut avr = AvrMachine::new(&image.words, u64::from(image.f_clk));
    match std::env::var("ELEC_PATTERN").as_deref() {
        Err(_) => {}
        // the firmware's saved pattern: a byte a step of its 8 tracks of 16, then its saved mark
        Ok("full") => {
            let mut bytes = [0xff_u8; 1024];
            bytes[..128].fill(1);
            bytes[128] = 165;
            avr.set_eeprom(&bytes);
        }
        Ok(other) => panic!("ELEC_PATTERN: `full` is the one pattern, not `{other}`"),
    }
    let machine = LogicMachine::new(avr, fw.logic.plan()).expect("the registers join the MCU");
    let device = CoMachine::new(machine, Config { stretch: 4000, debug: None }, &pins).expect("the co-simulated MCU");
    CoSimDevice { dev: Box::new(device), pins }
}

/// The rig's threads for a host (`SimHost`): its workers and jobs on threads of their own, its
/// runs' packs, checks and plans on the app's task pool (`RunPool`), as the app's spawn gives them.
struct PoolSpawn;

impl fab_elec_simhost::Spawn for PoolSpawn {
    fn worker(&self, _name: &'static str, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn job(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(f);
    }
    fn helpers(&self) -> Option<fab_elec_sim::rounds::Helpers> {
        app_pool().helpers()
    }
    fn background(&self) -> Option<fab_elec_sim::rounds::Helpers> {
        app_pool().background()
    }
    fn checks(&self) -> Option<fab_elec_sim::rounds::CheckPost> {
        app_pool().checks()
    }
    fn heard(&self) -> Option<Box<dyn FnMut(bool) + Send>> {
        Some(app_pool().heard_priority())
    }
    fn plan_jit(&self) -> Option<fab_elec_sim::sparse::compiled::PlanJit> {
        app_pool().plan_jit()
    }
    fn pool_name(&self) -> String {
        app_pool().name()
    }
}

/// `host-start <secs> <wait s> [opened]`: drum16's board through the host as the app runs it (`SimHost` on
/// the app's task pool, its MCU made by a maker as the glue makes one, paced at real time), each
/// start timed from its Play to the first values past its preparation: cold; again after a Stop;
/// and after a part marked not fitted while stopped (`cva`, the analog rail's capacitor: a
/// structure edit no kept cut carries) and `wait` seconds more stopped; and after a knob dragged
/// while stopped, a step a frame for a second, and `wait` seconds more. Each runs `secs` before
/// its Stop; the last, playing, takes a second structure edit (a block's decoupling capacitor
/// not fitted), timed from its commit to the first values of the run that has it. With `opened`
/// the board is committed `wait` seconds before its first Play, as the app opens it.
fn host_start(secs: f64, wait: f64, opened: bool) {
    use fab_elec_simhost::SimHost;
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
    let design = Arc::new(ev.design.clone());
    let scope = bench.scope_id();
    let mut rewired = ev.design.clone();
    rewired.generation += 1;
    let unit = rewired.units.iter_mut().find(|u| u.id == scope).expect("the bench's board is a unit");
    // not fitted, as a person marks a part: the board keeps its ids, and the part lowers to nothing
    let cva = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|c| c.path == "cva").expect("drum16 has cva");
    cva.dnp = true;
    // a second structure edit, made while playing: a block's decoupling capacitor not fitted too
    let mut rewired2 = rewired.clone();
    rewired2.generation += 1;
    let unit = rewired2.units.iter_mut().find(|u| u.id == scope).expect("the bench's board is a unit");
    let dec = Arc::make_mut(&mut unit.circuit).comps.iter_mut().find(|c| c.path.ends_with(".dec")).expect("drum16 has a block's decoupling capacitor");
    dec.dnp = true;
    let dec = dec.path.clone();
    let rewired = Arc::new(rewired);
    let rewired2 = Arc::new(rewired2);
    let maker: fab_elec_simhost::host::CoSimMaker = Arc::new(|l: &fab_elec_bench::Lowered, _member: u32| Some(drum_device(l)));
    let mut host = SimHost::with_spawn(Box::new(PoolSpawn));
    host.set_live_spec(LiveSpec { rate: 1.0, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.set_cosim(Some((maker, 1)));
    let t0 = Instant::now();
    let mut live = fab_elec_view::live::sim::SimLive::default();
    // a frame as the app's: the design committed, the host stepped, then the frame's wait
    let mut frames = |host: &mut SimHost, design: &Arc<_>, secs: f64, done: &mut dyn FnMut(&SimHost) -> bool| {
        let from = Instant::now();
        loop {
            host.commit(design);
            host.step(t0.elapsed().as_secs_f64(), &mut live);
            if done(host) || from.elapsed().as_secs_f64() >= secs {
                return from.elapsed().as_secs_f64();
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    };
    for (name, design) in [("cold", &design), ("again", &design), ("rewired", &rewired), ("knob", &rewired)] {
        // `opened`: the board committed and `wait` seconds on its frames before its first Play,
        // as the app opens a board and a person presses Play
        if name == "cold" && opened {
            frames(&mut host, design, wait, &mut |_| false);
        }
        if name == "rewired" {
            frames(&mut host, design, wait, &mut |_| false);
        }
        // a knob dragged while stopped, a step a frame for a second, then `wait` seconds more
        if name == "knob" {
            let (knob, place) = *host.index().knobs().first().expect("drum16 has knobs");
            for step in 1..=40 {
                host.command(&fab_elec_bench::SimCmd::SetParam { comp: knob, value: (place + 0.005 * f64::from(step)).clamp(0.0, 1.0) });
                frames(&mut host, design, 0.025, &mut |_| false);
            }
            frames(&mut host, design, wait, &mut |_| false);
        }
        host.command(&fab_elec_bench::SimCmd::Play);
        let mut prepared = false;
        let wall = frames(&mut host, design, 60.0, &mut |h| {
            let Some(v) = h.values().filter(|v| v.generation == h.index().generation) else { return false };
            prepared |= v.preparing;
            v.t > 0.0 && !v.preparing
        });
        println!("host start {name}: started {wall:.3} s after Play ({})", if prepared { "prepared" } else { "never preparing" });
        frames(&mut host, design, secs, &mut |_| false);
        if name == "knob" {
            // the structure edit made while playing: from its commit to the first values of the
            // run that has it, past its preparation (the run before it heard until then)
            // the run with it: on a lowering other than the one playing
            let old = host.index().generation;
            println!("host start live: {dec} not fitted, committed while playing at t {:.3} s", host.values().map_or(0.0, |v| v.t));
            let mut prepared = false;
            let wall = frames(&mut host, &rewired2, 60.0, &mut |h| {
                let g = h.index().generation;
                let Some(v) = h.values().filter(|v| g != old && v.generation == g) else { return false };
                prepared |= v.preparing;
                v.t > 0.0 && !v.preparing
            });
            println!("host start live: {dec} not fitted while playing, its run started {wall:.3} s after the commit ({})", if prepared { "prepared" } else { "never preparing" });
            frames(&mut host, &rewired2, secs, &mut |_| false);
        }
        host.command(&fab_elec_bench::SimCmd::Stop);
        frames(&mut host, design, 0.5, &mut |_| false);
    }
}

/// `host-edit <secs> <watch s> [<base>>]<parts> [live|stopped]`: drum16's board through the host
/// as `host-start` runs it (with `base`'s parts not fitted from the start), with one structure
/// edit: `parts` (parts' paths joined by `+`, `.dec` the first block's decoupling capacitor)
/// marked not fitted, either while playing (`live`, the
/// default: from its commit to the first values of the run that has it, and the longest the run's
/// time stood still from the commit to a second after that) or while stopped, 3 s before a Play
/// (`stopped`). The run with the edit plays `watch` seconds; the worker's own lines say its speed.
fn host_edit(secs: f64, watch: f64, part: &str, live_edit: bool) {
    use fab_elec_simhost::SimHost;
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
    let scope = bench.scope_id();
    let (base, part) = part.split_once('>').unwrap_or(("", part));
    // `parts` not fitted on the board: their paths
    macro_rules! not_fitted {
        ($design:expr, $parts:expr) => {{
            let unit = $design.units.iter_mut().find(|u| u.id == scope).expect("the bench's board is a unit");
            let circuit = Arc::make_mut(&mut unit.circuit);
            $parts.split('+').filter(|p| !p.is_empty()).map(|part| {
                let comp = circuit.comps.iter_mut()
                    .find(|c| if part.starts_with('.') { c.path.ends_with(part) } else { c.path == part })
                    .unwrap_or_else(|| panic!("drum16 has no part `{part}`"));
                comp.dnp = true;
                comp.path.clone()
            }).collect::<Vec<String>>()
        }};
    }
    let mut board = ev.design.clone();
    let based = not_fitted!(board, base);
    if !based.is_empty() {
        println!("host edit: the board with {} not fitted", based.join(" and "));
    }
    let mut edited = board.clone();
    edited.generation += 1;
    let path = not_fitted!(edited, part).join(" and ");
    let design = Arc::new(board);
    let edited = Arc::new(edited);
    let maker: fab_elec_simhost::host::CoSimMaker = Arc::new(|l: &fab_elec_bench::Lowered, _member: u32| Some(drum_device(l)));
    let mut host = SimHost::with_spawn(Box::new(PoolSpawn));
    host.set_live_spec(LiveSpec { rate: 1.0, ..LiveSpec::default() });
    host.set_bench(bench.spec.clone());
    host.set_scope(Some(bench.scope.clone()));
    host.set_cosim(Some((maker, 1)));
    let t0 = Instant::now();
    let mut live = fab_elec_view::live::sim::SimLive::default();
    let mut frames = |host: &mut SimHost, design: &Arc<_>, secs: f64, done: &mut dyn FnMut(&SimHost) -> bool| {
        let from = Instant::now();
        loop {
            host.commit(design);
            host.step(t0.elapsed().as_secs_f64(), &mut live);
            if done(host) || from.elapsed().as_secs_f64() >= secs {
                return from.elapsed().as_secs_f64();
            }
            std::thread::sleep(Duration::from_millis(4));
        }
    };
    // the run started on a lowering other than `old`'s, past its preparation
    let started = |old: u64| move |h: &SimHost| {
        let g = h.index().generation;
        g != old && h.values().is_some_and(|v| v.generation == g && v.t > 0.0 && !v.preparing)
    };
    host.command(&fab_elec_bench::SimCmd::Play);
    let wall = frames(&mut host, &design, 60.0, &mut started(u64::MAX));
    println!("host edit: the board started {wall:.3} s after Play");
    frames(&mut host, &design, secs, &mut |_| false);
    let old = host.index().generation;
    if !live_edit {
        host.command(&fab_elec_bench::SimCmd::Stop);
        frames(&mut host, &edited, 3.0, &mut |_| false);
        host.command(&fab_elec_bench::SimCmd::Play);
    }
    // the run's time as the frames see it: the longest wall it stood still, and where
    let (mut last_t, mut moved_at, mut still) = (host.values().map_or(0.0, |v| v.t), Instant::now(), (0.0_f64, 0.0_f64));
    let commit = Instant::now();
    let mut watch_time = |h: &SimHost| {
        if let Some(v) = h.values() {
            if v.t > last_t {
                let stood = moved_at.elapsed().as_secs_f64();
                if stood > still.0 {
                    still = (stood, moved_at.duration_since(commit).as_secs_f64());
                }
                (last_t, moved_at) = (v.t, Instant::now());
            }
        }
    };
    let is_started = started(old);
    let wall = frames(&mut host, &edited, 60.0, &mut |h| {
        watch_time(h);
        is_started(h)
    });
    if live_edit {
        frames(&mut host, &edited, 1.0, &mut |h| {
            watch_time(h);
            false
        });
    }
    println!("host edit: {path} not fitted {}, its run started {wall:.3} s after {}{}", if live_edit { "while playing" } else { "while stopped" }, if live_edit { "the commit" } else { "Play" },
        if live_edit { format!("; the run's time stood still for {:.3} s at most, from {:.3} s after the commit", still.0, still.1) } else { String::new() });
    frames(&mut host, &edited, watch, &mut |_| false);
    host.command(&fab_elec_bench::SimCmd::Stop);
    frames(&mut host, &edited, 0.5, &mut |_| false);
}

/// How a `board-edit` run takes the resistor's new value.
#[derive(Clone, Copy)]
enum Takes {
    /// Never: the board as it stands.
    Never,
    /// On its timeline from the start (`LiveCmd::At`), as a run started with it.
    Timeline,
    /// As the host sends a value edit heard while the recording plays: one `LiveCmd::Edit` at its
    /// time, sent once the run is this far past it.
    Behind(f64),
}

/// The `board-edit` recorder's tap and slot (the host's jack recorder has its own).
const JACK: u64 = 1;

/// What a `board-edit` run made: its jack recorder's take of LINE at the run's rate (and the
/// take's number), the worker's statistics and the run's clean stretch.
struct Made {
    take: Vec<f32>,
    number: u32,
    rate: f64,
    stats: LiveStats,
    clean: f64,
}

/// drum16's board through `run_live` and its channels to `secs`, as the host runs it: unpaced,
/// at its audio rate, keeping snapshots two loop windows deep, a recorder taping LINE as the
/// host's jack recorder does. The value `set` goes in at `at` as `takes` says.
fn drum_edit_run(secs: f64, res: f64, set: &LiveCmd, at: f64, takes: Takes) -> Made {
    let (board, device) = drum_board();
    let sample_rate = drum_rate(false);
    let snapshots = Snapshots { span: 2.0 * LOOP_SECONDS, ..Snapshots::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, trace: false, res, sample_rate, snapshots, ..LiveSpec::default() };
    let mut sim = LiveSim::new(board.circuit.clone(), board.plan.clone(), board.index.generation, spec).unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
    sim.attach(Some(device));
    // `ELEC_HELPERS=host`: in rounds as the host runs it (`rounds_pool`)
    let run = rounds_pool(false);
    let band = run.map_or(f64::INFINITY, |_| heard_band(sample_rate));
    if let Some(run) = run {
        host_rounds(&mut sim, run.helpers(), run.background(), run.checks(), Default::default(), band).unwrap_or_else(|d| panic!("the round plan installs: {d}"));
    }
    println!("  {}", setup_line(&sim, band, &app_pool().name()));
    let (tx, rx) = sync_channel(64);
    let values: Latest<LiveValues> = Latest::new();
    let mut publisher = TracePublisher::new(Arc::new(Latest::new()));
    let (sink_tx, mut sinks) = SinkSet::new();
    let content = Arc::new(RunContent::new(1, None, sample_rate));
    let bus = ListenBus::new(1 << 14, Arc::new(RunSlot::default()));
    bus.runs.set(Some(content.clone()));
    let voice = TapVoice { net: "LINE".into(), reference: None, gain: 1.0 };
    let tap = TapSpec { voices: vec![voice], latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(1) };
    let tap = AudioTap::recorder(tap, bus.ring.clone(), bus.record(JACK, JACK, LOOP_SECONDS));
    assert!(sink_tx.try_send(SinkCmd::Add(JACK, Box::new(tap))).is_ok(), "the recorder goes in");
    let (woke_tx, woke) = channel();
    let wake = move || {
        let _ = woke_tx.send(());
    };
    let past = |t: f64| values.latest().is_some_and(|(_, v)| v.t >= t);
    let wait = |t: f64| {
        while !past(t) {
            let _ = woke.recv_timeout(Duration::from_millis(20));
        }
    };
    let (values_out, publisher_out, sinks_in, wake) = (&values, &mut publisher, &mut sinks, &wake);
    let sim = std::thread::scope(|s| {
        let worker = s.spawn(move || run_live(sim, &rx, values_out, publisher_out, sinks_in, wake));
        if let Takes::Timeline = takes {
            tx.send(LiveCmd::At(at, Box::new(set.clone()))).expect("the worker takes commands");
        }
        tx.send(LiveCmd::Play).expect("the worker takes commands");
        if let Takes::Behind(by) = takes {
            wait(at + by);
            tx.send(LiveCmd::Edit(vec![(at, set.clone())])).expect("the worker takes commands");
        }
        wait(secs);
        tx.send(LiveCmd::Stop).expect("the worker takes commands");
        worker.join().expect("the worker ends").unwrap_or_else(|d| panic!("the board steps: {d}"))
    });
    settle_tapes();
    let tape = bus.tapes.current(JACK).expect("the run is taped");
    let mut take = vec![0.0f32; tape.frames() as usize];
    let n = tape.read(0, &mut take).expect("the tape reads back");
    take.truncate(n);
    Made { take, number: tape.take.take, rate: sample_rate, stats: sim.stats().clone(), clean: content.clean_until() }
}

/// Rates in the eight half-second wall windows from the start. The publications on either
/// side give the simulated time at each boundary by linear interpolation. A short play can
/// end inside a window; only complete windows have a rate.
/// 6f's figures over the first 4 s of `points` (host wall, simulated time) from the start: the
/// slowest 0.1 s window's rate and how many of the 40 are under x1.0, and the most the run fell
/// behind its 1x clock from its first point, which a listener's ring outlasts while it is under
/// the ring's lead.
fn start_fine(points: &[(f64, f64)]) -> String {
    let Some(&(wall0, t0)) = points.first() else { return "n/a".into() };
    let at = |wall: f64| {
        let k = points.partition_point(|p| p.0 < wall);
        let (b, tb) = *points.get(k)?;
        let (a, ta) = points[k.max(1) - 1];
        Some(if b > a { ta + (tb - ta) * ((wall - a) / (b - a)) } else { tb })
    };
    let rates: Vec<f64> = (0..40).map_while(|i| Some((at(wall0 + 0.1 * f64::from(i + 1))? - at(wall0 + 0.1 * f64::from(i))?) / 0.1)).collect();
    let slowest = rates.iter().copied().fold(f64::INFINITY, f64::min);
    let behind = points.iter().filter(|p| p.0 <= wall0 + 4.0).map(|&(w, t)| (w - wall0) - (t - t0)).fold(0.0, f64::max);
    format!("slowest 0.1 s window x{slowest:.4}, {} of {} under x1.0, at most {:.1} ms behind its clock", rates.iter().filter(|&&r| r < 1.0).count(), rates.len(), behind * 1e3)
}

fn start_windows(points: &[(f64, f64)]) -> String {
    let mut rates = Vec::with_capacity(8);
    if let Some(&(wall0, mut from)) = points.first() {
        for i in 1..=8 {
            let wall = wall0 + 0.5 * f64::from(i);
            let k = points.partition_point(|p| p.0 < wall);
            let Some(&(b, tb)) = points.get(k) else { break };
            let (a, ta) = points[k - 1];
            let t = ta + (tb - ta) * ((wall - a) / (b - a));
            rates.push(format!("{:.4}", (t - from) / 0.5));
            from = t;
        }
    }
    rates.resize(8, "n/a".to_string());
    rates.join(", ")
}

/// The host side of one `start` play. Time starts when Play goes on the command channel. The
/// worker makes its operating point and runs the ordinary live loop. Each wake drains every
/// kept publication so the one that says started survives even a late host. All measurements
/// use the host clock. Returns the prepared wall and the choice at that publication, if seen.
/// Folds the run's points through `to` (`fold`) where the host reads them after the run: a heard
/// run's end follows the host's Stop, so only the points both of two runs reach compare.
struct Digest {
    to: f64,
    h: (u64, u64),
    out: Arc<Mutex<Vec<(f64, u64, u64)>>>,
    next: usize,
    closed: bool,
}

/// The digest's marks: where a run made again first parts from the run it follows.
const DIGEST_EVERY: f64 = 0.05;

impl Sink for Digest {
    fn point(&mut self, at: f64, _outer: Option<f64>, x: &[f64]) {
        // the last mark, the whole digest through `to`, at the first point past it
        if at > self.to {
            if !std::mem::replace(&mut self.closed, true) {
                self.out.lock().expect("the digest's marks").push((self.to, self.h.0, self.h.1));
            }
            return;
        }
        while at >= self.next as f64 * DIGEST_EVERY {
            self.out.lock().expect("the digest's marks").push((self.next as f64 * DIGEST_EVERY, self.h.0, self.h.1));
            self.next += 1;
        }
        fold(&mut self.h, at, x);
    }
}

/// A heard start's timeline (`LiveSim::follow`): its timed commands, go-in rounds, refusals,
/// landings and the carry it tried, owned so a run made again on another thread takes them.
struct Followed {
    timeline: Vec<(f64, LiveCmd)>,
    go_ins: Vec<(f64, f64)>,
    refusals: Vec<(f64, Vec<String>)>,
    landings: Vec<(f64, fab_elec_sim::analysis::partition::Land)>,
    carried: Option<Arc<fab_elec_sim::analysis::round::Carry>>,
}

impl Followed {
    fn of(sim: &LiveSim) -> Self {
        Followed { timeline: sim.timeline().to_vec(), go_ins: sim.go_ins(), refusals: sim.refusals().to_vec(), landings: sim.landings().to_vec(), carried: sim.carried_cut().cloned() }
    }
}

/// With `digest`, the run's points through `secs` fold into it (`Digest`). Hands back the run.
/// `start`'s structure edit: a 10 Mohm resistor beside `part`, a part the circuit did not have, so
/// no kept cut carries to it.
fn rewire(c: &mut fab_elec_sim::SimCircuit, part: &str) {
    let r = c.devices.r.iter().find(|r| r.name == part).unwrap_or_else(|| panic!("drum16 has no resistor {part}")).clone();
    c.devices.r.push(fab_elec_sim::circuit::Res { name: format!("{part}.beside"), r: 10e6, ..r });
}

fn drum_start_play(name: &str, secs: f64, rate: f64, has_helpers: bool, ask_rounds: bool, digest: Option<&Arc<Mutex<Vec<(f64, u64, u64)>>>>,
    make_sim: impl FnOnce() -> Result<LiveSim, SimDiag> + Send) -> (Option<(f64, bool)>, LiveSim) {
    let values = Latest::<LiveValues>::new();
    values.keep_all(true);
    let bus = ListenBus::new(1 << 14, Arc::new(RunSlot::default()));
    bus.runs.set(Some(Arc::new(RunContent::new(1, None, rate))));
    let [tap] = board_taps(&bus, LOOP_SECONDS, [("LINE", JACK)], 1.0);
    let (sink_tx, mut sinks) = SinkSet::new();
    assert!(sink_tx.try_send(SinkCmd::Add(JACK, Box::new(tap))).is_ok(), "the LINE recorder goes in before Play");
    if let Some(out) = digest {
        let d = Digest { to: secs, h: (0xcbf29ce484222325, 0), out: out.clone(), next: 0, closed: false };
        assert!(sink_tx.try_send(SinkCmd::Add(JACK + 1, Box::new(d))).is_ok(), "the digest goes in before Play");
    }
    let mut publisher = TracePublisher::new(Arc::new(Latest::new()));
    let mut started = None;
    let mut hold_from = None;
    let mut held_wall = 0.0;
    let mut last_setup = None;
    let mut sound = None;
    let mut points = Vec::new();
    // With no helpers there is no preparation and no started publication. Playback is whole
    // from Play in that case, so the recorder and the windows can still be measured.
    if !has_helpers {
        points.push((0.0, 0.0));
    }
    let sim = std::thread::scope(|s| {
        let (tx, rx) = sync_channel(64);
        let (signal, woke) = sync_channel(1);
        let values = &values;
        if ask_rounds {
            tx.send(LiveCmd::SetRounds(true)).expect("the repeated board takes its remembered choice");
        }
        let play = Instant::now();
        tx.send(LiveCmd::Play).expect("the worker takes Play");
        let worker = s.spawn(move || {
            let wake = || { let _ = signal.try_send(()); };
            // raised while heard, its worker and its packs, as the host's are
            let mut sim = make_sim()?;
            sim.set_heard_priority(Some(app_pool().heard_priority()));
            run_live(sim, &rx, values, &mut publisher, &mut sinks, &wake)
        });
        let mut kept = Vec::new();
        while woke.recv().is_ok() {
            values.take_kept(&mut kept);
            let wall = play.elapsed().as_secs_f64();
            let mut stop = false;
            for v in kept.drain(..) {
                if v.preparing && last_setup.is_none_or(|(_, t)| t != v.t) {
                    last_setup = Some((wall, v.t));
                }
                if v.preparing && v.held == Some(fab_elec_sim::HoldKind::Plan) {
                    hold_from.get_or_insert(wall);
                } else if let Some(from) = hold_from.take() {
                    held_wall += wall - from;
                }
                if v.started && started.is_none() {
                    started = Some((wall, v.t, v.in_rounds));
                    points.push((wall, v.t));
                } else if let (Some(first), Some(last)) = (points.first(), points.last()) {
                    if last.0 < first.0 + 4.0 {
                        points.push((wall, v.t));
                    }
                }
                // Set-up advanced t before the playback clock started. For a four-second
                // measurement keep the publication that closes the eighth wall window too.
                let windows_done = secs < 4.0 || points.first().is_none_or(|p| wall >= p.0 + 4.0);
                stop |= (v.t >= secs && windows_done) || v.steady || !v.running;
            }
            // The recorder exposes whole chunks. Its last queued LINE frame must lie past
            // set-up; counting from the tape's own origin also works if it taped the set-up.
            if let Some(&(_, setup_t)) = points.first().filter(|_| sound.is_none()) {
                let last_frame = bus.tapes.current(JACK).and_then(|tape| {
                    tape.promised().checked_sub(1).map(|i| tape.t0 + i as f64 / tape.rate)
                });
                if last_frame.is_some_and(|t| t > setup_t) {
                    sound = Some(play.elapsed().as_secs_f64());
                }
            }
            if stop {
                if let Some(from) = hold_from.take() {
                    held_wall += wall - from;
                }
                let _ = tx.send(LiveCmd::Stop);
                break;
            }
        }
        worker.join().expect("the drum worker ends").unwrap_or_else(|d| panic!("start {name}: {d}"))
    });
    assert!(sim.t() >= secs, "start {name}: the run rested at {} s before {secs} s", sim.t());
    let stats = sim.stats();
    let seconds = |v: Option<f64>| v.map_or_else(|| "n/a".to_owned(), |x| format!("{x:.3} s"));
    let prepared = seconds(started.map(|s| s.0));
    let choice = match started {
        Some((_, _, true)) => "in rounds",
        Some(_) => "whole",
        None if !has_helpers => "whole (from Play; no started publication)",
        None => "unobserved",
    };
    // off_rounds counts whole time only with an active planner. During set-up it is parked.
    // If it stayed parked, every simulated second after the start was stepped whole.
    let whole = points.first().map(|&(_, t)| if stats.plans == 0 { sim.t() - t } else { stats.off_rounds });
    let whole = whole.map_or_else(|| "n/a".into(), |t| {
        if t > 0.0 && t < 0.00001 { "<0.01 ms".into() } else { format!("{:.2} ms", t * 1e3) }
    });
    // BlockPlanner drops its report and its internal plan cost is not published. At the
    // set-up boundary t stands still until installation. Without that publication the last
    // preparing interval includes the tail of set-up too.
    let plan = match (started, last_setup) {
        _ if stats.plans == 0 => "no plan made".to_owned(),
        (Some((end, t, _)), Some((from, at))) => {
            let span = if at == t { "at the set-up boundary through start" } else { "from last observed set-up through start (includes stepping)" };
            format!("{:.1} ms {span} (host wall; planner cost unpublished)", (end - from) * 1e3)
        }
        (Some((end, _, _)), None) => format!("{:.1} ms Play through start (includes operating point and set-up; planner cost unpublished)", end * 1e3),
        _ => "planner cost unpublished; no observed set-up interval".to_owned(),
    };
    println!("  start {name}: plans {} made, {} kept (no handover), {} blocks kept by new cuts, go-in {:.3} ms longest, waited for {} times; starts carried {}, carries refused {}{}",
        stats.plans, stats.plans_kept, stats.blocks_kept, stats.install_max.as_secs_f64() * 1e3, stats.plan_waits, stats.starts_carried, stats.carries_refused,
        stats.carry_refusal.as_ref().map_or(String::new(), |w| format!(" (the last: {w})")));
    println!("start {name}: prepared {prepared} (held for its plan {held_wall:.3} s), started {choice}; first sound {} (first queued LINE chunk past set-up); x rate per 0.5 s window over the first 4 s [{}]; whole after the set-up {whole} simulated; plan line: {plan}",
        seconds(sound), start_windows(&points));
    println!("  start {name} pace: {}", start_fine(&points));
    (started.map(|(wall, _, rounds)| (wall, rounds)), sim)
}

/// Three fresh runs of drum16 on one set of helpers and one planner memory. The second run
/// takes the first run's choice for the same circuit. The edit changes the lowered resistor
/// before LiveSim is made, so the third run chooses afresh and can reuse matching reader tests.
fn drum_start(secs: f64) {
    use fab_elec_sim::analysis::states::StateCache;
    let run = rounds_pool(true);
    let states = StateCache::default();
    let rate = drum_rate(false);
    assert!(rate.is_finite() && rate >= 0.0, "ELEC_RATE must be finite and nonnegative");
    let edit = std::env::var("ELEC_EDIT").unwrap_or_else(|_| "hats.cdis,330000".into());
    let (part, ohms) = edit.split_once(',').expect("ELEC_EDIT takes a resistor name and ohms separated by a comma");
    let part = part.trim();
    let ohms: f64 = ohms.trim().parse().expect("ELEC_EDIT ohms must be a number");
    assert!(ohms.is_finite() && ohms > 0.0, "ELEC_EDIT ohms must be finite and positive");
    // ELEC_REPLAY: the edited start made again from its timeline in a fresh run, memory and
    // planner (`LiveSim::follow`, the carry it tried included), its points compared by digest
    let replay = std::env::var_os("ELEC_REPLAY").is_some() && run.is_some();
    // ELEC_REPLAY=again makes the again start again instead
    let replay_of = if std::env::var("ELEC_REPLAY").as_deref() == Ok("again") { "again" } else { "edited" };
    let mut walls = Vec::new();
    let mut repeat_rounds = false;
    let mut edited = None;
    // ELEC_STOPPED: the rewired start (a part added) warmed ahead as a stopped host warms it
    // (`warm_ahead_of`), its pads as the edited start's set-up left them
    let stopped = std::env::var_os("ELEC_STOPPED").is_some();
    let spec = || LiveSpec { rate: 1.0, heard: true, playing: false, trace: false, res: 100e-6, sample_rate: rate,
        snapshots: Snapshots { span: 2.0 * LOOP_SECONDS, ..Snapshots::default() }, ..LiveSpec::default() };
    for name in ["cold", "again", "edited", "replayed", "rewired"] {
        if name == "replayed" && edited.is_none() {
            continue;
        }
        let (mut board, device) = drum_board();
        let r = board.circuit.devices.r.iter_mut().find(|r| r.name == part).unwrap_or_else(|| panic!("drum16 has no resistor {part}"));
        assert_ne!(r.r, ohms, "ELEC_EDIT must change the value of {part}");
        if name == "edited" || (name == "replayed" && replay_of == "edited") {
            // A thermal resistor's lowered current source carries its noise density.
            let noise = format!("{part}#noise");
            if let Some(i) = board.circuit.devices.i.iter_mut().find(|i| i.name == noise) {
                if let fab_elec_sim::Wave::Noise { dt, seed, .. } = i.src.wave {
                    i.src = fab_elec_sim::circuit::thermal_noise_source(ohms, board.circuit.temp_c, dt, seed)
                        .expect("the lowered thermal source has a positive sample spacing");
                }
            }
            r.r = ohms;
        }
        if name == "rewired" {
            rewire(&mut board.circuit, part);
            if let Some(run) = run.filter(|_| stopped) {
                let (mut ahead, _) = drum_board();
                rewire(&mut ahead.circuit, part);
                let sim = LiveSim::new(ahead.circuit, ahead.plan, ahead.index.generation, spec()).expect("the rewired board sets up");
                let t0 = Instant::now();
                let planned = warm_ahead_of(sim, run.background(), states.clone(), heard_band(rate), Arc::new(std::sync::atomic::AtomicBool::new(false)));
                println!("start rewired: warmed while stopped in {:.3} s, its key {}", t0.elapsed().as_secs_f64(), if planned { "planned" } else { "not planned" });
            }
        }
        let digest = (replay && (name == replay_of || name == "replayed")).then(|| Arc::new(Mutex::new(Vec::new())));
        let follow = edited.as_ref().filter(|_| name == "replayed").map(|(f, _): &(Followed, _)| f);
        let fresh = StateCache::default();
        let states = if follow.is_some() { &fresh } else { &states };
        let (got, sim) = drum_start_play(name, secs, rate, run.is_some(), name == "again" && repeat_rounds, digest.as_ref(), || {
            let mut sim = LiveSim::new(board.circuit, board.plan, board.index.generation, spec())?;
            sim.attach(Some(device));
            let band = run.map_or(f64::INFINITY, |_| heard_band(rate));
            match run {
                Some(run) if std::env::var_os("ELEC_PLAN_CHECK").is_some() => {
                    host_pool(&mut sim, run.helpers(), run.background(), run.checks());
                    let _ = sim.set_rounds(false);
                    sim.set_memory(Some(states.clone()));
                    let planner = fab_elec_sim::analysis::round::BlockPlanner { pool: run.background(), states: states.clone(), ..fab_elec_sim::analysis::round::BlockPlanner::for_run(&sim, band) };
                    sim.set_planner(Some(Box::new(Checked(planner))))?;
                }
                Some(run) => host_rounds(&mut sim, run.helpers(), run.background(), run.checks(), states.clone(), band)?,
                None => {
                    sim.set_rounds(false)?;
                    sim.set_memory(Some(states.clone()));
                }
            }
            // the edited start's timeline, made again in this fresh run once it is set up
            if let Some(f) = follow {
                sim.follow(&f.timeline, &f.go_ins, &f.refusals, &f.landings, f.carried.as_ref());
            }
            println!("  {}", setup_line(&sim, band, &app_pool().name()));
            Ok(sim)
        });
        repeat_rounds = got.is_some_and(|(_, rounds)| rounds);
        walls.push(got.map_or_else(|| "n/a".to_owned(), |(wall, _)| format!("{wall:.3} s")));
        let Some(digest) = digest else { continue };
        let marks = std::mem::take(&mut *digest.lock().expect("the digest's marks"));
        match edited.take() {
            None => edited = Some((Followed::of(&sim), marks)),
            Some((f, first)) => {
                let (&(_, h, n), &(_, h0, n0)) = (marks.last().expect("the replay's digest"), first.last().expect("the first start's digest"));
                println!("start replayed: the {replay_of} start made again from its timeline ({} go-ins, {} refusals, {} landings, {} carry) in a fresh run, memory and planner: {} run, {n} points through {secs} s against {n0}, digest {h:016x} against {h0:016x}",
                    f.go_ins.len(), f.refusals.len(), f.landings.len(), if f.carried.is_some() { "a" } else { "no" }, if (h, n) == (h0, n0) { "the same" } else { "a DIFFERENT" });
                println!("  replayed: {} marks against {}", marks.len(), first.len());
                if let Some(at) = marks.iter().zip(&first).position(|(a, b)| a != b) {
                    let (t, _, n) = marks[at];
                    let (_, _, n0) = first[at];
                    println!("  replayed: parts by {t:.2} s ({n} points against {n0}; the same through {:.2} s)", at.checked_sub(1).map_or(0.0, |k| marks[k].0));
                    let again = Followed::of(&sim);
                    println!("  replayed: go-ins {:?} against {:?}", again.go_ins, f.go_ins);
                    println!("  replayed: landings {:?} against {:?}", again.landings.iter().map(|l| l.0).collect::<Vec<_>>(), f.landings.iter().map(|l| l.0).collect::<Vec<_>>());
                    println!("  replayed: refusals {:?} against {:?}", again.refusals, f.refusals);
                }
            }
        }
    }
    println!("start prepared: cold {}, again {}, edited {} ({part} -> {ohms} ohm), rewired {} (10 Mohm beside {part}{})", walls[0], walls[1], walls[2], walls[walls.len() - 1],
        if stopped { ", warmed while stopped" } else { "" });
}

/// The nets a hearing recorder of eight probes tapes beside the jack: the board bench's three and
/// the five-net border's.
const TAPE_PROBES: [&str; 8] = ["LINE", "KICK", "SNARE", "VREF", "VA", "VBUS", "SUM", "ACCENT"];

/// The held Listen's slot in the board-tape runs, and the jack's first channel alone's.
const HELD: u64 = 2;
const ALONE: u64 = 3;

/// The host delivers takes at the audio device's rate, in Hz.
const DELIVERED: f64 = 48_000.0;
/// Each board-acc net has its own recorder and uses its tap id as its shelf slot.
const TAKE_NETS: [(&str, u64); 2] = [("LINE", 4), ("NOISE", 5)];
const CONTROL_NETS: [(&str, u64); 2] = [("LINE", 6), ("NOISE", 7)];
/// The gain of a voltage probe in a drum16 take.
const TAKE_GAIN: f32 = 0.4;

/// One net's delivered frames before playback clipping, and the tap's read counts.
struct BoardTake {
    samples: Vec<f32>,
    counts: Option<ReadCounts>,
}

fn take_bus() -> ListenBus {
    let bus = ListenBus::new(1 << 14, Arc::new(RunSlot::default()));
    bus.runs.set(Some(Arc::new(RunContent::new(1, None, DELIVERED))));
    bus
}

/// Each net's recorder starts with the run, as the host's jack recorder does. The window ends at
/// the first whole tape chunk covering `secs`, so closing it writes the last partial chunk.
fn board_taps<const N: usize>(bus: &ListenBus, secs: f64, nets: [(&str, u64); N], gain: f32) -> [AudioTap; N] {
    let window_s = (secs * DELIVERED / CHUNK as f64).ceil() * CHUNK as f64 / DELIVERED;
    nets.map(|(net, id)| {
        let voice = TapVoice { net: net.into(), reference: None, gain };
        let spec = TapSpec { voices: vec![voice], latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(1) };
        AudioTap::recorder(spec, bus.ring.clone(), bus.record(id, id, window_s))
    })
}

/// Report the share of instants beside a free period once the read has reached an instant.
fn beside_share(c: ReadCounts) -> String {
    if c.instants == 0 { "n/a".into() } else { format!("{:.6}", c.beside_free as f64 / c.instants as f64) }
}

/// Describe the read that produced a tape, including its counts when it had a grid.
fn read_description(counts: Option<ReadCounts>) -> String {
    let Some(c) = counts else { return "read=period-averages".into() };
    format!("read=banded instants={} s={} beside_free={} taken_back={} inner_points={} off_grid={}",
        c.instants, beside_share(c), c.beside_free, c.taken_back, c.inner_points, c.off_grid)
}

/// Read each mono tape from frame zero through the run's end, omitting any chunk padding.
fn read_takes<const N: usize>(bus: &ListenBus, secs: f64, nets: [(&str, u64); N], taps: &[AudioTap; N], label: &str) -> [BoardTake; N] {
    settle_tapes();
    if let Some(why) = bus.tapes.error() {
        panic!("could not record the takes: {why}");
    }
    assert_eq!(bus.tapes.bounded.load(Ordering::Relaxed), 0, "a take reached the recorder's byte bound");
    let frames = (secs * DELIVERED).floor() as usize;
    std::array::from_fn(|i| {
        let (net, id) = nets[i];
        let mut samples = Vec::new();
        if let Some(tape) = bus.tapes.current(id) {
            if let Some(why) = tape.error() {
                panic!("could not record {net}: {why}");
            }
            samples.resize(frames.min(tape.frames() as usize), 0.0);
            let n = tape.read(0, &mut samples).expect("the tape reads back");
            samples.truncate(n);
        }
        let peak = samples.iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
        let counts = taps[i].read_counts();
        println!("  {label} {net}: frames={} peak={peak:.6} {}", samples.len(), read_description(counts));
        if peak >= 1.0 {
            println!("  WARNING: {label} {net} clipped. Spectrum windows containing clipped frames are void.");
        }
        BoardTake { samples, counts }
    })
}

/// drum16's board rendering a take nobody hears, as the host renders it (`run_live` unpaced at its
/// audio rate, the circuit whole, snapshots two loop windows deep), with the hearing recorder
/// taping the output jack's channels as the host's does (each against its reference at the Listen
/// gain shared by the channels) and each of `probes` against ground at the Listen gain, as it tapes
/// the bench's voltage probes: the worker's speed, the sinks' share of its stepping, and what was
/// taped. With `alone`, a recorder of the jack's first channel alone from the run's start too: a
/// Listen of that channel over the take at rest is bit for bit what it taped, after a resistor
/// edited behind the front sent the run back.
fn drum_tape(secs: f64, res: f64, probes: &[&str], alone: bool) {
    let (board, device) = drum_board();
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
    let circuit = &ev.design.unit(bench.scope_id()).expect("the board bench's unit").circuit;
    let sample_rate = drum_rate(false);
    let snapshots = Snapshots { span: 2.0 * LOOP_SECONDS, ..Snapshots::default() };
    let spec = LiveSpec { rate: f64::INFINITY, playing: false, res, sample_rate, snapshots, until: secs, ..LiveSpec::default() };
    let mut sim = LiveSim::new(board.circuit.clone(), board.plan.clone(), board.index.generation, spec).unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
    sim.attach(Some(device));
    // a net the run solves goes by its name; the ground net (no unknown) is the reference "0"
    let names: Vec<String> = sim.solver().unknowns.iter().map(|u| u.name.clone()).collect();
    let mut voices: Vec<TapVoice> = Vec::new();
    for jack in fab_elec_bench::audio::output_jacks(circuit) {
        let channels: Vec<_> = jack.channels().collect();
        let reference = circuit.nets.get(jack.reference.ix()).map(|n| n.name.clone()).filter(|n| names.contains(n));
        for &net in &channels {
            let net = circuit.nets.get(net.ix()).map(|n| n.name.clone()).filter(|n| names.contains(n)).unwrap_or_else(|| "0".to_owned());
            // as a Listen of the jack hears it, a net on both channels once (`jack_tap_voices`)
            let voice = TapVoice { net, reference: reference.clone(), gain: 0.4 / channels.len() as f32 };
            if !voices.contains(&voice) {
                voices.push(voice);
            }
        }
    }
    let jacks = voices.len();
    for &net in probes {
        if names.iter().any(|n| n == net) {
            voices.push(TapVoice { net: net.to_owned(), reference: None, gain: 0.4 });
        } else {
            println!("board-tape: {net} is no unknown of the board; not taped");
        }
    }
    let n = voices.len();
    let (tx, rx) = sync_channel(64);
    let values: Latest<LiveValues> = Latest::new();
    let mut publisher = TracePublisher::new(Arc::new(Latest::new()));
    let (sink_tx, mut sinks) = SinkSet::new();
    let bus = ListenBus::new(1 << 14, Arc::new(RunSlot::default()));
    bus.runs.set(Some(Arc::new(RunContent::new(1, None, sample_rate))));
    let tap_voices = voices.clone();
    let tap = TapSpec { voices, latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(n) };
    let tap = AudioTap::recorder(tap, bus.ring.clone(), bus.record(JACK, JACK, secs));
    assert!(sink_tx.try_send(SinkCmd::Add(JACK, Box::new(tap))).is_ok(), "the recorder goes in");
    if alone {
        let spec = TapSpec { voices: vec![tap_voices[0].clone()], latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(1) };
        let tap = AudioTap::recorder(spec, bus.ring.clone(), bus.record(ALONE, ALONE, secs));
        assert!(sink_tx.try_send(SinkCmd::Add(ALONE, Box::new(tap))).is_ok(), "the recorder alone goes in");
    }
    let (woke_tx, woke) = channel();
    let wake = move || {
        let _ = woke_tx.send(());
    };
    let (values_out, publisher_out, sinks_in, wake) = (&values, &mut publisher, &mut sinks, &wake);
    let first = tap_voices[0].clone();
    // with `alone`, a resistor edited behind the front while the take renders: the run goes back
    let edit = LiveCmd::SetParam { generation: board.index.generation, device: "r_hats".into(), key: "value".into(), value: 10e3 };
    let start = Instant::now();
    let (sim, wall, served) = std::thread::scope(|s| {
        let worker = s.spawn(move || run_live(sim, &rx, values_out, publisher_out, sinks_in, wake));
        tx.send(LiveCmd::Play).expect("the worker takes commands");
        if alone {
            while !values.latest().is_some_and(|(_, v)| v.t >= 0.6 * secs) {
                let _ = woke.recv_timeout(Duration::from_millis(20));
            }
            tx.send(LiveCmd::Edit(vec![(0.3 * secs, edit)])).expect("the worker takes commands");
        }
        while !values.latest().is_some_and(|(_, v)| v.t >= secs - 1e-9 && !v.running) {
            let _ = woke.recv_timeout(Duration::from_millis(20));
        }
        let wall = start.elapsed();
        // a Listen of the jack's first channel over the take at rest: from the hearing record, the
        // worker's rest loop serving it, with no run again; until its window is whole
        let held = TapSpec { voices: vec![first], latency: 0.06, dc_block_hz: 5.0, mix: TapMix::new(1) };
        // the player's queue, as large as the bus's
        let (chunks, heard) = sync_channel(1024);
        let mut held = AudioTap::new(held, bus.ring.clone()).recording(RecordSpec { chunks, ..bus.record(HELD, HELD, secs) });
        let asked = Instant::now();
        let served = held.play_recorded(&bus.tapes, JACK, true).map(|()| {
            assert!(sink_tx.try_send(SinkCmd::Add(HELD, Box::new(held))).is_ok(), "the held tap goes in");
            tx.send(LiveCmd::Sinks).expect("the worker takes commands");
            let want = window_frames(secs, sample_rate) / CHUNK;
            let mut got: Vec<f32> = Vec::new();
            while got.len() < want * CHUNK && asked.elapsed() < Duration::from_secs(60) {
                if let Ok(d) = heard.recv_timeout(Duration::from_millis(100)) {
                    got.extend_from_slice(&d.chunk);
                }
            }
            (got, want, asked.elapsed())
        });
        tx.send(LiveCmd::Stop).expect("the worker takes commands");
        (worker.join().expect("the worker ends").unwrap_or_else(|d| panic!("the board steps: {d}")), wall, served)
    });
    settle_tapes();
    let frames = bus.tapes.current(JACK).map_or(0, |tape| tape.frames());
    let st = sim.stats();
    let label = if res == 0.0 { "Exact".to_string() } else { format!("Fast res {:.0} us", res * 1e6) };
    println!("drum16 board, {label}, {n} voices taped ({jacks} jack, {} probes): {:.2} s simulated in {:.2} s wall: x{:.3}; sinks {:.1} ms, {:.2} % of the stepping ({:.1} ms), {:.2} % of the wall; {frames} frames taped, {} rewinds",
        n - jacks, sim.t(), wall.as_secs_f64(), sim.t() / wall.as_secs_f64(), ms(st.sink_wall), 100.0 * st.sink_wall.as_secs_f64() / st.step_wall.as_secs_f64().max(1e-9), ms(st.step_wall),
        100.0 * st.sink_wall.as_secs_f64() / wall.as_secs_f64(), st.rewinds);
    match served {
        Ok((got, want, took)) => {
            println!("  a Listen over the take at rest: {} of its {want} chunks from the hearing record in {:.1} ms", got.len() / CHUNK, ms(took));
            if let Some(tape) = bus.tapes.current(ALONE).filter(|_| alone) {
                let mut taped = vec![0.0f32; got.len()];
                let n = tape.read(0, &mut taped).expect("the recorder alone reads back");
                let differ = got.iter().zip(&taped[..n]).position(|(a, b)| a.to_bits() != b.to_bits());
                println!("  against the channel alone from the run's start ({n} frames): {}", differ.map_or("bit for bit".to_owned(), |k| format!("differs from frame {k}")));
            }
        }
        Err(why) => println!("  a Listen over the take at rest records it again: {why}"),
    }
}

/// A resistor's value edited while drum16 plays, as the host sends it when the recording plays:
/// one `LiveCmd::Edit` at the time heard, `behind` seconds before the run's front. Against the
/// same run with the value on its timeline from the start, and the board as it stands, each
/// taped as the host's jack recorder tapes it: whether the run went back and took the edit as one take, whether
/// its take is the timeline run's by bits (so the return and the re-render add nothing), where
/// it parts from the board as it stands (the edit heard from its time, never before), how loud
/// the change is, and the steepest sample step round the return and round the edit against the
/// board's own steepest.
fn drum_edit(secs: f64, ress: &[f64], part: &str, ohms: f64, at: f64, behind: f64) {
    let (board, _) = drum_board();
    let was = board.circuit.devices.r.iter().find(|r| r.name == part).map(|r| r.r).unwrap_or_else(|| {
        let near: Vec<&str> = board.circuit.devices.r.iter().map(|r| r.name.as_str()).filter(|n| n.contains(part) || part.contains(*n)).collect();
        panic!("drum16's board has no resistor {part} (near: {near:?})")
    });
    let set = LiveCmd::SetParam { generation: board.index.generation, device: part.into(), key: "value".into(), value: ohms };
    for &res in ress {
        let label = if res == 0.0 { "Exact".to_string() } else { format!("Fast res {:.0} us", res * 1e6) };
        let never = drum_edit_run(secs, res, &set, at, Takes::Never);
        let timeline = drum_edit_run(secs, res, &set, at, Takes::Timeline);
        let heard = drum_edit_run(secs, res, &set, at, Takes::Behind(behind));
        let rate = heard.rate;
        let n = never.take.len().min(timeline.take.len()).min(heard.take.len());
        let parted = |a: &[f32], b: &[f32]| a[..n].iter().zip(&b[..n]).position(|(x, y)| x.to_bits() != y.to_bits());
        let (same, from) = (parted(&heard.take, &timeline.take), parted(&heard.take, &never.take));
        let k = ((at * rate) as usize).min(n);
        let rms = |x: &[f32]| (x.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt();
        let db = |a: f64, b: f64| 20.0 * (a / b).log10();
        let diff: Vec<f32> = heard.take[k..n].iter().zip(&never.take[k..n]).map(|(a, b)| a - b).collect();
        let steep = |x: &[f32], t: f64| {
            let (a, b) = (((t - 0.005) * rate).max(0.0) as usize, (((t + 0.005) * rate) as usize).min(n));
            x[a.min(b)..b].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max)
        };
        let own = never.take[..n].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let st = &heard.stats;
        println!("board-edit {label}: {part} {was:.0} -> {ohms:.0} ohm at {at} s, sent {behind} s past it; {n} frames at {:.0} Hz", rate);
        println!("  the run went back {} time(s) (late edits {}), to {:.6} s (its clean stretch), {} snapshots taken, {} kept; one take: number {} (the board as it stands: {})",
            st.rewinds, st.late_edits, heard.clean, st.snapshots, st.snapshots_kept, heard.number, never.number);
        if st.plans > 0 {
            println!("  in rounds: {} plans ({} kept), {} edits left rounds, {:.1} ms simulated on one stepper with a planner; the run with the value on its timeline: {} plans ({} kept), {} left",
                st.plans, st.plans_kept, st.edit_leaves, st.off_rounds * 1e3, timeline.stats.plans, timeline.stats.plans_kept, timeline.stats.edit_leaves);
        }
        println!("  the take against the run with the value on its timeline: {}",
            same.map_or("the same by bits".to_string(), |i| format!("parts at frame {i} ({:.6} s)", i as f64 / rate)));
        println!("  against the board as it stands: {}; the rest {:+.2} dB, the change {:.1} dB below it",
            from.map_or("never parts".to_string(), |i| format!("parts from frame {i} ({:.6} s; the edit's period starts at frame {k})", i as f64 / rate)),
            db(rms(&heard.take[k..n]), rms(&never.take[k..n])), -db(rms(&diff), rms(&never.take[k..n])));
        println!("  steepest step: {:.3e} round the return, {:.3e} round the edit; the board's own {:.3e} (the run with the value on its timeline round the edit {:.3e})",
            steep(&heard.take, heard.clean), steep(&heard.take, at), own, steep(&timeline.take, at));
    }
}

/// drum16's whole board, its `board` bench: the MCU running the sequencer with its three shift
/// registers on the digital side, the key matrix, every voice, the mixer and the line out, the
/// live worker's loop unpaced in 8 ms slices for `secs` simulated seconds from zero. Prints the
/// realtime factor, the stepper's counts per simulated second, accepted steps by length and,
/// profiled, where the analog time goes. With a recording selection, returns its points and
/// resolved names instead of printing the timing report. Missing names are reported and skipped.
/// drum16's keys `pressed` in turn from `from` for `span` s, `rate` times a second each, held
/// `hold` s a press (the presses staggered across a period), as commands at their instants.
fn press_keys(sim: &mut LiveSim, index: &fab_elec_bench::ValueIndex, (from, span, rate, hold): (f64, f64, f64, f64), pressed: &[usize]) {
    use fab_elec_bench::SimCmd;
    let keys = index.momentary().to_vec();
    for j in 0..(span * rate).round() as usize {
        for (i, &k) in pressed.iter().enumerate() {
            let t = from + j as f64 / rate + i as f64 / (rate * pressed.len() as f64);
            for (at, down) in [(t, true), (t + hold, false)] {
                for c in index.command(&SimCmd::press(keys[k], down)) {
                    sim.apply(LiveCmd::At(at, Box::new(c)));
                }
            }
        }
    }
}

/// `ELEC_PRESSES=<from s>,<for s>,<presses a second>,<hold ms>,<key>/<key>..`: the keys drum()
/// presses (board-acc's tapped and held takes), as board-tap presses them; unset presses none.
fn presses_env() -> Option<((f64, f64, f64, f64), Vec<usize>)> {
    let v = std::env::var("ELEC_PRESSES").ok()?;
    let f: Vec<&str> = v.split(',').collect();
    let num = |k: usize| f.get(k).and_then(|v| v.parse::<f64>().ok()).unwrap_or_else(|| panic!("ELEC_PRESSES: field {k} is a number"));
    let keys = f.get(4).copied().unwrap_or("0/5/10").split('/').map(|k| k.parse().expect("ELEC_PRESSES: the keys are numbers")).collect();
    Some(((num(0), num(1), num(2), num(3) * 1e-3), keys))
}

fn drum(secs: f64, run: DrumRun<'_>) -> (Rec, Vec<String>) {
    let DrumRun { res, profile, reltol, tolerance_divisors, record, attrib, digest, tiers, bypass, single, snapshot, rounds, round, declared, handovers, reference, take, carry_source } = run;
    let profile = profile || attrib;
    let (lowered, device) = drum_board();
    let mut circuit = lowered.circuit.clone();
    circuit.opts.profile = profile;
    if let Some(on) = tiers {
        circuit.opts.tiers = on;
    }
    // `ELEC_BYPASS=0`: Fast evaluates every device at every iteration (`Options::bypass` off)
    if let Some(on) = bypass.or((std::env::var("ELEC_BYPASS").as_deref() == Ok("0")).then_some(false)) {
        circuit.opts.bypass = on;
    }
    if let Some(grade) = single {
        circuit.opts.single_pass = grade;
    }
    // `ELEC_BLOCKS=1` loads the board block by block, cut at the five-net border (ESIMRT C2)
    if std::env::var_os("ELEC_BLOCKS").is_some() {
        circuit.opts.border = DRUM_BORDER.iter().map(|s| s.to_string()).collect();
    }
    if let Some((voltage, current)) = tolerance_divisors {
        circuit.opts.vntol /= voltage;
        circuit.opts.abstol /= current;
    }
    let defaults = LiveSpec::default();
    // drum16 has an audio output: the host's default rate for it
    let sample_rate = drum_rate(reference);
    let mut max_step = defaults.max_step;
    // the step the plan is made at: the shipping step (`LiveSim::step_cap` at the default bound)
    // when `ELEC_STEPS_A_PERIOD` steps finer, the run's own step otherwise
    let mut plan_step = None;
    if res > 0.0 && sample_rate.is_finite() && sample_rate > 0.0 {
        if let Some(n) = std::env::var("ELEC_STEPS_A_PERIOD").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n >= 2) {
            max_step = max_step.min(1.0 / (sample_rate * n as f64));
            plan_step = Some(defaults.max_step.min(1.0 / sample_rate));
        }
    }
    let spec = LiveSpec {
        rate: f64::INFINITY, trace: false, res, reltol,
        sample_rate, max_step, round: round.unwrap_or(defaults.round),
        pipeline: std::env::var("ELEC_T4").as_deref() != Ok("0"), // A/B runs
        edge_restarts: std::env::var("ELEC_F3").as_deref() != Ok("0"),
        own_bound: std::env::var("ELEC_I3").as_deref() != Ok("0"),
        merge_job: std::env::var("ELEC_MJ").as_deref() != Ok("0"),
        // With `ELEC_HEARD=1` the run waits for its first plan and takes later ones when ready.
        heard: std::env::var_os("ELEC_HEARD").is_some(),
        ..defaults
    };
    let mut sim = LiveSim::new(circuit, lowered.plan.clone(), lowered.index.generation, spec).unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
    if let Some(plan) = sim.solver().blocks.as_ref() {
        print!("{}", plan.report());
    }
    let max_step = sim.step_cap();
    let plan_step = plan_step.unwrap_or(max_step);
    println!("  sample rate {:.1} kHz: steps of at most {:.2} us", sim.sample_rate() / 1e3, max_step * 1e6);
    if plan_step != max_step && rounds.is_some() {
        println!("  planned at the shipping step, {:.2} us: the rounds play the plan that ships", plan_step * 1e6);
    }
    sim.attach(Some(device));
    // `ELEC_HELPERS=host`: in rounds on the host's pool, a run's work where the host's live bench
    // puts it (`host_pool`); the planner's work goes to its background jobs too
    let run = rounds_pool(false);
    let plan_pool = run.and_then(|r| r.background());
    if let Some(run) = run {
        host_pool(&mut sim, run.helpers(), plan_pool.clone(), run.checks());
    }
    // with `ELEC_TWICE` the runs share the host's memory as well (`LiveSim::set_memory`): a second
    // run starts with what the first refused and landed, as the host's next run of the board does
    if std::env::var_os("ELEC_TWICE").is_some() {
        sim.set_memory(Some(plan_states()));
    }
    if let Some((window, pressed)) = presses_env() {
        println!("  keys {pressed:?} pressed from {} s for {} s, {} times a second, held {} s each", window.0, window.1, window.2, window.3);
        press_keys(&mut sim, &lowered.index, window, &pressed);
    }
    // `ELEC_TURN=<knob's start position>,<from s>,<to s>,<steps>,<position from>,<position to>[;..]`:
    // the board's knob that starts at that position (`ValueIndex::knobs`) turned from one position
    // to the other in even steps between the two times, as the panel turns it, at their instants;
    // each turn after a `;` as well. A step at 0 s is where the knob stands as the run begins: set
    // now, before the planner, as the host's board has it when it starts a run, so the run's memory
    // key (`LiveSim::set_memory`) is the one its runs remembered under
    if let Ok(v) = std::env::var("ELEC_TURN") {
        use fab_elec_bench::SimCmd;
        let knobs = lowered.index.knobs().to_vec();
        for turn in v.split(';').filter(|t| !t.is_empty()) {
            let f: Vec<f64> = turn.split(',').map(|x| x.parse().expect("ELEC_TURN: six numbers a turn")).collect();
            assert_eq!(f.len(), 6, "ELEC_TURN: six numbers a turn");
            let (comp, _) = *knobs.iter().find(|k| (k.1 - f[0]).abs() < 1e-6).unwrap_or_else(|| panic!("ELEC_TURN: no knob starts at {} (they start at {:?})", f[0], knobs.iter().map(|k| k.1).collect::<Vec<_>>()));
            let steps = f[3].max(1.0) as usize;
            for j in 0..=steps {
                let (share, at) = (j as f64 / steps as f64, f[1] + (f[2] - f[1]) * j as f64 / steps as f64);
                for c in lowered.index.command(&SimCmd::SetParam { comp, value: f[4] + (f[5] - f[4]) * share }) {
                    sim.apply(if at <= 0.0 { c } else { LiveCmd::At(at, Box::new(c)) });
                }
            }
            println!("  the knob at {} turned from {} to {} in {steps} steps from {} s to {} s", f[0], f[4], f[5], f[1], f[2]);
        }
    }
    // `ELEC_SET=<resistor>,<ohms>,<at s>[,<ohms>,<at s>..]`: the board's resistor set at each time, as
    // the host sends a value edit (`LiveCmd::SetParam`), at their instants
    if let Some(v) = std::env::var("ELEC_SET").ok().filter(|_| !carry_source) {
        let f: Vec<&str> = v.split(',').collect();
        assert!(f.len() >= 3 && f.len() % 2 == 1, "ELEC_SET: a device, then values and times");
        let num = |s: &str| s.parse::<f64>().unwrap_or_else(|_| panic!("ELEC_SET: {s} is a number"));
        if !lowered.circuit.devices.r.iter().any(|r| r.name == f[0]) {
            let near: Vec<&str> = lowered.circuit.devices.r.iter().map(|r| r.name.as_str()).filter(|n| n.contains(f[0]) || f[0].contains(*n)).collect();
            panic!("ELEC_SET: drum16's board has no resistor {} (near: {near:?})", f[0]);
        }
        for p in f[1..].chunks(2) {
            let set = LiveCmd::SetParam { generation: lowered.index.generation, device: f[0].into(), key: "value".into(), value: num(p[0]) };
            sim.apply(LiveCmd::At(num(p[1]), Box::new(set)));
        }
        println!("  {} set to {:?} at {:?} s", f[0], f[1..].iter().step_by(2).collect::<Vec<_>>(), f[2..].iter().step_by(2).collect::<Vec<_>>());
    }
    if attrib {
        use fab_elec_sim::analysis::attrib::Attribution;
        use fab_elec_sim::analysis::partition::{partition, PartOpts};
        let opts = PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: fab_elec_sim::analysis::round::CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax: 0.5 / max_step, band: f64::INFINITY, per_octave: 4, min_island: 8, control: Vec::new(), supplies: Vec::new(), heard: Vec::new(), round: 32.0 * max_step };
        let p = partition(&lowered.circuit, &opts).unwrap_or_else(|e| panic!("the board partitions: {}", e.message));
        let blocks: Vec<Vec<String>> = p.border_blocks(&DRUM_BORDER).expect("the border nets exist").iter()
            .map(|b| b.iter().map(|&u| p.names[u].clone()).collect()).collect();
        let border: Vec<String> = DRUM_BORDER.iter().map(|s| s.to_string()).collect();
        let a = Attribution::new(sim.solver(), &blocks, &border, &["VA", "VREF", "VBUS"]).unwrap_or_else(|e| panic!("{e}"));
        sim.profile_mut().expect("a profiled circuit").attrib = Some(Box::new(a));
    }
    let mut steps = Steps { max: max_step, last: None, hist: [0; 12] };
    let digest = (digest || std::env::var_os("ELEC_DIGEST").is_some()).then_some((0xcbf29ce484222325, 0));
    let (mut columns, mut names) = (Vec::new(), Vec::new());
    if let Some(wanted) = record {
        for name in wanted {
            if let Some(i) = sim.solver().unknowns.iter().position(|u| u.name == *name) {
                columns.push(i);
                names.push(name.clone());
            } else {
                println!("board-acc: unknown {name} not found; skipped");
            }
        }
    }
    let heard = sim.solver().unknown("LINE").into_iter().collect();
    let every = std::env::var("ELEC_DIGEST_EVERY").ok().and_then(|v| v.parse::<f64>().ok()).filter(|&e| e > 0.0).map(|e| (e, e));
    let dump = std::env::var("ELEC_DIGEST_DUMP").ok().and_then(|v| v.split_once(',').and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?))));
    let mut rec = Rec { on: true, keep: record.is_some(), columns: record.map(|_| columns), digest, heard, every, dump,
        reltol: sim.solver().opts.reltol, ..Rec::default() };
    let bus = take.then(take_bus);
    let mut both = Both(&mut steps, &mut rec);
    let mut taps = bus.as_ref().map(|bus| board_taps(bus, secs, TAKE_NETS, TAKE_GAIN));
    let mut control = bus.as_ref().filter(|_| !reference).map(|bus| board_taps(bus, secs, CONTROL_NETS, TAKE_GAIN));
    let mut recording = None;
    let sink: &mut dyn Sink = if let Some([line, noise]) = &mut taps {
        let s = recording.insert(TakeSink {
            sinks: [&mut both, line, noise], control: control.as_mut(), filter: ControlFilter::default(), clock: TakeClock::default(),
        });
        s.begin(&sim.solver().unknowns);
        s.sample_period(1.0 / sim.sample_rate());
        s
    } else {
        &mut both
    };
    let seen = Arc::new(Mutex::new(Seen::default()));
    let a = snap(&sim, &seen);
    // Accuracy runs have no wall-clock slice boundary; only simulated events control stepping.
    let budget = if record.is_some() { None } else { Some(Duration::from_millis(8)) };
    let start = Instant::now();
    let (mut taken, mut last) = (Vec::new(), None);
    let mut plan_at = rounds;
    // the host's band unless `ELEC_BAND` names another; every point for a run with no rate
    let heard = if sample_rate.is_finite() && sample_rate > 0.0 { heard_band(sample_rate) } else { f64::INFINITY };
    let band = std::env::var("ELEC_BAND").map_or(heard, |s| s.parse().expect("ELEC_BAND: a frequency in Hz"));
    println!("  {}", setup_line(&sim, if rounds.is_some() { band } else { f64::INFINITY }, &app_pool().name()));
    while sim.t() < secs {
        let to = plan_at.map_or(secs, |at| at.min(secs));
        sim.advance(to, budget, sink).expect("the board steps");
        if plan_at.is_some_and(|at| sim.t() >= at) {
            plan_at = None;
            // the first plan is made on the worker and goes in at once (`first_plan`)
            let land = std::env::var("ELEC_LAND").map(|s| s.split(';').filter(|entry| !entry.is_empty()).map(|entry| {
                let (born, to) = entry.split_once('>').unwrap_or((entry, entry));
                Land { born: born.to_string(), to: to.to_string() }
            }).collect()).unwrap_or_default();
            sim.set_planner(Some(Box::new(Declared { fmax: 0.5 / plan_step, band, never_cut: Vec::new(), land, round: sim.round_len(), only: declared.clone(), handovers: handovers.clone(), states: plan_states(), pool: plan_pool.clone(), carry: std::env::var_os("ELEC_CARRY").is_some() && !carry_source }))).unwrap_or_else(|d| panic!("the round plan installs: {d}"));
            // A synchronous first plan is already in rounds. Only a deferred plan calls started.
            if take && sim.block_work().is_some() {
                sink.started();
            }
            println!("  in rounds from t = {:.4} s: {} blocks", sim.t(), sim.blocks());
        }
        if snapshot {
            let (bytes, at) = (ALLOCATED.load(Ordering::Relaxed), Instant::now());
            let snap = sim.snapshot().expect("drum16's MCU forks");
            taken.push((at.elapsed(), ALLOCATED.load(Ordering::Relaxed) - bytes));
            last = Some(snap);
        }
    }
    let wall = start.elapsed() - taken.iter().map(|&(d, _)| d).sum::<Duration>();
    // board-acc's reference arm runs whole; its candidate in rounds is the one held
    if std::env::var_os("ELEC_SHIP_PLAN").is_some() && rounds.is_some() {
        assert!((res - 100e-6).abs() < 1e-12 && sample_rate == AUDIO_SAMPLE_RATE && band == heard,
            "ELEC_SHIP_PLAN: a run in rounds at the shipping settings (res 100 us, {AUDIO_SAMPLE_RATE} Hz, the heard band), not res {res} s, {sample_rate} Hz, band {band} Hz");
        let plan = sim.round_plan().expect("ELEC_SHIP_PLAN: the board runs in rounds at the shipping settings");
        let stages = plan.blocks.iter().map(|b| b.stage as usize + 1).max().unwrap_or(0);
        let shape: Vec<String> = plan.blocks.iter().map(|b| format!("{} (stage {})", b.name, b.stage)).collect();
        assert_eq!((plan.blocks.len(), stages), SHIP_PLAN, "the shipping plan is {} blocks over {} stages: {shape:?}", SHIP_PLAN.0, SHIP_PLAN.1);
        println!("board-acc ship plan: {} blocks over {stages} stages, as shipped", plan.blocks.len());
    }
    if take {
        sink.ended(sim.t());
        sink.idle();
    }
    if let Some(recording) = recording {
        let mut clock = recording.clock;
        // A deferred planner can keep the whole stepper and still announce the end of setup.
        if sim.stats().rounds == 0 && sim.block_work().is_none() {
            clock.t_plan = None;
        }
        rec.take_clock = clock;
    }
    if let Some((bus, taps)) = bus.zip(taps) {
        rec.takes = Some(read_takes(&bus, secs, TAKE_NETS, &taps, "take"));
        if let Some(control) = control {
            rec.control = Some(read_takes(&bus, secs, CONTROL_NETS, &control, "control take"));
        }
    }
    if record.is_some() {
        return (rec, names);
    }
    let z = snap(&sim, &seen);
    let t = z.t - a.t;
    let per_s = |x: u64| x as f64 / t;
    let d = |f: fn(&Snap) -> u64| f(&z) - f(&a);
    let dd = |f: fn(&Snap) -> Duration| f(&z) - f(&a);
    let (step, co_wall, sink_wall) = (dd(|s| s.live.step_wall), dd(|s| s.live.co_wall), dd(|s| s.live.sink_wall));
    let solver_wall = step.saturating_sub(co_wall).saturating_sub(sink_wall);
    println!();
    let label = format!("{}{}{}{}", if res == 0.0 { "Exact".to_string() } else { format!("Fast res {:.1} us", res * 1e6) },
        if profile { " (profiled)" } else { "" }, match (tiers, bypass) {
            (Some(true), Some(true)) => ", tiers and bypass on",
            (Some(false), Some(false)) => ", tiers and bypass off",
            (Some(true), _) => ", tiers on",
            (Some(false), _) => ", tiers off",
            (None, Some(true)) => ", bypass on",
            (None, Some(false)) => ", bypass off",
            (None, None) => "",
        }, single.map_or(String::new(), |g| format!(", single pass {g}")))
        + &rounds.map_or(String::new(), |at| format!(", in rounds of {} samples from {:.0} ms, {} helpers", round.unwrap_or(defaults.round), at * 1e3, sim.pool_roles().0.unwrap_or(0)));
    println!("drum16 board, {label}: {:.1} ms simulated in {:.1} ms wall: realtime x{:.4}; {} unknowns, step bound {:.2} us; per simulated second:",
        t * 1e3, ms(wall), t / wall.as_secs_f64(), sim.solver().n, max_step * 1e6);
    let pms = |x: Duration| ms(x) / t;
    println!("  wall ms: device {:.1}, analog {:.1}, sinks {:.1}; total {:.1}", pms(co_wall), pms(solver_wall), pms(sink_wall), pms(wall));
    let (tried, iterations) = (d(|s| s.st.steps), d(|s| s.st.iterations));
    println!("  analog: {:.0} points, {:.0} steps tried, rejected {:.0} (newton {:.0}, crossing {:.0}, provisional {:.0}, error {:.0}), {:.2} Newton iterations a step, {:.0} loads, {:.0} solves",
        per_s(d(|s| s.live.points)), per_s(tried), per_s(d(|s| s.st.rejected)), per_s(d(|s| s.st.rejected_newton)), per_s(d(|s| s.st.rejected_crossing)),
        per_s(d(|s| s.st.rejected_provisional)), per_s(d(|s| s.st.rejected_error)), iterations as f64 / tried.max(1) as f64,
        per_s(d(|s| s.st.loads)), per_s(d(|s| s.st.solves)));
    println!("  restarts: change {:.0}, corner {:.0}, crossing {:.0} (cut ahead {:.0}; cuts ahead short {:.0}); floor steps {:.0}, warm starts {:.0}; {:.2} us a step tried, {:.2} us a Newton iteration",
        per_s(d(|s| s.rs.change)), per_s(d(|s| s.rs.corner)), per_s(d(|s| s.rs.crossing)), per_s(d(|s| s.rs.crossing_ahead)), per_s(d(|s| s.rs.ahead_short)), per_s(d(|s| s.st.floor_steps)),
        per_s(d(|s| s.st.warm_starts)), solver_wall.as_secs_f64() * 1e6 / tried.max(1) as f64, solver_wall.as_secs_f64() * 1e6 / iterations.max(1) as f64);
    println!("  device: {:.0} runs, write instants {:.0}; {:.2} us a run", per_s(d(|s| s.live.co_runs)), per_s(d(|s| s.live.write_instants)),
        co_wall.as_secs_f64() * 1e6 / d(|s| s.live.co_runs).max(1) as f64);
    let hist = steps.hist.iter().enumerate().filter(|e| *e.1 > 0)
        .map(|(k, &n)| format!("{}{:.3} us: {:.0}", if k == 11 { "under " } else { "" }, max_step * 1e6 / (1u64 << (k + 1).min(11)) as f64, per_s(n)))
        .collect::<Vec<_>>().join(", ");
    println!("  accepted steps by length (at least): {hist}");
    if let (Some(p0), Some(p1)) = (&a.profile, &z.profile) {
        let total = solver_wall.as_nanos() as f64;
        let share = |x: u64| 100.0 * x as f64 / total;
        let (load, refactor, factor, solve) = (p1.load_ns - p0.load_ns, p1.refactor_ns - p0.refactor_ns, p1.factor_ns - p0.factor_ns, p1.solve_ns - p0.solve_ns);
        println!("  analog profile: load {:.1} ms ({:.0} %), factor {:.1} ms ({:.0}, {:.0} %), refactor {:.1} ms ({:.0}, {:.0} %), solve {:.1} ms ({:.0} %), the rest {:.1} ms ({:.0} %)",
            load as f64 / 1e6 / t, share(load), factor as f64 / 1e6 / t, per_s(p1.factors - p0.factors), share(factor),
            refactor as f64 / 1e6 / t, per_s(p1.refactors - p0.refactors), share(refactor), solve as f64 / 1e6 / t, share(solve),
            (total - (load + refactor + factor + solve) as f64) / 1e6 / t, 100.0 - share(load + refactor + factor + solve));
        rest_profile(p0, p1, t);
        let (skipped, recomputed) = (p1.columns_skipped - p0.columns_skipped, p1.columns_recomputed - p0.columns_recomputed);
        println!("  refactor columns: {:.0} skipped and {:.0} recomputed a simulated second ({:.1} % skipped)",
            per_s(skipped), per_s(recomputed), 100.0 * skipped as f64 / (skipped + recomputed).max(1) as f64);
        if let Some(plan) = sim.solver().tiers.as_ref() {
            println!("  {}", plan.report(&sim.solver().banks));
        }
        let kinds = ["on its way", "after a restart", "cut to a crossing"];
        println!("  steps by kind: {}", (0..3).map(|k| {
            let (steps, its) = (p1.kind_steps[k] - p0.kind_steps[k], p1.kind_iterations[k] - p0.kind_iterations[k]);
            format!("{} {:.0} a simulated second, {:.2} iterations each", kinds[k], per_s(steps), its as f64 / steps.max(1) as f64)
        }).collect::<Vec<_>>().join("; "));
        // what kept Newton open, over the measured window, the largest first
        let solver = sim.solver();
        let mut open: Vec<_> = p1.open_by.iter().map(|(&why, &n)| (n - p0.open_by.get(&why).copied().unwrap_or(0), why)).filter(|e| e.0 > 0).collect();
        open.sort_by(|a, b| b.0.cmp(&a.0));
        let total: u64 = open.iter().map(|e| e.0).sum();
        let name = |why: fab_elec_sim::newton::OpenBy| match why {
            fab_elec_sim::newton::OpenBy::InitJunctions => "junction init".to_string(),
            fab_elec_sim::newton::OpenBy::FirstIteration => "first iteration rule".to_string(),
            fab_elec_sim::newton::OpenBy::Instance(b, i) => format!("{} {}", solver.banks[b as usize].kind(), solver.banks[b as usize].inst_name(i as usize)),
            fab_elec_sim::newton::OpenBy::Unknown(u) => format!("moved {}", solver.unknowns[u as usize].name),
            fab_elec_sim::newton::OpenBy::Settle(b) => format!("{} refused to settle", solver.banks[b as usize].kind()),
        };
        println!("  held open ({:.0} iterations a simulated second): {}", per_s(total),
            open.iter().take(8).map(|&(n, why)| format!("{} {:.1} %", name(why), 100.0 * n as f64 / total.max(1) as f64)).collect::<Vec<_>>().join(", "));
        let refactors = (p1.refactors - p0.refactors).max(1) as f64;
        println!("  per refactor: {:.1} columns changed ({:.0} % of refactors after a move of a0), {:.1} recomputed with the ones reading them",
            (p1.columns_dirty - p0.columns_dirty) as f64 / refactors, 100.0 * (p1.a0_moves - p0.a0_moves) as f64 / refactors, recomputed as f64 / refactors);
        if let Some((fill, refactor, solve)) = sim.solver().lu_work() {
            println!("  LU: {fill} entries of L and U, {refactor} multiply-subtracts a full refactor, {solve} a solve");
        }
        let banks = &sim.solver().banks;
        let split = (0..p1.bank_ns.len()).filter(|&b| p1.bank_ns[b] > 0)
            .map(|b| format!("{}x{} {:.1} ms", banks[b].kind(), banks[b].len(), (p1.bank_ns[b] - p0.bank_ns.get(b).copied().unwrap_or(0)) as f64 / 1e6 / t))
            .collect::<Vec<_>>().join(", ");
        if !split.is_empty() {
            println!("  tiered load by bank (a simulated second): {split}");
        }
        // Fast's bypass: skipped evaluations of each bank against its evaluations at every load
        let loads = per_s(d(|s| s.st.loads));
        let bypass = sim.solver().banks.iter().filter(|b| b.bypassed() > 0)
            .map(|b| format!("{} {:.1} %", b.kind(), 100.0 * b.bypassed() as f64 / sim.t() / (loads * b.len() as f64).max(1.0)))
            .collect::<Vec<_>>().join(", ");
        if !bypass.is_empty() {
            println!("  bypassed (of the evaluations at every load, over the whole run): {bypass}");
        }
        // Fast's single pass: solves it accepted, and which bank refused the others it was asked
        let refused: u64 = p1.settle_refused.iter().sum::<u64>() - p0.settle_refused.iter().sum::<u64>();
        if p1.settled > p0.settled || refused > 0 {
            let by = p1.settle_refused.iter().enumerate().filter(|&(b, &n)| n > p0.settle_refused.get(b).copied().unwrap_or(0))
                .map(|(b, &n)| format!("{} {:.1} %", banks[b].kind(), 100.0 * (n - p0.settle_refused.get(b).copied().unwrap_or(0)) as f64 / refused.max(1) as f64))
                .collect::<Vec<_>>().join(", ");
            println!("  single pass: {:.0} solves a simulated second accepted, {:.0} refused ({by})", per_s(p1.settled - p0.settled), per_s(refused));
        }
    }
    if rounds.is_some() {
        let st = sim.stats();
        let split = &st.round_split;
        let round_count = st.rounds.max(1) as f64;
        let us_per_round = |ns: u64| ns as f64 / 1e3 / round_count;
        let block_work = sim.block_work();
        println!("  plans: {} made, {} kept (the cut the run already ran: no handover), {} blocks kept by new cuts; go-in {:.2} ms in all, {:.3} ms longest; {} rounds, packs {} on helpers, {} taken back by the worker",
            st.plans, st.plans_kept, st.blocks_kept, st.install_wall.as_secs_f64() * 1e3, st.install_max.as_secs_f64() * 1e3, st.rounds, st.packs_helped, st.packs_claimed);
        println!("  edits: {} left rounds, {:.1} ms simulated on one stepper with a planner, {} re-proofs ({:.2} ms the longest, {} over the accuracy bar at the run's state, gating nothing); plans waited for {} times ({:.2} ms the longest); {} plans and {} warms stopped (let go of before they were back); starts carried {}, carries refused {}{}",
            st.edit_leaves, st.off_rounds * 1e3, st.reproofs, st.reproof_max.as_secs_f64() * 1e3, st.reproofs_over, st.plan_waits, st.plan_wait_max.as_secs_f64() * 1e3, st.plans_stopped, st.warms_stopped,
            st.starts_carried, st.carries_refused, st.carry_refusal.as_ref().map_or(String::new(), |w| format!(" (the last: {w})")));
        // the shadow rounds: per figure its largest, where, and its decades from 1e-6; the cuts refused
        let figures: Vec<String> = fab_elec_sim::rounds::SHADOW_FIGURES.iter().enumerate().map(|(k, name)| format!("{name} {:.3e} ({}) [{}]", split.shadow_worst[k],
            if split.shadow_at[k].is_empty() { "none" } else { &split.shadow_at[k] }, split.shadow_hist[k].iter().map(u64::to_string).collect::<Vec<_>>().join(" "))).collect();
        println!("  shadow rounds: {} run again ({} by the worker, the floor), {} failed; owed and let go: {} sampling rounds replaced by a newer, {} burst rounds stale, {} thinned, {} still owed at their plan's end ({} chain trees skipped); {:.1} us a round taking them, {:.1} us running them; where a block ends held to {:.1e} at least, at most {:.2} of its bar judged ({}); past their bar unshifted {} refused, {} shifted to an edge and held, {} shifted and refusing; {} cuts refused{}; {}",
            split.shadows, split.shadow_floor, split.shadow_failed, split.shadow_replaced, split.shadow_stale, split.shadow_thinned, split.shadow_lost, split.chain_skipped, us_per_round(split.shadow_take_ns), us_per_round(split.shadow_run_ns), split.shadow_bar, split.shadow_nearest,
            if split.shadow_nearest_at.is_empty() { "none" } else { &split.shadow_nearest_at }, split.shadow_past[0], split.shadow_past[1], split.shadow_past[2], st.shadow_acts,
            st.last_shadow_act.as_ref().map_or(String::new(), |w| format!(" (the last: {w})")), figures.join("; "));
        // their ends as shifts (Fable's A-next), counted: per reader its drift and smear
        let by: Vec<String> = split.shift_by.iter().map(|(name, s)| format!("{name} {} rounds, drift {:.2e}{}, smear {:.2e}, largest {:.4} us", s.n, s.drift(),
            if s.drifts() { " (clear of its scatter)" } else { "" }, s.smear(), s.most * 1e6)).collect();
        println!("  shadow shifts: {} within the bar shifted along the last step, {} still; {} past it but shifted within the step bound, {} past it however shifted; {} with their own crossings apart; shifts by decade from 1e-12 s [{}]; {}",
            split.shift_plain, split.shift_still, split.shift_found, split.shift_over, split.shift_parity, split.shift_hist.iter().map(u64::to_string).collect::<Vec<_>>().join(" "), by.join("; "));
        // the chain shadow: born imports' jumps, trees run again, per figure its largest over the members
        let chain: Vec<String> = fab_elec_sim::rounds::SHADOW_FIGURES.iter().enumerate().map(|(k, name)| format!("{name} {:.3e} ({})", split.chain_worst[k],
            if split.chain_at[k].is_empty() { "none" } else { &split.chain_at[k] })).collect();
        let per_member = |ns: u64| if split.chain_members > 0 { ns as f64 / split.chain_members as f64 / 1e3 } else { 0.0 };
        let escalated_by: Vec<String> = split.chain_escalated_by.iter().map(|(name, n)| format!("{name} {n}")).collect();
        println!("  chain shadow: {} imports with a born jump split, {} trees run again ({} members, {} failed, {} cut late); {:.2} ms in all taking them, {:.1} us a round taking them, {:.1} us running them, {:.1} us a member-round (its own round in the run {:.1} us); {} trees on first jumps, {:.1} us a round of it; plans' trees kept {}, begun {}; at most {:.2} of its bar ({}), {} trees past it, followed {}: {} past it at the window's end, {} held past it, {} healed, own held at most {:.2} of its bar ({}); {} trees escalated [{}], {} members unswept, {} owed trees carried, {} owed trees dropped stale; {}; own signed mean {:+.3e} V ({})",
            split.chain_jumps, split.chain_trees, split.chain_members, split.chain_failed, split.chain_cut_late, split.chain_take_ns as f64 / 1e6, us_per_round(split.chain_take_ns), us_per_round(split.chain_run_ns), per_member(split.chain_run_ns), per_member(split.chain_own_ns),
            split.chain_first, us_per_round(split.chain_first_ns), split.chain_kept, split.chain_began,
            split.chain_nearest, if split.chain_nearest_at.is_empty() { "none" } else { &split.chain_nearest_at }, split.chain_over,
            split.chain_followed, split.chain_later_over, split.chain_held_over, split.chain_healed,
            split.chain_held_nearest, if split.chain_held_nearest_at.is_empty() { "none" } else { &split.chain_held_nearest_at },
            split.chain_escalated, escalated_by.join(", "), split.chain_unswept, split.chain_owed_carried, split.chain_owed_stale, chain.join("; "),
            split.chain_signed, if split.chain_signed_at.is_empty() { "none" } else { &split.chain_signed_at });
        println!("  chain act: {} exports convicted, {} marked, {} marks healed; {} chains landed{}", split.chain_convicted, split.chain_marked, split.chain_unmarked, st.chain_acts,
            st.last_chain_act.as_ref().map_or(String::new(), |w| format!(" (the last: {w})")));
        println!("  growth rule: {} breaches{}", st.breaches, st.last_breach.as_ref().map_or(String::new(), |w| format!(" (the last: {w})")));
        assert_eq!(OFF_SHAPE.load(Ordering::Relaxed), 0, "plans off the shape ELEC_SHAPE pins");
        if let Some(d) = &st.plan_error {
            println!("  the last plan not taken: {}", d.message);
        }
        // each block's work since its rounds began: per simulated second, and its smoothed wall a
        // round; a stage's slowest block bounds it, the stages run one after another
        if let Some((n, round, ref work)) = block_work {
            let span = (n as f64 * round).max(f64::MIN_POSITIVE);
            let stages = work.iter().map(|w| w.stage).max().map_or(0, |s| s + 1);
            let slowest: Vec<f64> = (0..stages).map(|k| work.iter().filter(|w| w.stage == k).map(|w| w.cost_ns).fold(0.0, f64::max)).collect();
            let all: f64 = work.iter().map(|w| w.cost_ns).sum();
            let it: u64 = work.iter().map(|w| w.stats.iterations).sum();
            let tried: u64 = work.iter().map(|w| w.stats.steps).sum();
            println!("  blocks over {:.1} ms in rounds: {:.0} steps tried and {:.0} iterations a simulated second ({:.2} a step); a round {:.1} us of block wall, the stages' slowest {} = {:.1} us",
                span * 1e3, tried as f64 / span, it as f64 / span, it as f64 / tried.max(1) as f64, all / 1e3,
                slowest.iter().map(|c| format!("{:.1}", c / 1e3)).collect::<Vec<_>>().join(" + "), slowest.iter().sum::<f64>() / 1e3);
            let mut costliest: Vec<_> = work.iter().collect();
            costliest.sort_by(|a, b| b.cost_ns.total_cmp(&a.cost_ns));
            let step_us = |ns: u64, steps: u64| {
                if steps == 0 { "n/a".to_string() } else { format!("{:.2} us", ns as f64 / 1e3 / steps as f64) }
            };
            for w in costliest.iter().take(8) {
                println!("    {} (stage {}, {} unknowns): {:.1} us a round, {:.0} steps tried and {:.0} loads a simulated second, {:.2} iterations a step",
                    w.name, w.stage, w.unknowns, w.cost_ns / 1e3, w.stats.steps as f64 / span, w.stats.loads as f64 / span, w.stats.iterations as f64 / w.stats.steps.max(1) as f64);
                println!("      first step {}, later steps {}; on the worker {} a round ({} rounds), on a helper {} a round ({} rounds)",
                    step_us(w.first_ns, w.first_n), step_us(w.later_ns, w.later_n), step_us(w.worker_ns, w.worker_rounds), w.worker_rounds, step_us(w.helper_ns, w.helper_rounds), w.helper_rounds);
            }
        }
        if st.rounds > 0 {
            let sum = split.ctl_ns + split.round_start_ns + split.export_ns + split.feed_ns + split.pack_ns + split.own_ns + split.claimed_ns
                + split.wait_ns + split.stage_rest_ns + split.next_point_ns + split.put_ns + split.end_merge_ns + split.merge_wait_ns;
            println!("  round split (us a round): block 0 {:.1} ({:.1} steps), round start {:.1}, exports {:.1}, feeds {:.1}, packing {:.1}, own pack {:.1}, claimed {:.1}, wait {:.1}, stage rest {:.1}, next_point {:.1}, put {:.1} (sink {:.1}, trace {:.1}, interval {:.1}), end_merge {:.1}, merge wait {:.1}, merge job {:.1} ({:.2} jobs, {:.2} claimed); sum {:.1} of {:.1}",
                us_per_round(split.ctl_ns), split.ctl_steps as f64 / round_count, us_per_round(split.round_start_ns), us_per_round(split.export_ns),
                us_per_round(split.feed_ns), us_per_round(split.pack_ns), us_per_round(split.own_ns), us_per_round(split.claimed_ns), us_per_round(split.wait_ns),
                us_per_round(split.stage_rest_ns), us_per_round(split.next_point_ns), us_per_round(split.put_ns), sink_wall.as_secs_f64() * 1e6 / round_count,
                us_per_round(split.put_trace_ns), us_per_round(split.put_interval_ns), us_per_round(split.end_merge_ns), us_per_round(split.merge_wait_ns), us_per_round(split.merge_job_ns),
                split.merge_jobs as f64 / round_count, split.merge_claimed as f64 / round_count, us_per_round(sum), wall.as_secs_f64() * 1e6 / round_count);
            let stage_us = |ns: &[u64]| ns.iter().map(|&n| format!("{:.1}", us_per_round(n))).collect::<Vec<_>>().join(", ");
            println!("  per stage (us a round): own [{}], wait [{}]", stage_us(&split.stage_own_ns), stage_us(&split.stage_wait_ns));
            println!("  helpers: {} packs started, wake-up mean {:.2} us, max {:.2} us; helper pack run {:.1} us a round", split.wake_n,
                split.wake_ns as f64 / 1e3 / split.wake_n.max(1) as f64, split.wake_max_ns as f64 / 1e3, us_per_round(split.helper_run_ns));
            let owner = |b: u32| block_work.as_ref().and_then(|(_, _, blocks)| blocks.get(b as usize)).map_or_else(|| format!("block {b}"), |w| w.name.clone());
            let mut pairs: Vec<_> = split.close_pairs.iter().collect();
            pairs.sort_by(|a, b| b.1.cmp(a.1));
            let pairs = pairs.into_iter().take(8).map(|(&(a, b), &n)| format!("{} / {} {:.1}/s", owner(a), owner(b), per_s(n))).collect::<Vec<_>>().join(", ");
            println!("  merged points: {:.1} a round; close points (under 5 ns) {:.1} a simulated second (<1 ps {:.1}, <100 ps {:.1}, <5 ns {:.1}), same block both {:.1} %; top owner pairs: {}",
                split.merged_points as f64 / round_count, per_s(split.close_points), per_s(split.close_gap[0]), per_s(split.close_gap[1]), per_s(split.close_gap[2]),
                100.0 * split.close_same_block as f64 / split.close_points.max(1) as f64, if pairs.is_empty() { "none" } else { &pairs });
        } else {
            println!("  round split: no completed rounds");
        }
    }
    if let Some((h, n)) = rec.digest {
        println!("  digest drum16 board, {label}: {n} points {h:016x}");
    }
    if let Some(a) = sim.solver().profile.as_ref().and_then(|p| p.attrib.as_ref()) {
        print!("{}", a.report(sim.solver(), t, u64::try_from(solver_wall.as_nanos()).unwrap_or(u64::MAX)));
    }
    if let Some(snap) = last {
        taken.sort();
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        let (n, mid) = (taken.len(), taken[taken.len() / 2]);
        let max_bytes = taken.iter().map(|&(_, b)| b).max().unwrap_or(0);
        let at = Instant::now();
        sim.restore(&snap);
        let restore = at.elapsed();
        println!("  snapshots, {label}: {n} taken (one at each return from `advance`, out of the wall time above): {:.0} us median, {:.0} us longest; {:.0} KB allocated by the median, {:.0} KB the most; a restore {:.0} us",
            us(mid.0), us(taken[n - 1].0), mid.1 as f64 / 1024.0, max_bytes as f64 / 1024.0, us(restore));
    }
    (rec, names)
}

const DRUM_TRIGGERS: [&str; 7] = ["KICK", "SNARE", "LTOM", "HTOM", "CHAT", "OHAT", "CYM"];

/// A trigger net's edge counts as an event (`trigger_events`) only where the net swings at least
/// this far over the window (V): a logic trigger, not a quiet net's noise.
const TRIGGER_SWING_V: f64 = 1.0;

/// After a trigger's edge, how long a self-timed oscillator's edges are left out of its pitch
/// (`edge_pitch`, s): the voice's charge has been drawn, and its late copy in rounds (a pipeline's
/// depth of rounds, under 2 ms at the drum rig's rounds) has settled.
const PITCH_SETTLE_S: f64 = 4e-3;

/// The reference's trigger edges inside `[from, to]`, rising and falling at each trigger's
/// midpoint, ascending: the instants a voice draws its envelope's charge, which a self-timed
/// oscillator's pitch is judged between (`edge_pitch`). A trigger that swings less than
/// `TRIGGER_SWING_V` gives none. O(triggers x points).
fn trigger_events(r: &Rec, names: &[String], from: f64, to: f64) -> Vec<f64> {
    let mut events = Vec::new();
    for (i, _) in names.iter().enumerate().filter(|(_, n)| DRUM_TRIGGERS.contains(&n.as_str())) {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for (k, &at) in r.t.iter().enumerate() {
            if at >= from && at <= to {
                lo = lo.min(r.x[k * r.n + i]);
                hi = hi.max(r.x[k * r.n + i]);
            }
        }
        if hi - lo < TRIGGER_SWING_V {
            continue;
        }
        let level = lo + 0.5 * (hi - lo);
        for k in 0..r.t.len().saturating_sub(1) {
            let (a, b) = (r.x[k * r.n + i], r.x[(k + 1) * r.n + i]);
            if (a < level) != (b < level) {
                let at = r.t[k] + (r.t[k + 1] - r.t[k]) * ((level - a) / (b - a));
                if at >= from && at <= to {
                    events.push(at);
                }
            }
        }
    }
    events.sort_by(f64::total_cmp);
    events
}

/// A self-timed oscillator's rising edges (`run`) against the reference's: its pitch, the slope
/// of the edge error on the stretches between the trigger `events` (each stretch its own offset,
/// one slope for all; the edges within `PITCH_SETTLE_S` after an event left out), in cents; its
/// phase step at the events, each fitted stretch's offset less the one before it (us), the largest
/// by size, signed (None with one stretch); and the error's scatter about the fit (rms, us). Each
/// run edge is read against the nearest reference edge. None with no stretch of two edges. A
/// phase step is no pitch: over a window with hits a mean rate of edges mixes the two.
/// O(edges + events).
fn edge_pitch(reference: &[f64], run: &[f64], events: &[f64]) -> Option<(f64, Option<f64>, f64)> {
    if reference.is_empty() {
        return None;
    }
    let mut stretches: Vec<Vec<(f64, f64)>> = vec![Vec::new(); events.len() + 1];
    let mut k = 0;
    for &at in run {
        while k + 1 < reference.len() && (reference[k + 1] - at).abs() <= (reference[k] - at).abs() {
            k += 1;
        }
        let t = reference[k];
        let s = events.partition_point(|&e| e < t);
        if s > 0 && t <= events[s - 1] + PITCH_SETTLE_S {
            continue;
        }
        stretches[s].push((t, at - t));
    }
    let means: Vec<Option<(f64, f64)>> = stretches.iter().map(|v| {
        let n = v.len() as f64;
        (v.len() >= 2).then(|| (v.iter().map(|p| p.0).sum::<f64>() / n, v.iter().map(|p| p.1).sum::<f64>() / n))
    }).collect();
    let (mut num, mut den) = (0.0, 0.0);
    for (v, m) in stretches.iter().zip(&means) {
        let Some((tm, dm)) = *m else { continue };
        for &(t, d) in v {
            num += (t - tm) * (d - dm);
            den += (t - tm) * (t - tm);
        }
    }
    if den <= 0.0 {
        return None;
    }
    let slope = num / den;
    let offsets: Vec<Option<f64>> = means.iter().map(|m| m.map(|(tm, dm)| dm - slope * tm)).collect();
    let fitted: Vec<f64> = offsets.iter().flatten().copied().collect();
    let step = fitted.windows(2).map(|w| w[1] - w[0]).fold(None, |m: Option<f64>, s| if m.is_none_or(|m| s.abs() > m.abs()) { Some(s) } else { m });
    let (mut sq, mut n) = (0.0, 0usize);
    for (v, o) in stretches.iter().zip(&offsets) {
        let Some(o) = *o else { continue };
        for &(t, d) in v {
            sq += (d - o - slope * t).powi(2);
            n += 1;
        }
    }
    Some((-1200.0 * (1.0 + slope).log2(), step.map(|s| s * 1e6), (sq / n as f64).sqrt() * 1e6))
}

/// Rising midpoint crossings of the piecewise-linear accepted points, inside the common window.
fn rising_edges(r: &Rec, i: usize, level: f64, from: f64, to: f64) -> Vec<f64> {
    let mut edges = Vec::new();
    for k in 0..r.t.len() - 1 {
        let (a, b) = (r.x[k * r.n + i], r.x[(k + 1) * r.n + i]);
        if a < level && b >= level {
            let at = r.t[k] + (r.t[k + 1] - r.t[k]) * ((level - a) / (b - a));
            if at >= from && at <= to {
                edges.push(at);
            }
        }
    }
    edges
}

/// Zero error stays zero for a constant reference; a nonzero error has infinite relative size.
fn relative_error(error: f64, scale: f64) -> f64 {
    if error == 0.0 { 0.0 } else { error / scale }
}

/// board-acc judges a third-octave band only this close to the reference's loudest band (dB): a
/// quieter band is printed, not judged (what holds a free-running oscillator's phase there is
/// inaudible under the loud ones).
///
/// The bars the judged figures are read against. Exact: each judged band within 1 dB of the
/// reference, each metal oscillator within 0.1 cent of its pitch. Fast trades accuracy for speed,
/// and its distance from the reference is its stated cost, printed: a change to Fast keeps each
/// judged band within 1 dB of the Fast before it, and the oscillators within 2 cents of the
/// reference at the host's 96 kHz. A metal oscillator's pitch is the slope of its edge error
/// between the triggers (`edge_pitch`); its phase step at a trigger is printed, and a plan's own
/// steps at hits are judged by what is heard (the hit bar, `HIT_ON_TIME_US`): at the 48 kHz band
/// in rounds they reach 25 us with drum16's own pattern and 194 us with every voice on every step,
/// unheard. The reference's own noise is under 0.2 us (Exact at reltol 1e-4 against 1e-5: steps
/// 0.1 us, scatter 0.16 us), whole Fast's steps under 1 us.
const BAND_JUDGED_DB: f64 = 30.0;

/// The hats' envelopes: their edges are when a hat sounds (the hit bar, `HIT_ON_TIME_US`).
const HATS_ENVELOPES: [&str; 2] = ["ENV", "OENV"];

/// The hit bar: a plan's own phase steps at hits, judged by what is heard. The oscillators of the
/// hats' metal run free, so every hit of the circuit itself catches them at another phase: a step
/// moves no hit, and is heard only if it widens the hits' tone from one hit to the next. Per hats
/// hit (`hit_figures`), against the reference: the hats' triggers and envelopes on time within
/// `HIT_ON_TIME_US` (a flam needs a millisecond); each hit's hats bands from `HIT_FROM_HZ` to
/// `BAND_TOP_HZ` within `HIT_BAND_DB`; and from one hit to the next, the run's largest band change
/// there at most `HIT_SPREAD_DB` over the reference's. The bounds are the figures with every voice
/// and the accent on every step (`ELEC_PATTERN=full`, the 48 kHz band in rounds on one helper at
/// 96 kHz: 7.9 us, 1.2 dB, 0.17 dB) with headroom. Below `HIT_FROM_HZ`, where HATS is 20 to 30 dB
/// under its loudest band, the figures are printed, not judged: there that run's hit-to-hit change
/// reaches 5.2 dB against the reference's 3.9 dB.
const HIT_ON_TIME_US: f64 = 10.0;

/// The hit bar's lowest judged band (Hz, `HIT_ON_TIME_US`).
const HIT_FROM_HZ: u32 = 2000;

/// The hit bar's largest band difference of a hit from the reference's (dB, `HIT_ON_TIME_US`).
const HIT_BAND_DB: f64 = 1.5;

/// The hit bar's largest widening of the change from one hit to the next (dB, `HIT_ON_TIME_US`).
const HIT_SPREAD_DB: f64 = 0.5;

/// The highest third-octave band board-acc judges (Hz): the 20 kHz band, past what most hear and
/// at a 48 kHz run's band edge, is printed, not judged.
const BAND_TOP_HZ: u32 = 16_000;

/// The shortest window board-acc judges bands in (s): a shorter one has too few bins in the low
/// bands, and a phase drift reads as a level error there.
const BAND_MIN_WINDOW_S: f64 = 0.2;

/// Nominal ISO third-octave centres; integration uses their exact base-ten band edges.
const THIRD_OCTAVE_HZ: [u32; 24] = [
    100, 125, 160, 200, 250, 315, 400, 500, 630, 800, 1000, 1250,
    1600, 2000, 2500, 3150, 4000, 5000, 6300, 8000, 10000, 12500, 16000, 20000,
];

/// AC band power in dBV, with Hann noise-power normalization. Padding changes the frequency
/// grid, not the power: sum the one-sided bins with scale 2 / (n_fft * sum(window^2)). A band
/// without a bin in this window is unavailable; zero power is -inf dBV.
fn third_octave(samples: &[f64], dt: f64) -> [Option<f64>; 24] {
    let n = samples.len().next_power_of_two();
    let mut data = vec![C64::new(0.0, 0.0); n];
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let mut weights = 0.0;
    for (i, &sample) in samples.iter().enumerate() {
        let w = Window::Hann.coeff(i, samples.len());
        data[i] = C64::new((sample - mean) * w, 0.0);
        weights += w * w;
    }
    fft(&mut data, false).expect("a zero-padded power-of-two FFT");
    let df = 1.0 / (n as f64 * dt);
    std::array::from_fn(|band| {
        let lo = 1000.0 * 10.0_f64.powf((band as f64 - 10.5) / 10.0);
        let hi = 1000.0 * 10.0_f64.powf((band as f64 - 9.5) / 10.0);
        let first = ((lo / df).ceil() as usize).max(1);
        let end = ((hi / df).ceil() as usize).min(n / 2 + 1);
        if first >= end {
            return None;
        }
        let power = data[first..end].iter().map(|v| v.norm_sqr()).sum::<f64>() * 2.0 / (n as f64 * weights);
        Some(10.0 * power.log10())
    })
}

/// The accepted steps presented to the recorder between whole grid instants in the read check.
#[derive(Clone, Copy)]
enum ReadSteps {
    Uniform { per_period: usize, free: bool },
    Watched { every: usize },
    OnChord,
    RoundHalves,
}

/// Record the signal on X against ground, with the board recorder's DC block and unity gain.
fn read_check_take(secs: f64, rate: f64, steps: ReadSteps, signal: &impl Fn(f64) -> f64) -> BoardTake {
    let nets = [("X", 1)];
    let bus = take_bus();
    let mut taps = board_taps(&bus, secs, nets, 1.0);
    let [tap] = &mut taps;
    tap.begin(&["0", "X"].map(|name| UnknownInfo { name: name.into(), kind: UnkKind::Voltage }));
    tap.sample_period(1.0 / rate);
    let mut point = |t, free| {
        tap.point(t, None, &[0.0, signal(t)]);
        if free {
            tap.chosen(t);
        }
    };
    let periods = (secs * rate).floor() as usize;
    point(0.0, false);
    match steps {
        ReadSteps::Uniform { per_period, free } => {
            for n in 1..=periods * per_period {
                point(n as f64 / (per_period as f64 * rate), free);
            }
        }
        ReadSteps::Watched { every } => {
            // Stepper::step retries a watched crossing at its interpolated fraction times
            // (1 + 5e-4), with Exact's floor. Here the trial crossed a quarter-period in.
            // The watch still moves on the provisional Euler step and settles on its confirmation.
            // Both are limited by EDGE_STEP, above the confirming floor, and neither is snapped.
            // With a small accepted error the next step doubles. Fast's floor is now one period.
            // The quarter and half steps are below it, then a bound-length trial snaps to the grid.
            const CUT: f64 = 0.25 * (1.0 + 5e-4);
            const EDGE_STEP: f64 = 0.125;
            let mut period = 0;
            while period < periods {
                if period != 0 && period % every == 0 && periods - period >= 2 {
                    let mut at = period as f64;
                    for (length, free) in [(CUT, true), (EDGE_STEP, true), (EDGE_STEP, true), (0.25, false), (0.5, false)] {
                        at += length;
                        point(at / rate, free);
                    }
                    period += 2;
                } else {
                    period += 1;
                }
                point(period as f64 / rate, false);
            }
        }
        ReadSteps::OnChord => {
            let mut seed = 41_u64;
            let mut next = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (seed >> 32) as u32
            };
            let (mut t0, mut y0) = (0.0, signal(0.0));
            for n in 1..=periods {
                let t1 = n as f64 / rate;
                let y1 = signal(t1);
                let count = 1 + next() % 9;
                // One offset inside each subdivision, in order and clear of its ends.
                for i in 0..count {
                    let jitter = 0.125 + 0.75 * f64::from(next()) / f64::from(u32::MAX);
                    let t = t0 + (t1 - t0) * ((f64::from(i) + jitter) / f64::from(count));
                    let y = y0 + (y1 - y0) * ((t - t0) / (t1 - t0));
                    tap.point(t, None, &[0.0, y]);
                    if next() >> 30 == 0 {
                        tap.mark(t);
                    }
                }
                tap.point(t1, None, &[0.0, y1]);
                (t0, y0) = (t1, y1);
            }
        }
        ReadSteps::RoundHalves => {
            for n in 0..periods {
                if n % 32 == 31 && (n / 32) % 3 != 2 {
                    point((n as f64 + 0.5) / rate, false);
                }
                point((n + 1) as f64 / rate, false);
            }
        }
    }
    tap.ended(periods as f64 / rate);
    tap.idle();
    let [take] = read_takes(&bus, secs, nets, &taps, "take");
    take
}

#[derive(Clone, Copy)]
struct ReadTone {
    band: usize,
    hz: u32,
    amplitude: f64,
    phase: f64,
}

/// One tone per measured band. Even with all thirteen aligned the peak is at most 0.52 V.
fn read_tones() -> Vec<ReadTone> {
    let bands = THIRD_OCTAVE_HZ.iter().copied().enumerate().filter(|(_, hz)| (1000..=BAND_TOP_HZ).contains(hz));
    let count = bands.clone().count() as f64;
    bands.enumerate().map(|(i, (band, hz))| ReadTone {
        band, hz, amplitude: 0.04, phase: std::f64::consts::TAU * i as f64 / count,
    }).collect()
}

fn tone_signal(tones: &[ReadTone], t: f64) -> f64 {
    tones.iter().map(|s| s.amplitude * (std::f64::consts::TAU * f64::from(s.hz) * t + s.phase).sin()).sum()
}

/// The complete take and the delivered frames left after trimming 0.1 s from each end.
fn read_window(mode: &str, secs: f64) -> (usize, std::ops::Range<usize>) {
    let frames = (secs * DELIVERED).floor() as usize;
    let trim = (0.1 * DELIVERED) as usize;
    assert!(secs.is_finite() && frames >= 2 * trim + 3, "{mode} needs at least three frames after trimming 0.1 s at each end");
    (frames, trim..frames - trim)
}

fn read_levels(interior: std::ops::Range<usize>, sample: impl Fn(usize) -> f64) -> [Option<f64>; 24] {
    let samples: Vec<_> = interior.map(sample).collect();
    third_octave(&samples, 1.0 / DELIVERED)
}

/// Compare the interior of every take with the same tones at the delivered frame instants.
fn take_read(secs: f64) {
    use ReadSteps::{OnChord, RoundHalves, Uniform, Watched};

    let (frames, interior) = read_window("take-read", secs);
    let tones = read_tones();
    let signal = |t| tone_signal(&tones, t);
    let truth = read_levels(interior.clone(), |n| signal(n as f64 / DELIVERED));
    assert!(tones.iter().all(|s| truth[s.band].is_some_and(f64::is_finite)), "take-read needs a longer interior to measure every band from 1 to 16 kHz");

    let kinds = [
        ("a", Uniform { per_period: 1, free: false }),
        ("b", Uniform { per_period: 16, free: true }),
        // Each restart marks two instants. These fixed spacings give the nearest 2/N to
        // 0.05, 0.18 and 0.35. The row reports the share the read actually counted.
        ("c", Watched { every: 40 }),
        ("c", Watched { every: 11 }),
        ("c", Watched { every: 6 }),
        ("d", Uniform { per_period: 2, free: false }),
        ("e", OnChord),
        ("f", RoundHalves),
    ];
    let mut summaries = Vec::new();
    for rate in [48_000, 64_000, 96_000] {
        let mut grid = Vec::new();
        for (kind, steps) in kinds {
            let case = match steps {
                Watched { every } => format!("R={rate} kind={kind} restart_periods={every}"),
                _ => format!("R={rate} kind={kind}"),
            };
            let take = read_check_take(secs, f64::from(rate), steps, &signal);
            assert_eq!(take.samples.len(), frames, "take-read {case} needs the complete delivered take");
            let counts = take.counts.expect("the recorder received the run's sample period");
            let levels = read_levels(interior.clone(), |n| f64::from(take.samples[n]));
            let mut max_abs = 0.0_f64;
            let mut gains = Vec::new();
            for &ReadTone { band: i, hz, .. } in &tones {
                let gain = levels[i].expect("the same bins as the truth") - truth[i].unwrap();
                assert!(!gain.is_nan(), "take-read {case} has undefined gain at {hz} Hz");
                gains.push(format!("{hz}Hz={gain:+.3}"));
                max_abs = max_abs.max(gain.abs());
            }
            println!("take-read {case} gain_db=[{}] max_abs_db={max_abs:.3} {}", gains.join(" "), read_description(take.counts));
            summaries.push(format!("take-read summary {case} max_abs_db={max_abs:.3} s={}", beside_share(counts)));
            if kind == "a" {
                grid = take.samples;
            } else if kind == "e" {
                let diff = take.samples.iter().zip(&grid).map(|(&e, &a)| (f64::from(e) - f64::from(a)).abs()).fold(0.0, f64::max);
                println!("take-read e-vs-a R={rate} max_abs={diff:.9e}");
            }
        }
    }
    for summary in summaries {
        println!("{summary}");
    }
}

const RC_CORNER_HZ: f64 = 4000.0;

/// Record both sides of the RC in one run. The taps resolve their nets from the solver.
/// `reference`: at board-acc's reference tolerances (reltol 1e-4, vntol and abstol divided as
/// there), else at the grade's own.
fn rc_takes(tones: &[ReadTone], secs: f64, rate: f64, res: f64, reference: bool) -> (f64, [BoardTake; 2]) {
    use fab_elec_sim::{Source, Wave};
    use std::f64::consts::TAU;

    let mut circuit = SimCircuit::new();
    let mut lower = "0".to_owned();
    for (i, tone) in tones.iter().enumerate() {
        let upper = if i + 1 == tones.len() { "in".to_owned() } else { format!("n{}", i + 1) };
        let wave = Wave::Sin { vo: 0.0, va: tone.amplitude, freq: f64::from(tone.hz), td: 0.0, theta: 0.0, phase: tone.phase.to_degrees() };
        circuit.v(&format!("v{}", i + 1), &upper, &lower, Source::wave(wave));
        lower = upper;
    }
    let ohms = 1e3;
    circuit.r("r1", "in", "out", ohms);
    circuit.c("c1", "out", "0", 1.0 / (TAU * ohms * RC_CORNER_HZ));
    let reltol = reference.then(|| {
        let ref_reltol = 1e-4;
        circuit.opts.vntol /= FAST_RELTOL / ref_reltol;
        circuit.opts.abstol /= FAST_RELTOL / ref_reltol;
        ref_reltol
    });
    let spec = LiveSpec { rate: f64::INFINITY, trace: false, sample_rate: rate, res, reltol, ..LiveSpec::default() };
    let mut sim = LiveSim::new(circuit, ValuePlan::default(), 1, spec).expect("the RC operating point converges");

    let nets = [("in", 1), ("out", 2)];
    let bus = take_bus();
    let mut taps = board_taps(&bus, secs, nets, 1.0);
    let [input, output] = &mut taps;
    let mut sink = TakeSink { sinks: [input, output], control: None, filter: ControlFilter::default(), clock: TakeClock::default() };
    let period = 1.0 / sim.sample_rate();
    sink.begin(&sim.solver().unknowns);
    sink.sample_period(period);
    while sim.t() < secs {
        sim.advance(secs, None, &mut sink).expect("the RC steps");
    }
    sink.ended(sim.t());
    sink.idle();
    let takes = read_takes(&bus, secs, nets, &taps, "rc take");
    // Frame zero is the read's first grid instant (Band::first_at), including at a rate change.
    let first = bus.tapes.current(nets[0].1).expect("the input was recorded").t0;
    let origin = ((first - 1e-6 * period) / period).ceil() * period;
    (origin, takes)
}

/// K3: the RC's delivered levels against the continuous or grid-trapezoidal transfer.
fn rc_read(secs: f64) {
    use std::f64::consts::PI;

    let (frames, interior) = read_window("rc-read", secs);
    let tones = read_tones();
    let mut summaries = Vec::new();
    // Exact and Fast at their own tolerances at 48 and 64 kHz, then Exact as board-acc's reference
    // arm runs it (its tolerances, at the reference rate the take checks use)
    let runs = [48_000, 64_000].into_iter().flat_map(|r| [(r, "Exact", 0.0, false), (r, "Fast", 100e-6, false)]).chain([(96_000, "Exact-ref", 0.0, true)]);
    for (rate, grade, res, reference) in runs {
        let h = 1.0 / f64::from(rate);
        let case = format!("R={rate} grade={grade}");
        let (origin, [input, output]) = rc_takes(&tones, secs, f64::from(rate), res, reference);
        assert_eq!([input.samples.len(), output.samples.len()], [frames; 2], "rc-read {case} needs both complete takes");
        let mut wanted = Vec::new();
        let mut want_db = Vec::new();
        for &tone in &tones {
            let hz = tone.hz;
            let f = f64::from(hz);
            let fw = if res == 0.0 { f } else { (PI * f * h).tan() / (PI * h) };
            let x = fw / RC_CORNER_HZ;
            let gain = 1.0 / (1.0 + x * x).sqrt();
            wanted.push(ReadTone { amplitude: tone.amplitude * gain, phase: tone.phase - x.atan(), ..tone });
            want_db.push(format!("{hz}Hz={:+.3}", 20.0 * gain.log10()));
        }
        let at = |n: usize| origin + n as f64 / DELIVERED;
        let truth = read_levels(interior.clone(), |n| tone_signal(&tones, at(n)));
        let want = read_levels(interior.clone(), |n| tone_signal(&wanted, at(n)));
        let input = read_levels(interior.clone(), |n| f64::from(input.samples[n]));
        let output = read_levels(interior.clone(), |n| f64::from(output.samples[n]));
        let (mut max_abs, mut floor_max, mut ratio_max) = (0.0_f64, 0.0_f64, 0.0_f64);
        let mut dev_db = Vec::new();
        for &ReadTone { band: i, hz, .. } in &tones {
            let truth = truth[i].expect("rc-read needs a longer interior to measure every band from 1 to 16 kHz");
            let want = want[i].expect("the same bins as the truth");
            let input = input[i].expect("the same bins as the truth");
            let output = output[i].expect("the same bins as the truth");
            let dev = output - want;
            let floor = input - truth;
            let ratio = (output - input) - (want - truth);
            assert!(!dev.is_nan() && !floor.is_nan() && !ratio.is_nan(), "rc-read {case} has undefined gain at {hz} Hz");
            dev_db.push(format!("{hz}Hz={dev:+.3}"));
            max_abs = max_abs.max(dev.abs());
            floor_max = floor_max.max(floor.abs());
            ratio_max = ratio_max.max(ratio.abs());
        }
        let maxima = format!("max_abs_db={max_abs:.3} floor_max_abs_db={floor_max:.3} ratio_max_abs_db={ratio_max:.3}");
        println!("rc-read {case} want_db=[{}] dev_db=[{}] {maxima}", want_db.join(" "), dev_db.join(" "));
        summaries.push(format!("rc-read summary {case} {maxima} bar_db=0.05"));
    }
    for summary in summaries {
        println!("{summary}");
    }
}

/// drum16's deterministic voices: each struck by its trigger, none free-running.
const DRUM_VOICES: [&str; 7] = ["KICKOUT", "LOW", "HIGH", "ENV", "OENV", "snare.body", "snare.sw"];

/// The shift of `b` against `a` (1 us grids) within `span` steps of `stride` samples that leaves the
/// smallest RMS difference over their overlap, and that difference: a voice late or early, against
/// a voice wrong.
fn best_lag(a: &[f64], b: &[f64], stride: usize, span: i64) -> (i64, f64) {
    let (a, b): (Vec<f64>, Vec<f64>) = (a.iter().step_by(stride).copied().collect(), b.iter().step_by(stride).copied().collect());
    let n = a.len().min(b.len()) as i64;
    let mut best = (0, f64::INFINITY);
    for lag in -span..=span {
        let (from, to) = (0.max(-lag), n.min(n - lag));
        if to - from < 2 {
            continue;
        }
        let sum: f64 = (from..to).map(|k| (b[(k + lag) as usize] - a[k as usize]).powi(2)).sum();
        let rms = (sum / (to - from) as f64).sqrt();
        if rms < best.1 {
            best = (lag * stride as i64, rms);
        }
    }
    best
}

/// The app's task pool as the host's live runs use it (`RunPool`), made once for the process:
/// drum16's runs go where the host's would, by the host's own code.
fn app_pool() -> &'static RunPool {
    static POOL: std::sync::OnceLock<RunPool> = std::sync::OnceLock::new();
    POOL.get_or_init(RunPool::of_process)
}

/// Whether a drum16 run goes in rounds (`ELEC_HELPERS`): `host` on the host's pool, set up by the
/// host's own code (`host_pool`, `host_rounds`), `0` stepping whole; unset, `default`. A count
/// is refused: the host's pool sets its helpers, and its planning runs there too.
fn rounds_pool(default: bool) -> Option<&'static RunPool> {
    assert!(std::env::var_os("ELEC_PLAN_POOL").is_none(), "ELEC_PLAN_POOL: the host's pool runs the planning (ELEC_HELPERS=host)");
    let on = match std::env::var("ELEC_HELPERS").as_deref() {
        Err(_) => default,
        Ok("host") => true,
        Ok("0") => false,
        Ok(v) => panic!("ELEC_HELPERS={v}: `host` (in rounds on the host's pool, which sets the helpers) or `0` (whole)"),
    };
    on.then(app_pool)
}

/// LINE of a recorded run on a 48 kHz grid over `[0, secs)`.
fn line_48k(r: &Rec, names: &[String], secs: f64) -> Vec<f64> {
    at_48k(r, names, "LINE", secs).expect("LINE is recorded")
}

/// A recorded net on a 48 kHz grid over `[0, secs)`.
fn at_48k(r: &Rec, names: &[String], name: &str, secs: f64) -> Option<Vec<f64>> {
    let i = names.iter().position(|n| n == name)?;
    let grid: Vec<f64> = (0..(secs * 48e3) as usize).map(|k| k as f64 / 48e3).collect();
    let mut out = Vec::new();
    resample(r, i, &grid, &mut out);
    Some(out)
}

/// The power of `x` (48 kHz) below `hz`, its mean removed, Hann windowed.
fn low_power(x: &[f64], hz: f64) -> f64 {
    let n = x.len().next_power_of_two();
    let mean = x.iter().sum::<f64>() / x.len() as f64;
    let mut data = vec![C64::new(0.0, 0.0); n];
    for (i, &v) in x.iter().enumerate() {
        data[i] = C64::new((v - mean) * Window::Hann.coeff(i, x.len()), 0.0);
    }
    fft(&mut data, false).expect("a zero-padded power-of-two FFT");
    let top = ((hz * n as f64 / 48e3) as usize).min(n / 2);
    data[1..=top].iter().map(|v| v.norm_sqr()).sum()
}

/// How far under `a` the difference `b - a` sits below `hz`, in dB.
fn low_under(a: &[f64], b: &[f64], hz: f64) -> f64 {
    let diff: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    10.0 * (low_power(a, hz) / low_power(&diff, hz)).log10()
}

/// drum16's deterministic voices and the trigger that strikes each.
const VOICE_TRIGGERS: [(&str, &str); 7] = [("KICKOUT", "KICK"), ("LOW", "LTOM"), ("HIGH", "HTOM"), ("snare.body", "SNARE"), ("snare.sw", "SNARE"), ("ENV", "SNARE"), ("OENV", "OHAT")];

/// The run against the whole by window: each voice from strike to strike (the first window from
/// zero), its rms against its range over the run; LINE below 300 Hz from `from` on and by 50 ms
/// window; and the 100 ms after each handover.
fn windowed(base: &Rec, cand: &Rec, names: &[String], cand_names: &[String], secs: f64, from: f64, handovers: &[f64]) {
    let (Some(a), Some(b)) = (at_48k(base, names, "LINE", secs), at_48k(cand, cand_names, "LINE", secs)) else { return };
    let k = |t: f64| ((t * 48e3) as usize).min(a.len());
    println!("  windows: LINE below 300 Hz from {from:.3} s: {:.1} dB under", low_under(&a[k(from)..], &b[k(from)..], 300.0));
    let by: Vec<String> = (0..a.len() / 2400).map(|w| format!("{:.2}:{:.0}", w as f64 * 0.05, low_under(&a[w * 2400..(w + 1) * 2400], &b[w * 2400..(w + 1) * 2400], 300.0))).collect();
    println!("  windows: LINE below 300 Hz by 50 ms (start s: dB under): {}", by.join(" "));
    for (voice, trigger) in VOICE_TRIGGERS {
        let (Some(x), Some(y)) = (at_48k(base, names, voice, secs), at_48k(cand, cand_names, voice, secs)) else { continue };
        let Some(ti) = names.iter().position(|n| n == trigger) else { continue };
        let range = x.iter().fold(f64::MIN, |m, &v| m.max(v)) - x.iter().fold(f64::MAX, |m, &v| m.min(v));
        let lo = (0..base.t.len()).map(|j| base.x[j * base.n + ti]).fold(f64::MAX, f64::min);
        let hi = (0..base.t.len()).map(|j| base.x[j * base.n + ti]).fold(f64::MIN, f64::max);
        let mut edges = vec![0.0];
        edges.extend(rising_edges(base, ti, lo + 0.5 * (hi - lo), 0.0, secs));
        edges.push(secs);
        let rms = |s: usize, e: usize| ((s..e).map(|j| (y[j] - x[j]).powi(2)).sum::<f64>() / (e - s).max(1) as f64).sqrt() / range;
        let parts: Vec<String> = edges.windows(2).map(|w| format!("{:.3}:{:.1e}", w[0], rms(k(w[0]), k(w[1])))).collect();
        let after: Vec<String> = handovers.iter().map(|&h| format!("{h:.3}:{:.1e}", rms(k(h), k(h + 0.1)))).collect();
        println!("  windows: {voice} by strike (start s: rms/range) {}; the 100 ms after each handover {}", parts.join(" "), after.join(" "));
    }
    let after: Vec<String> = handovers.iter().map(|&h| format!("{h:.3}:{:.1}", low_under(&a[k(h)..k(h + 0.1)], &b[k(h)..k(h + 0.1)], 300.0))).collect();
    println!("  windows: LINE below 300 Hz in the 100 ms after each handover (s: dB under) {}", after.join(" "));
}

/// `samples` in volts as a mono 16-bit 48 kHz WAV, `(v - mean) * gain` full scale at one.
fn write_wav(path: &std::path::Path, samples: &[f64], mean: f64, gain: f64) {
    let data: Vec<u8> = samples.iter().flat_map(|&v| ((((v - mean) * gain).clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()).collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(48000u32.to_le_bytes());
    out.extend(96000u32.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    std::fs::write(path, out).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    println!("hear: wrote {}", path.display());
}

/// `ELEC_ACC_EDGES=1` (board-acc): where in time LINE's error against the reference sits. The run's
/// jumps are its own crossings of each edge net's mid level (the metal oscillators, the triggers),
/// on the 1 us grid. Prints LINE's error energy by distance from the nearest jump against the share
/// of the time at that distance; per source, the error locked to its jumps (the error's mean at each
/// lag from -10 to +60 us over them, times their count, against the whole); and the error energy in
/// each 100 ms of the window.
fn error_in_time(base: &Rec, cand: &Rec, names: &[String], cand_names: &[String], grid: &[f64]) {
    let (Some(i), Some(j)) = (names.iter().position(|x| x == "LINE"), cand_names.iter().position(|x| x == "LINE")) else { return };
    let (mut a, mut b, mut v) = (Vec::new(), Vec::new(), Vec::new());
    resample(base, i, grid, &mut a);
    resample(cand, j, grid, &mut b);
    let d: Vec<f64> = a.iter().zip(&b).map(|(x, y)| y - x).collect();
    let n = d.len();
    let total = d.iter().map(|x| x * x).sum::<f64>().max(f64::MIN_POSITIVE);
    // LINE at the run's sample instants alone, what a sink at the rate hears: no line between them
    let rate = sample_rate(0.0);
    if rate > 0.0 && n > 1 {
        let at: Vec<f64> = ((grid[0] * rate).ceil() as i64..=(grid[n - 1] * rate).floor() as i64).map(|k| k as f64 / rate).collect();
        let (mut sa, mut sb) = (Vec::new(), Vec::new());
        resample(base, i, &at, &mut sa);
        resample(cand, j, &at, &mut sb);
        let m = at.len() as f64;
        let mean = sa.iter().sum::<f64>() / m;
        let ac = (sa.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / m).sqrt();
        let rms = (sa.iter().zip(&sb).map(|(x, y)| (y - x) * (y - x)).sum::<f64>() / m).sqrt();
        println!("  error in time, at the {rate} Hz sample instants alone: {} of them, audio_db={:.3}", at.len(), 20.0 * (rms / ac).log10());
    }
    let groups: [(&str, Vec<String>); 2] = [
        ("the metal oscillators", (0..6).map(|k| format!("hats.metal.y[{k}]")).collect()),
        ("the triggers", DRUM_TRIGGERS.iter().map(|s| s.to_string()).collect()),
    ];
    let mut all = Vec::new();
    const BEFORE: usize = 10;
    const AFTER: usize = 60;
    for (group, nets) in &groups {
        let mut edges = Vec::new();
        for net in nets {
            let Some(k) = cand_names.iter().position(|x| x == net) else { continue };
            resample(cand, k, grid, &mut v);
            let (lo, hi) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &x| (l.min(x), h.max(x)));
            let mid = 0.5 * (lo + hi);
            // signed by its direction: an artefact that follows the jump's sign adds up, not cancels
            edges.extend((1..n).filter(|&s| (v[s - 1] - mid) * (v[s] - mid) < 0.0).map(|s| (s, (v[s] - v[s - 1]).signum())));
        }
        let mut mean = vec![0.0; BEFORE + AFTER + 1];
        let mut count = 0usize;
        for &(e, sign) in edges.iter().filter(|&&(e, _)| e >= BEFORE && e + AFTER < n) {
            count += 1;
            for (l, m) in mean.iter_mut().enumerate() {
                *m += sign * d[e - BEFORE + l];
            }
        }
        mean.iter_mut().for_each(|m| *m /= count.max(1) as f64);
        let locked = count as f64 * mean.iter().map(|m| m * m).sum::<f64>();
        let peak = mean.iter().enumerate().max_by(|x, y| x.1.abs().total_cmp(&y.1.abs())).map_or(0, |(l, _)| l as i64) - BEFORE as i64;
        println!("  error in time, {group}: {} jumps; the error locked to them {:.2}% of LINE's error energy, its mean largest {peak:+} us from them", edges.len(), 100.0 * locked / total);
        all.extend(edges.iter().map(|&(e, _)| e));
    }
    all.sort_unstable();
    // each grid point's distance in us to the nearest jump
    let mut dist = vec![usize::MAX; n];
    let mut last = None;
    for s in 0..n {
        if all.binary_search(&s).is_ok() {
            last = Some(s);
        }
        if let Some(l) = last {
            dist[s] = s - l;
        }
    }
    last = None;
    for s in (0..n).rev() {
        if all.binary_search(&s).is_ok() {
            last = Some(s);
        }
        if let Some(l) = last {
            dist[s] = dist[s].min(l - s);
        }
    }
    let bins = [0usize, 5, 10, 25, 50, 100, usize::MAX];
    let shares: Vec<String> = bins.windows(2).map(|w| {
        let (e, c) = (0..n).filter(|&s| dist[s] >= w[0] && dist[s] < w[1]).fold((0.0, 0usize), |(e, c), s| (e + d[s] * d[s], c + 1));
        let hi = if w[1] == usize::MAX { "on".to_string() } else { format!("{}", w[1]) };
        format!("{}-{hi} us: {:.1}% of the energy in {:.1}% of the time", w[0], 100.0 * e / total, 100.0 * c as f64 / n as f64)
    }).collect();
    println!("  error in time, by distance from the nearest jump: {}", shares.join("; "));
    let per = 100_000;
    let profile: Vec<String> = d.chunks(per).map(|c| format!("{:.1}", 100.0 * c.iter().map(|x| x * x).sum::<f64>() / total)).collect();
    println!("  error in time, its energy in each 100 ms (%): {}", profile.join(" "));
}

/// Print the bands on the two arms' common grid and return the largest judged level difference.
fn band_errors(prefix: &str, name: &str, a: &[f64], b: &[f64], dt: f64, window_s: f64, clipped: bool) -> Option<f64> {
    let reference = third_octave(a, dt);
    let run = third_octave(b, dt);
    let diff: Vec<f64> = a.iter().zip(b).map(|(x, y)| y - x).collect();
    let diff = third_octave(&diff, dt);
    let mut band_max: Option<f64> = None;
    let mut closest: Option<(u32, f64)> = None;
    let loudest = THIRD_OCTAVE_HZ.iter().zip(&reference).filter(|(hz, _)| **hz <= BAND_TOP_HZ).filter_map(|(_, v)| *v).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    let bands_judged = window_s >= BAND_MIN_WINDOW_S - 1e-9 && !clipped;
    let note = if clipped {
        ", clipped take, rows void".to_string()
    } else if bands_judged {
        String::new()
    } else {
        format!(", none in a window under {BAND_MIN_WINDOW_S} s")
    };
    println!("  {prefix}spectrum {name}: AC third-octave levels, Hann window; delta is run minus reference; diff is the level of run minus reference, under is how far under the reference it sits; judged to {BAND_TOP_HZ} Hz within {BAND_JUDGED_DB} dB of the loudest band ({loudest:.3} dBV){note}");
    for (band, hz) in THIRD_OCTAVE_HZ.iter().enumerate() {
        if let (Some(reference), Some(run)) = (reference[band], run[band]) {
            // Two silent bands agree; only one silent band gives an infinite difference.
            let delta = if reference == run { 0.0 } else { run - reference };
            let judged = bands_judged && *hz <= BAND_TOP_HZ && reference >= loudest - BAND_JUDGED_DB;
            if judged {
                band_max = Some(band_max.map_or(delta.abs(), |v| v.max(delta.abs())));
            }
            let d = diff[band].unwrap_or(f64::NEG_INFINITY);
            if closest.is_none_or(|(_, under)| reference - d < under) {
                closest = Some((*hz, reference - d));
            }
            println!("    {hz} Hz: ref_dbv={reference:.3} run_dbv={run:.3} delta_db={delta:.3} diff_dbv={d:.3} under_db={:.3}{}", reference - d,
                if judged { "" } else { " (not judged)" });
        } else {
            println!("    {hz} Hz: ref_dbv=n/a run_dbv=n/a delta_db=n/a (no FFT bin in band)");
        }
    }
    if let Some((hz, under)) = closest {
        println!("  {prefix}difference {name}: closest to the reference at {hz} Hz, {under:.1} dB under it");
    }
    band_max
}

fn band_summary(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{v:.3}"))
}

/// Compare both nets on the requested delivered frames, through the arms' common end.
fn take_errors(prefix: &str, base: &[BoardTake; 2], cand: &[BoardTake; 2], start: usize, ends: [usize; 2]) -> [String; 2] {
    std::array::from_fn(|i| {
        let (a, b) = (&base[i].samples, &cand[i].samples);
        let end = ends[i].min(a.len()).min(b.len());
        let frames = end.saturating_sub(start);
        let net = TAKE_NETS[i].0;
        if frames < 2 {
            println!("  {prefix}spectrum {net}: unavailable: fewer than two common frames");
            return band_summary(None);
        }
        let samples = |v: &[f32]| v[start..end].iter().copied().map(f64::from).collect::<Vec<_>>();
        let (a, b) = (samples(a), samples(b));
        let clipped = a.iter().chain(&b).any(|x| x.abs() >= 1.0);
        let max = band_errors(prefix, net, &a, &b, 1.0 / DELIVERED, frames as f64 / DELIVERED, clipped);
        band_summary(max)
    })
}

/// Compare every delivered sample by bits. A missing frame also parts the takes. The maximum
/// amplitude difference is over the frames present on both arms.
fn take_identity(base: &[BoardTake; 2], cand: &[BoardTake; 2], frames: usize) {
    let mut first = None;
    let mut max_abs: Option<f64> = None;
    let lengths = |takes: &[BoardTake; 2]| takes.each_ref().map(|t| t.samples.len());
    for (i, (net, _)) in TAKE_NETS.into_iter().enumerate() {
        let (a, b) = (&base[i].samples, &cand[i].samples);
        let mut mismatch = None;
        for (frame, (&x, &y)) in a.iter().zip(b).enumerate() {
            if mismatch.is_none() && x.to_bits() != y.to_bits() {
                mismatch = Some(frame);
            }
            let diff = (f64::from(x) - f64::from(y)).abs();
            max_abs = Some(max_abs.unwrap_or(0.0).max(diff));
        }
        let mismatch = mismatch.or_else(|| (a.len() != b.len()).then_some(a.len().min(b.len())));
        if let Some(frame) = mismatch {
            if first.is_none_or(|(earliest, _)| frame < earliest) {
                first = Some((frame, net));
            }
        }
    }
    let (reference, candidate) = (lengths(base), lengths(cand));
    if let Some((frame, net)) = first {
        let max = max_abs.map_or_else(|| "n/a".into(), |d| format!("{d:.9e}"));
        println!("take-self equal=false first_frame={frame} net={net} max_abs_common={max} reference_frames={reference:?} candidate_frames={candidate:?}");
    } else if frames == 0 || reference != [frames; 2] {
        println!("take-self equal=unavailable reference_frames={reference:?} candidate_frames={candidate:?} expected_frames={frames} (empty or incomplete take)");
    } else {
        println!("take-self equal=true frames={reference:?}");
    }
}

fn polyline_errors(base: &Rec, cand: &Rec, names: &[String], cand_names: &[String], res: f64) {
    println!();
    println!("board-acc res_us={:.6}: live reltol={:.6e}", res * 1e6, cand.reltol);
    if base.t.len() < 2 || cand.t.len() < 2 {
        println!("board-acc summary res_us={:.6}: unavailable: fewer than two accepted points", res * 1e6);
        return;
    }
    let from = base.t[0].max(cand.t[0]);
    let to = base.t[base.t.len() - 1].min(cand.t[cand.t.len() - 1]);
    let first = (from / 1e-6).ceil();
    let last = (to / 1e-6).floor();
    if to <= from || last <= first {
        println!("board-acc summary res_us={:.6}: unavailable: common window contains fewer than two 1 us grid points", res * 1e6);
        return;
    }
    let grid: Vec<f64> = (0..=(last - first) as usize).map(|k| (first + k as f64) * 1e-6).collect();
    println!("  window_s=[{from:.9e},{to:.9e}] grid_us=1 samples={}; mean is run minus reference", grid.len());
    println!("  zero reference range/RMS: zero error gives 0, nonzero error gives inf");
    let samples = grid.len() as f64;
    let (mut a, mut b, mut endpoints) = (Vec::new(), Vec::new(), Vec::new());
    let mut worst_net: Option<(&str, f64)> = None;
    let mut audio = None;
    let mut band_max: [Option<f64>; 2] = [None; 2];
    let mut worst_edge: Option<(&str, f64)> = None;
    let mut worst_pitch: Option<(&str, f64)> = None;
    let mut worst_step: Option<(&str, f64)> = None;
    let mut worst_on_time: Option<(&str, f64)> = None;
    let events = trigger_events(base, names, from, to);
    let mut edge_lines = Vec::new();
    let mut count_mismatches = 0;
    for (i, name) in names.iter().enumerate() {
        let Some(j) = cand_names.iter().position(|n| n == name) else { continue };
        resample(base, i, &grid, &mut a);
        resample(cand, j, &grid, &mut b);
        // Range and crossing level use the reference's full piecewise-linear waveform over
        // the common window, including its interpolated endpoints, not just grid samples.
        resample(base, i, &[from, to], &mut endpoints);
        let (mut lo, mut hi) = (endpoints[0].min(endpoints[1]), endpoints[0].max(endpoints[1]));
        for (k, &at) in base.t.iter().enumerate() {
            if at >= from && at <= to {
                let v = base.x[k * base.n + i];
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        let range = hi - lo;
        let mean = a.iter().sum::<f64>() / samples;
        let rms = (a.iter().zip(&b).map(|(x, y)| (y - x) * (y - x)).sum::<f64>() / samples).sqrt();
        let normalized = relative_error(rms, range);
        let bias = relative_error(b.iter().sum::<f64>() / samples - mean, range);
        println!("  net {name}: rms/range={normalized:.6e} mean/range={bias:.6e} ref_range={range:.6e}");
        if DRUM_VOICES.contains(&name.as_str()) {
            let (lag, rms) = best_lag(&a, &b, 10, 100);
            println!("  net {name} aligned: best lag {lag} us, rms/range={:.6e}", relative_error(rms, range));
        }
        if worst_net.is_none_or(|(_, value)| normalized > value) {
            worst_net = Some((name, normalized));
        }
        if name == "LINE" {
            let ac_rms = (a.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / samples).sqrt();
            let error = relative_error(rms, ac_rms);
            audio = Some((error, 20.0 * error.log10(), ac_rms));
        }
        if let Some(net) = ["LINE", "NOISE"].iter().position(|n| *n == name.as_str()) {
            band_max[net] = band_errors("", name, &a, &b, 1e-6, to - from, false);
        }
        if DRUM_TRIGGERS.contains(&name.as_str()) || HATS_ENVELOPES.contains(&name.as_str()) || (0..6).any(|k| *name == format!("hats.metal.y[{k}]")) {
            let level = lo + 0.5 * range;
            let reference = rising_edges(base, i, level, from, to);
            let run = rising_edges(cand, j, level, from, to);
            if reference.len() != run.len() {
                count_mismatches += 1;
            }
            let timing = if reference.is_empty() || run.is_empty() {
                "mean_abs_dt_us=n/a max_abs_dt_us=n/a".to_string()
            } else {
                let (mut k, mut sum, mut max) = (0, 0.0, 0.0_f64);
                for &at in &run {
                    while k + 1 < reference.len() && (reference[k + 1] - at).abs() <= (reference[k] - at).abs() {
                        k += 1;
                    }
                    let dt = (reference[k] - at).abs() * 1e6;
                    sum += dt;
                    max = max.max(dt);
                }
                if worst_edge.is_none_or(|(_, value)| max > value) {
                    worst_edge = Some((name, max));
                }
                let hats = ["CHAT", "OHAT"].contains(&name.as_str()) || HATS_ENVELOPES.contains(&name.as_str());
                if hats && worst_on_time.is_none_or(|(_, value)| max > value) {
                    worst_on_time = Some((name, max));
                }
                format!("mean_abs_dt_us={:.6e} max_abs_dt_us={max:.6e}", sum / run.len() as f64)
            };
            // a self-timed oscillator: its pitch between the triggers and its phase steps at them
            let timing = match name.starts_with("hats.metal.y[").then(|| edge_pitch(&reference, &run, &events)) {
                Some(Some((cents, step, scatter))) => {
                    if worst_pitch.is_none_or(|(_, value)| cents.abs() > value.abs()) {
                        worst_pitch = Some((name, cents));
                    }
                    if let Some(us) = step.filter(|&us| worst_step.is_none_or(|(_, value)| us.abs() > value.abs())) {
                        worst_step = Some((name, us));
                    }
                    let hz = (reference.len() - 1) as f64 / (reference[reference.len() - 1] - reference[0]);
                    let step = step.map_or_else(|| "n/a".to_string(), |us| format!("{us:.1}"));
                    format!("{timing} ref_hz={hz:.3} pitch_cents={cents:.3} phase_step_us={step} scatter_us={scatter:.2} events={}", events.len())
                }
                Some(None) => format!("{timing} pitch_cents=n/a phase_step_us=n/a"),
                None => timing,
            };
            edge_lines.push(format!("  edge {name}: ref_count={} run_count={} level={level:.6e} {timing}", reference.len(), run.len()));
        }
    }
    let audio_summary = if let Some((error, db, ac_rms)) = audio {
        println!("  audio LINE: rms/ref_ac_rms={error:.6e} error_db={db:.3} ref_ac_rms={ac_rms:.6e}");
        format!("{error:.6e} audio_db={db:.3}")
    } else {
        println!("  audio LINE: unavailable");
        "n/a audio_db=n/a".to_string()
    };
    for line in edge_lines {
        println!("{line}");
    }
    let hits = hit_figures(base, cand, names, cand_names, &grid, from, to);
    let net_summary = worst_net.map_or_else(|| "n/a rms/range=n/a".to_string(), |(name, value)| format!("{name} rms/range={value:.6e}"));
    let edge_summary = worst_edge.map_or_else(|| "n/a max_abs_dt_us=n/a".to_string(), |(name, value)| format!("{name} max_abs_dt_us={value:.6e}"));
    let [line_band, noise_band] = band_max.map(band_summary);
    let pitch_summary = worst_pitch.map_or_else(|| "n/a cents=n/a".to_string(), |(name, value)| format!("{name} cents={value:.3}"));
    let step_summary = worst_step.map_or_else(|| "n/a us=n/a".to_string(), |(name, value)| format!("{name} us={value:.1}"));
    println!("board-acc summary res_us={:.6}: worst_net={net_summary} audio={audio_summary} line_band_max_abs_db={line_band} noise_band_max_abs_db={noise_band} worst_edge={edge_summary} worst_pitch={pitch_summary} worst_phase_step={step_summary} edge_count_mismatches={count_mismatches}", res * 1e6);
    let verdict = |ok: bool| if ok { "PASS" } else { "FAIL" };
    let on_time = worst_on_time.map_or_else(|| "on_time=n/a".to_string(), |(name, us)| format!("on_time={name} {us:.3} us (bar {HIT_ON_TIME_US}) -> {}", verdict(us <= HIT_ON_TIME_US)));
    let per_hit = match hits {
        Some((band, excess, n)) => format!("per_hit={band:.3} dB (bar {HIT_BAND_DB}) -> {} spread_excess={} hits={n}", verdict(band <= HIT_BAND_DB),
            excess.map_or_else(|| "n/a".to_string(), |x| format!("{x:+.3} dB (bar {HIT_SPREAD_DB}) -> {}", verdict(x <= HIT_SPREAD_DB)))),
        None => "per_hit=n/a spread_excess=n/a hits=0".to_string(),
    };
    println!("board-acc hits res_us={:.6}: {on_time} {per_hit}", res * 1e6);
    if std::env::var_os("ELEC_ACC_EDGES").is_some() {
        error_in_time(base, cand, names, cand_names, &grid);
    }
}

/// The hit bar's figures (`HIT_ON_TIME_US`) on HATS, run against reference: a hit is a CHAT or
/// OHAT pulse's first edge, its window from 1 ms before it to the next hit, at most 100 ms. Prints
/// each hit's largest band difference (third octaves within `BAND_JUDGED_DB` of the hit's loudest,
/// to `BAND_TOP_HZ`), from `HIT_FROM_HZ` up and below it, and per pad each next hit's largest band
/// change from the hit before, in the reference and in the run. Returns the largest difference
/// from `HIT_FROM_HZ` up, the largest excess of the run's change over the reference's there (None
/// without two hits of a pad), and the hits; None without HATS or a hit. O(hits x the window's FFT).
fn hit_figures(base: &Rec, cand: &Rec, names: &[String], cand_names: &[String], grid: &[f64], from: f64, to: f64) -> Option<(f64, Option<f64>, usize)> {
    let take = |r: &Rec, ns: &[String]| -> Option<Vec<f64>> {
        let i = ns.iter().position(|n| n == "HATS")?;
        let mut out = Vec::new();
        resample(r, i, grid, &mut out);
        Some(out)
    };
    let (a, b) = (take(base, names)?, take(cand, cand_names)?);
    let at = |t: f64| (((t - grid[0]) * 1e6).round().max(0.0) as usize).min(grid.len() - 1);
    let mut hits: Vec<(f64, &str)> = Vec::new();
    for pad in ["CHAT", "OHAT"] {
        // the pad's own trigger edges; a pulse's second edge is no hit
        let only: Vec<String> = names.iter().map(|n| if n == pad { n.clone() } else { String::new() }).collect();
        let mut last = f64::NEG_INFINITY;
        for e in trigger_events(base, &only, from, to) {
            if e - last > 20e-3 {
                hits.push((e, pad));
            }
            last = e;
        }
    }
    hits.sort_by(|x, y| x.0.total_cmp(&y.0));
    // the largest band difference of `y` from `x`, from `HIT_FROM_HZ` up and below, and its band
    let differ = |x: &[f64], y: &[f64]| -> ((f64, u32), (f64, u32)) {
        let (rx, ry) = (third_octave(x, 1e-6), third_octave(y, 1e-6));
        let loudest = THIRD_OCTAVE_HZ.iter().zip(&rx).filter(|(hz, _)| **hz <= BAND_TOP_HZ).filter_map(|(_, v)| *v).filter(|v| v.is_finite()).fold(f64::NEG_INFINITY, f64::max);
        let (mut high, mut low) = ((0.0, 0), (0.0, 0));
        for (band, &hz) in THIRD_OCTAVE_HZ.iter().enumerate() {
            let (Some(u), Some(v)) = (rx[band], ry[band]) else { continue };
            if hz > BAND_TOP_HZ || u < loudest - BAND_JUDGED_DB {
                continue;
            }
            let d = if u == v { 0.0 } else { (v - u).abs() };
            let side = if hz >= HIT_FROM_HZ { &mut high } else { &mut low };
            if d > side.0 {
                *side = (d, hz);
            }
        }
        (high, low)
    };
    let mut windows: Vec<(&str, usize, usize)> = Vec::new();
    let mut worst = 0.0f64;
    for (h, &(e, pad)) in hits.iter().enumerate() {
        let end = hits.get(h + 1).map_or(to, |x| x.0).min(e + 0.1).min(to);
        let (lo, hi) = (at(e - 1e-3), at(end));
        if hi < lo + 5000 {
            continue;
        }
        let ((high, high_hz), (low, low_hz)) = differ(&a[lo..hi], &b[lo..hi]);
        worst = worst.max(high);
        println!("  hit {pad} at {e:.6} s: hats bands run-ref, {HIT_FROM_HZ} to {BAND_TOP_HZ} Hz at most {high:.3} dB ({high_hz} Hz); below, {low:.3} dB ({low_hz} Hz), not judged");
        windows.push((pad, lo, hi - lo));
    }
    if windows.is_empty() {
        return None;
    }
    let mut excess: Option<f64> = None;
    for pad in ["CHAT", "OHAT"] {
        let mine: Vec<&(&str, usize, usize)> = windows.iter().filter(|w| w.0 == pad).collect();
        for w in mine.windows(2) {
            let len = w[0].2.min(w[1].2);
            let ((rh, _), (rl, _)) = differ(&a[w[0].1..w[0].1 + len], &a[w[1].1..w[1].1 + len]);
            let ((uh, _), (ul, _)) = differ(&b[w[0].1..w[0].1 + len], &b[w[1].1..w[1].1 + len]);
            excess = Some(excess.map_or(uh - rh, |x| x.max(uh - rh)));
            println!("  hit to hit {pad} at {:.6} s: the largest band change, {HIT_FROM_HZ} to {BAND_TOP_HZ} Hz: reference {rh:.3} dB, run {uh:.3} dB; below, reference {rl:.3} dB, run {ul:.3} dB, not judged", grid[w[1].1]);
        }
    }
    Some((worst, excess, windows.len()))
}

fn board_errors(base: &Rec, cand: &Rec, names: &[String], cand_names: &[String], res: f64) {
    polyline_errors(base, cand, names, cand_names, res);
    if let (Some(reference), Some(run)) = (&base.takes, &cand.takes) {
        let res_us = res * 1e6;
        let clock = cand.take_clock;
        let frame = |t: f64| (t * DELIVERED).ceil() as usize;
        let judged_from_s = clock.t_plan.map_or(0.0, |t| t + (BAND_LAG as usize + 2 * BAND_HALF + 1) as f64 * clock.period);
        let start = frame(judged_from_s);
        let ends = std::array::from_fn(|i| reference[i].samples.len().min(run[i].samples.len()));
        if let Some(t_plan) = clock.t_plan {
            let [line, noise] = take_errors("take start ", reference, run, 0, [frame(t_plan); 2]);
            println!("take-start summary res_us={res_us:.6}: line_band_max_abs_db={line} noise_band_max_abs_db={noise} t_plan_s={t_plan:.9} (diagnosis only)");
        }
        let [line, noise] = take_errors("take ", reference, run, start, ends);
        println!("take-judge summary res_us={res_us:.6}: line_band_max_abs_db={line} noise_band_max_abs_db={noise} delivered_hz={DELIVERED:.0} judged_from_s={judged_from_s:.9}");
        // K5 is a comparison within one run. Print it once even when grades are also compared.
        if let (None, Some(control)) = (&base.control, &cand.control) {
            let [line, noise] = take_errors("control ", control, run, start, ends);
            println!("take-control summary res_us={res_us:.6}: line_band_max_abs_db={line} noise_band_max_abs_db={noise} judged_from_s={judged_from_s:.9}");
        }
    }
}

fn board_accuracy(secs: f64, ress: &[f64], ref_reltol: f64) {
    let mut wanted: Vec<String> = ["LINE", "SUM", "VREF", "VA", "KICKOUT", "LOW", "HIGH", "HATS", "NOISE"]
        .into_iter().chain(DRUM_TRIGGERS).chain(HATS_ENVELOPES).map(String::from).collect();
    wanted.extend((0..6).map(|i| format!("hats.metal.y[{i}]")));
    wanted.sort();
    let take = std::env::var("ELEC_TAKE").as_deref() == Ok("1");
    let divisor = FAST_RELTOL / ref_reltol;
    println!("board-acc reference: secs={secs:.9e} res_us=0 reltol={ref_reltol:.6e} vntol_divisor={divisor:.6e} abstol_divisor={divisor:.6e}");
    let reference = || drum(secs, DrumRun {
        reltol: Some(ref_reltol), tolerance_divisors: Some((divisor, divisor)), record: Some(&wanted), reference: true, take, ..DrumRun::default()
    });
    let (base, names) = reference();
    if take && std::env::var("ELEC_TAKE_SELF").as_deref() == Ok("1") {
        let (cand, cand_names) = reference();
        board_errors(&base, &cand, &names, &cand_names, 0.0);
        take_identity(base.takes.as_ref().expect("the reference recorded"), cand.takes.as_ref().expect("the second reference recorded"),
            (secs * DELIVERED).floor() as usize);
        return;
    }
    // Only request resolved names again, so an absent net is reported once for the whole series.
    // `ELEC_SINGLE=<grade,..>`: each grade against the reference, and each after the first
    // against the first
    // `ELEC_ROUNDS=<ms>`: the candidate in rounds from then on (the board's own plan)
    let rounds = std::env::var("ELEC_ROUNDS").ok().and_then(|v| v.parse::<f64>().ok()).map(|ms| ms * 1e-3);
    // `ELEC_CARRY=1` (with `ELEC_ROUNDS` and `ELEC_SET`): before each candidate, the board planned
    // at its stock values, its cut kept; the candidate, at `ELEC_SET`'s values as the reference
    // is, begins on that cut as a heard start at new part values does, and keeps it to its end
    let carry = std::env::var_os("ELEC_CARRY").is_some();
    assert!(!carry || (rounds.is_some() && std::env::var_os("ELEC_SET").is_some()), "ELEC_CARRY: with ELEC_ROUNDS and ELEC_SET");
    for &res in ress {
        if carry && res > 0.0 {
            println!("board-acc carry source: the board at its stock values, res {res} us");
            let _ = drum(secs, DrumRun { res: res * 1e-6, record: Some(&names), rounds, carry_source: true, ..DrumRun::default() });
        }
        let mut first = None;
        for single in single_grades() {
            if let Some(g) = single {
                println!("board-acc single pass {g}:");
            }
            let (cand, cand_names) = drum(secs, DrumRun { res: res * 1e-6, record: Some(&names), single, rounds, take, ..DrumRun::default() });
            board_errors(&base, &cand, &names, &cand_names, res * 1e-6);
            match &first {
                None => first = Some((cand, cand_names, single)),
                Some((f, f_names, g0)) => {
                    println!("board-acc single pass {single:?} against {g0:?}:");
                    board_errors(f, &cand, f_names, &cand_names, res * 1e-6);
                }
            }
        }
    }
}

fn partitions(b: &Board) {
    use fab_elec_sim::analysis::partition::{partition, PartOpts};
    let opts = |fmax: f64, control: Vec<String>| PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: fab_elec_sim::analysis::round::CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax, band: f64::INFINITY, per_octave: 4, min_island: 8, control, supplies: Vec::new(), heard: Vec::new(), round: 16.0 / fmax };
    let report = |title: &str, c: &SimCircuit, o: PartOpts| {
        let t = Instant::now();
        let p = partition(c, &o);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        match &p {
            Ok(p) => print!("{}", p.report(title)),
            Err(e) => println!("partition {title}: {}", e.message),
        }
        println!("  (partitioned in {ms:.1} ms)");
        p.ok()
    };
    let pads: Vec<String> = b.pins.iter().map(|p| p.pad.clone()).collect();
    let fmax = 0.5 / LiveSpec::default().max_step;
    report("ledavr chase, every pad in the control set", &b.circuit, opts(fmax, pads));
    report("ledavr chase, no control set", &b.circuit, opts(fmax, Vec::new()));
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    for bench in &ev.benches {
        let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
        // the band to half the live step (drum16's audio sample period bounds it), the MCU's
        // pads as the control set
        let step = LiveSpec::default().max_step.min(1.0 / AUDIO_SAMPLE_RATE);
        let pads: Vec<String> = lowered.firmware.iter().flat_map(|fw| lowered.cosim_pins(&fw.part)).map(|p| p.pad).collect();
        let title = format!("drum16 {} bench, step {:.1} us, {} pads in the control set", bench.name, step * 1e6, pads.len());
        let Some(p) = report(&title, &lowered.circuit, opts(0.5 / step, pads)) else { continue };
        // the shared nets as an exact border instead of weak cuts
        for border in [&["VREF"][..], &["VREF", "VA", "VBUS"], &["VREF", "VA", "VBUS", "SUM"], &["VREF", "VA", "VBUS", "SUM", "ACCENT"]] {
            match p.border_view(border) {
                Ok(view) => print!("{view}"),
                Err(e) => println!("border {border:?}: {e}"),
            }
        }
    }
}

/// `held [shares]`: the held step (`analysis::held`) and the round plan it gives (`analysis::round`).
/// drum16's board live from zero
/// (the MCU and its registers attached, Fast at the live default): at its operating point, after
/// the firmware set its pins, inside the first step's trigger pulses and while that step's voices
/// ring. Then drum16's voice benches and the tone box at their operating points, and ledavr live
/// at its operating point and once the chase runs. `shares` adds each searched node's shares.
fn helds(b: &Board, shares: bool) {
    use fab_elec_sim::analysis::held::held;
    use fab_elec_sim::analysis::op::operating_point;
    use fab_elec_sim::analysis::partition::PartOpts;
    use fab_elec_sim::analysis::round::{circuit_opts, plan_blocks};
    use fab_elec_sim::{SimCircuit, Solver};
    struct Nothing;
    impl Sink for Nothing {
        fn point(&mut self, _at: f64, _outer: Option<f64>, _x: &[f64]) {}
    }
    let opts = |fmax: f64, control: Vec<String>| PartOpts { never_cut: Vec::new(), land: Vec::new(), cut_tol: fab_elec_sim::analysis::round::CUT_TOL, hub_tol: 0.1, fmin: 10.0, fmax, band: f64::INFINITY, per_octave: 4, min_island: 8, control, supplies: Vec::new(), heard: Vec::new(), round: 16.0 / fmax };
    let show = |title: &str, ckt: &SimCircuit, s: &Solver, t: f64, o: &PartOpts| {
        match held(s, t, &circuit_opts(ckt, o)) {
            Ok(h) => {
                print!("{}", h.report(title));
                if shares {
                    print!("{}", h.share_table(8));
                }
            }
            Err(e) => println!("held {title}: {}", e.message),
        }
        match plan_blocks(ckt, s, t, o) {
            Ok(Some(plan)) => print!("{}", plan.report),
            Ok(None) => println!("{title}: no round plan"),
            Err(e) => println!("round plan {title}: {}", e.message),
        }
    };
    let high = |s: &Solver, pads: &[String]| pads.iter().filter(|p| s.unknown(p).is_some_and(|u| s.x[u as usize] > 2.5)).count();
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("drum16 evaluates: {d:?}"));
    {
        let bench = ev.benches.iter().find(|b| b.name == "board").expect("drum16 has the board bench");
        let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
        let fw = &lowered.firmware[0];
        let params: Vec<(&str, f64)> = fw.params.iter().map(|(n, v)| (n.as_str(), *v)).collect();
        let image = fab_elec_mcu_emit_avr::firmware(DRUM_FW, fw.f_clk.expect("a clock"), &params).unwrap_or_else(|d| panic!("the firmware builds: {d:?}"));
        let pins = lowered.cosim_pins(&fw.part);
        let machine = LogicMachine::new(AvrMachine::new(&image.words, u64::from(image.f_clk)), fw.logic.plan()).expect("the registers join the MCU");
        let device = CoMachine::new(machine, Config { stretch: 4000, debug: None }, &pins).expect("the co-simulated MCU");
        let spec = LiveSpec { rate: f64::INFINITY, trace: false, sample_rate: sample_rate(AUDIO_SAMPLE_RATE), ..LiveSpec::default() };
        let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec).unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
        let o = opts(0.5 / sim.step_cap(), pins.iter().map(|p| p.pad.clone()).collect());
        sim.attach(Some(CoSimDevice { dev: Box::new(device), pins: pins.clone() }));
        for (t, what) in [(0.0, "at its operating point"), (0.002, "after the pin setup"), (0.1255, "inside the first step's trigger pulses"), (0.130, "while the first step's voices ring")] {
            while sim.t() < t {
                sim.advance(t, None, &mut Nothing).expect("the board steps");
            }
            println!();
            println!("== drum16 board {what}: {} of {} pads above 2.5 V", high(sim.solver(), &o.control), o.control.len());
            show("drum16 board", &lowered.circuit, sim.solver(), sim.t(), &o);
        }
    }
    for bench in ev.benches.iter().filter(|b| b.name != "board") {
        let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
        let mut s = Solver::new(&lowered.circuit).expect("the bench builds");
        operating_point(&mut s, false).expect("the bench's operating point");
        let step = LiveSpec::default().max_step.min(1.0 / AUDIO_SAMPLE_RATE);
        println!();
        show(&format!("drum16 {} bench", bench.name), &lowered.circuit, &s, 0.0, &opts(0.5 / step, Vec::new()));
    }
    let sources = fab_elec_fixtures::docs("tonebox").expect("the tone box is a stock project");
    let ev = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("the tone box evaluates: {d:?}"));
    for bench in &ev.benches {
        let lowered = lower(&ev.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
        let mut s = Solver::new(&lowered.circuit).expect("the bench builds");
        operating_point(&mut s, false).expect("the bench's operating point");
        let step = LiveSpec::default().max_step.min(1.0 / AUDIO_SAMPLE_RATE);
        println!();
        show(&format!("tonebox {} bench", bench.name), &lowered.circuit, &s, 0.0, &opts(0.5 / step, Vec::new()));
    }
    let spec = LiveSpec { rate: f64::INFINITY, trace: false, sample_rate: sample_rate(0.0), ..LiveSpec::default() };
    let mut sim = LiveSim::new(b.circuit.clone(), b.plan.clone(), b.generation, spec).expect("ledavr's operating point");
    let o = opts(0.5 / sim.step_cap(), b.pins.iter().map(|p| p.pad.clone()).collect());
    let co = CoMachine::new(AvrMachine::new(&b.words, b.hz), Config { stretch: 4000, debug: Some(b.debug.clone()) }, &b.pins).expect("a device");
    sim.attach(Some(CoSimDevice { dev: Box::new(co), pins: b.pins.clone() }));
    for (t, what) in [(0.0, "at its operating point"), (0.05, "with the chase running")] {
        while sim.t() < t {
            sim.advance(t, None, &mut Nothing).expect("ledavr steps");
        }
        println!();
        println!("== ledavr {what}: {} of {} pads above 2.5 V", high(sim.solver(), &o.control), o.control.len());
        show("ledavr chase", &b.circuit, sim.solver(), sim.t(), &o);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let what = args.first().map_or("all", String::as_str);
    if what == "take-read" {
        let secs = args.get(1).map_or("1.0", String::as_str).parse().expect("take-read seconds must be a number");
        take_read(secs);
        return;
    }
    if what == "rc-read" {
        let secs = args.get(1).map_or("1.0", String::as_str).parse().expect("rc-read seconds must be a number");
        rc_read(secs);
        return;
    }
    let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.1);
    let modes: &[bool] = match args.get(2).map_or("both", String::as_str) {
        "chase" => &[false],
        "breathe" => &[true],
        _ => &[false, true],
    };
    if what == "board-acc" {
        let secs: f64 = args.get(1).map_or("0.2", String::as_str).parse().expect("board-acc seconds must be a number");
        let ress: Vec<f64> = args.get(2).map_or("0,100", String::as_str).split(',')
            .map(|v| v.parse().expect("board-acc resolutions must be numbers in microseconds")).collect();
        let ref_reltol: f64 = args.get(3).map_or("1e-4", String::as_str).parse().expect("board-acc reference reltol must be a number");
        assert!(secs.is_finite() && secs > 0.0, "board-acc seconds must be finite and positive");
        assert!(ress.iter().all(|r| r.is_finite() && *r >= 0.0), "board-acc resolutions must be finite and nonnegative");
        assert!(ref_reltol.is_finite() && ref_reltol > 0.0 && ref_reltol <= FAST_RELTOL,
            "board-acc reference reltol must be positive and no greater than FAST_RELTOL");
        assert!((FAST_RELTOL / ref_reltol).is_finite(), "board-acc reference tolerance divisor must be finite");
        board_accuracy(secs, &ress, ref_reltol);
        return;
    }
    if what == "hear" {
        // drum16's board whole against its own round plans, one per case
        // `<declared>@<round>[@<plan ms>]` (`all` the board's declared supplies, `none`, or nets
        // joined by `+`; the round in sample periods; the plan at `plan_at` ms unless the case
        // says), and with `pair` whole against whole at half the reltol (what the simulation
        // itself moves); each also by window. With DEVHUB_KEEP set, LINE of each run, and of each
        // round plan minus the whole, goes there as a 48 kHz WAV, all at the whole run's gain:
        // `hear <secs> <res us> <plan_at ms> <case,..> [<net+..>]`, the nets traced side by side
        // over the 130 ms after each case's first handover
        let res: f64 = args.get(2).map_or("100", String::as_str).parse::<f64>().expect("hear: the resolution in us") * 1e-6;
        let plan_at: f64 = args.get(3).map_or("2", String::as_str).parse::<f64>().expect("hear: the plan's time in ms") * 1e-3;
        let list = args.get(4).map_or("all@32,pair", String::as_str);
        let pair = list.split(',').any(|c| c == "pair");
        let cases: Vec<(Option<Vec<String>>, u32, f64, String)> = list.split(',').filter(|&c| c != "pair").map(|case| {
            let mut parts = case.split('@');
            let declared = parts.next().expect("hear: a case is <declared>@<round>[@<plan ms>]");
            let round = parts.next().expect("hear: a case is <declared>@<round>[@<plan ms>]");
            let at = parts.next().map_or(plan_at, |ms| ms.parse::<f64>().expect("hear: the case's plan time in ms") * 1e-3);
            let only = match declared {
                "all" => None,
                "none" => Some(Vec::new()),
                nets => Some(nets.split('+').map(String::from).collect()),
            };
            (only, round.parse().expect("hear: a round length in samples"), at, format!("{declared}-{round}-{:.0}ms", at * 1e3).replace('+', "_"))
        }).collect();
        let mut wanted: Vec<String> = ["LINE", "SUM", "VREF", "VA", "VBUS", "HATS", "NOISE"].into_iter().chain(DRUM_VOICES).chain(DRUM_TRIGGERS).map(String::from).collect();
        wanted.extend((0..6).map(|i| format!("hats.metal.y[{i}]")));
        // `<net+..>` after the cases: those nets too, printed side by side after each case's first handover
        let trace: Vec<String> = args.get(5).map(|v| v.split('+').map(String::from).collect()).unwrap_or_default();
        wanted.extend(trace.iter().cloned());
        wanted.sort();
        wanted.dedup();
        let keep = std::env::var_os("DEVHUB_KEEP").map(std::path::PathBuf::from);
        let (base, names) = drum(secs, DrumRun { res, record: Some(&wanted), ..DrumRun::default() });
        let whole = line_48k(&base, &names, secs);
        let mean = whole.iter().sum::<f64>() / whole.len() as f64;
        let gain = 0.5 / whole.iter().fold(0.0f64, |m, &v| m.max((v - mean).abs()));
        if let Some(dir) = &keep {
            write_wav(&dir.join("line-whole.wav"), &whole, mean, gain);
        }
        for (only, round, at, label) in &cases {
            println!("hear: drum16 for {secs} s at res {:.0} us, whole against in rounds of {round} samples from {:.1} ms, declared: {}", res * 1e6, at * 1e3,
                only.as_ref().map_or("the board's own".to_string(), |o| if o.is_empty() { "none".to_string() } else { o.join(", ") }));
            let times = Arc::new(Mutex::new(Vec::new()));
            let (cand, cand_names) = drum(secs, DrumRun { res, record: Some(&names), rounds: Some(*at), round: Some(*round), declared: only.clone(), handovers: Some(times.clone()), ..DrumRun::default() });
            board_errors(&base, &cand, &names, &cand_names, res);
            let times = times.lock().expect("the handover times").clone();
            windowed(&base, &cand, &names, &cand_names, secs, *at, &times);
            if let Some(&h) = times.first().filter(|_| !trace.is_empty()) {
                use std::fmt::Write as _;
                let grid: Vec<f64> = (0..40).map(|k| h + k as f64 * res).chain((4..30).map(|k| h + k as f64 * 1e-3)).chain((6..28).map(|k| h + k as f64 * 5e-3)).collect();
                let (mut x, mut y) = (Vec::new(), Vec::new());
                let cols: Vec<(usize, usize)> = trace.iter().filter_map(|n| Some((names.iter().position(|m| m == n)?, cand_names.iter().position(|m| m == n)?))).collect();
                let shown: Vec<&str> = cols.iter().map(|&(i, _)| names[i].as_str()).collect();
                println!("  trace after the handover at {h:.4} s: t ms, then whole/rounds per net: {}", shown.join(", "));
                let mut rows = vec![String::new(); grid.len()];
                for &(i, j) in &cols {
                    resample(&base, i, &grid, &mut x);
                    resample(&cand, j, &grid, &mut y);
                    for (k, row) in rows.iter_mut().enumerate() {
                        let _ = write!(row, " {:.6}/{:.6}", x[k], y[k]);
                    }
                }
                for (k, row) in rows.iter().enumerate() {
                    println!("  trace {:.3}{row}", grid[k] * 1e3);
                }
            }
            if let Some(dir) = &keep {
                let line = line_48k(&cand, &cand_names, secs);
                write_wav(&dir.join(format!("line-{label}.wav")), &line, mean, gain);
                let diff: Vec<f64> = line.iter().zip(&whole).map(|(x, w)| x - w).collect();
                write_wav(&dir.join(format!("line-{label}-minus-whole.wav")), &diff, 0.0, gain);
            }
        }
        if pair {
            println!("hear: drum16 for {secs} s at res {:.0} us, whole against whole at half the reltol", res * 1e6);
            let (cand, cand_names) = drum(secs, DrumRun { res, record: Some(&names), reltol: Some(FAST_RELTOL / 2.0), ..DrumRun::default() });
            board_errors(&base, &cand, &names, &cand_names, res);
            windowed(&base, &cand, &names, &cand_names, secs, 0.0, &[]);
        }
        return;
    }
    if what == "drumavr" {
        drum_avr(secs);
        return;
    }
    if what == "board-tape" {
        // the hearing recorder's cost: `board-tape <secs> <res us> [once]`, the jack alone against
        // the jack and eight probes, alternating, twice (`once`: the eight once, and the held
        // Listen against the jack's first channel recorded alone)
        let res = args.get(2).and_then(|v| v.parse::<f64>().ok()).unwrap_or(100.0) * 1e-6;
        if args.get(3).map(String::as_str) == Some("once") {
            drum_tape(secs, res, &TAPE_PROBES, true);
            return;
        }
        for _ in 0..2 {
            for probes in [&[][..], &TAPE_PROBES[..]] {
                drum_tape(secs, res, probes, false);
            }
        }
        return;
    }
    if what == "board-edit" {
        // a resistor edited while drum16 plays: `board-edit <secs> <res us,..> <resistor> <ohms> <at s> <behind s>`
        let arg = |k: usize, d: &str| args.get(k).map_or(d.to_string(), String::clone);
        let num = |k: usize, d: &str| arg(k, d).parse::<f64>().unwrap_or_else(|_| panic!("board-edit: argument {k} must be a number"));
        let ress: Vec<f64> = arg(2, "0,100").split(',').map(|v| v.parse::<f64>().expect("board-edit: resolutions in us") * 1e-6).collect();
        drum_edit(num(1, "0.4"), &ress, &arg(3, "r_hats"), num(4, "10e3"), num(5, "0.2"), num(6, "0.05"));
        return;
    }
    if what == "board-tap" {
        // drum16 in rounds (the board's planner) at rest and with keys pressed, two passes
        // interleaved: `board-tap <secs> <res us> <from s> <for s> <presses a second> <hold ms>
        // <keys>`. The window [from, from + for] is timed on its own: its realtime factor is what
        // a person pressing pads hears keep up or not (`ELEC_RATE`, `ELEC_HELPERS` as for `board`)
        let arg = |k: usize, d: &str| args.get(k).map_or(d.to_string(), String::clone);
        let num = |k: usize, d: &str| arg(k, d).parse::<f64>().unwrap_or_else(|_| panic!("board-tap: argument {k} is a number"));
        let (secs, res, from, span, rate, hold) = (num(1, "2.5"), num(2, "100") * 1e-6, num(3, "0.3"), num(4, "2.0"), num(5, "6"), num(6, "80") * 1e-3);
        let pressed: Vec<usize> = arg(7, "0,5,10").split(',').map(|k| k.parse().expect("board-tap: the keys are numbers")).collect();
        let run = rounds_pool(false);
        let spec = LiveSpec { rate: f64::INFINITY, trace: false, res, sample_rate: drum_rate(false), ..LiveSpec::default() };
        let band = heard_band(spec.sample_rate);
        println!("board-tap: keys {pressed:?} pressed {rate} times a second for {hold} s each from {from} s for {span} s; Fast res {:.0} us, {:.0} Hz",
            res * 1e6, spec.sample_rate);
        for pass in 1..=2 {
            for presses in [false, true] {
                let label = if presses { "pressed" } else { "rest" };
                let (board, device) = drum_board();
                let mut sim = LiveSim::new(board.circuit.clone(), board.plan.clone(), board.index.generation, spec.clone()).unwrap_or_else(|e| panic!("the board's operating point: {}", e.message));
                sim.attach(Some(device));
                // the helpers first, as the host gives them: the first plan waits for the firmware's set-up
                match run {
                    Some(run) => host_rounds(&mut sim, run.helpers(), run.background(), run.checks(), Default::default(), band),
                    None => sim.set_planner(Some(Box::new(fab_elec_sim::analysis::round::BlockPlanner::for_run(&sim, band)))),
                }
                .unwrap_or_else(|d| panic!("the round plan installs: {d}"));
                if pass == 1 && !presses {
                    println!("  {}", setup_line(&sim, band, &app_pool().name()));
                }
                if presses {
                    press_keys(&mut sim, &board.index, (from, span, rate, hold), &pressed);
                }
                let mut rec = Rec::default();
                let mut walls = Vec::new();
                for stop in [from, from + span, secs] {
                    let start = Instant::now();
                    while sim.t() < stop {
                        sim.advance(stop, Some(Duration::from_millis(8)), &mut rec).unwrap_or_else(|d| panic!("{label}: the board steps: {d}"));
                    }
                    walls.push(start.elapsed().as_secs_f64());
                }
                let whole: f64 = walls.iter().sum();
                println!("board-tap pass {pass} {label}: the window {span} s in {:.3} s wall (x{:.3}); the whole {secs} s in {whole:.2} s (x{:.3}); {} plans ({} kept), {} blocks",
                    walls[1], span / walls[1], secs / whole, sim.stats().plans, sim.stats().plans_kept, sim.blocks());
            }
        }
        return;
    }
    if what == "host-edit" {
        let secs: f64 = args.get(1).map_or("2", String::as_str).parse().expect("host-edit seconds must be a number");
        let watch: f64 = args.get(2).map_or("10", String::as_str).parse().expect("host-edit's watch must be a number");
        let part = args.get(3).map_or(".dec", String::as_str);
        let live_edit = match args.get(4).map(String::as_str) {
            None | Some("live") => true,
            Some("stopped") => false,
            Some(other) => panic!("host-edit's fifth argument is `live` or `stopped`, not `{other}`"),
        };
        assert!(secs.is_finite() && secs > 0.0 && watch.is_finite() && watch >= 0.0, "host-edit takes positive seconds and a watch");
        host_edit(secs, watch, part, live_edit);
        return;
    }
    if what == "host-start" {
        let secs: f64 = args.get(1).map_or("2", String::as_str).parse().expect("host-start seconds must be a number");
        let wait: f64 = args.get(2).map_or("3", String::as_str).parse().expect("host-start's wait must be a number");
        assert!(secs.is_finite() && secs > 0.0 && wait.is_finite() && wait >= 0.0, "host-start takes positive seconds and a wait");
        let opened = match args.get(3).map(String::as_str) {
            None => false,
            Some("opened") => true,
            Some(other) => panic!("host-start's fourth argument is `opened`, not `{other}`"),
        };
        host_start(secs, wait, opened);
        return;
    }
    if what == "start" {
        let secs: f64 = args.get(1).map_or("4", String::as_str).parse().expect("start seconds must be a number");
        assert!(secs.is_finite() && secs > 0.0, "start seconds must be finite and positive");
        drum_start(secs);
        return;
    }
    if what == "board" {
        // drum16's whole board: `board <secs> <res us,..> [profile]`
        let ress: Vec<f64> = args.get(2).map_or("0,100", String::as_str).split(',').filter_map(|v| v.parse().ok()).collect();
        let profiles: &[bool] = if args.get(3).map(String::as_str) == Some("profile") { &[false, true] } else { &[false] };
        if args.get(3).map(String::as_str) == Some("attrib") {
            // row B0: each case profiled with the attribution, twice (the counts must repeat)
            for round in 1..=2 {
                println!();
                println!("=== attribution round {round}");
                for &res in &ress {
                    drum(secs, DrumRun { res: res * 1e-6, attrib: true, ..DrumRun::default() });
                }
            }
            return;
        }
        let digest = args.get(3).map(String::as_str) == Some("digest");
        let snapshot = args.get(3).map(String::as_str) == Some("snapshot");
        // `ELEC_TIERS=0` runs without the three-tier load; `ELEC_TIERS=alt` runs each case without
        // and with it, twice, alternating (the JIT's first run of a side pays its tier-up);
        // `ELEC_BYPASS=alt` alternates Fast's bypass the same way
        let tiers: &[Option<bool>] = match std::env::var("ELEC_TIERS").as_deref() {
            Ok("0") => &[Some(false)],
            Ok("alt") => &[Some(false), Some(true), Some(false), Some(true)],
            _ => &[None],
        };
        let bypass: &[Option<bool>] = if std::env::var("ELEC_BYPASS").as_deref() == Ok("alt") { &[Some(false), Some(true), Some(false), Some(true)] } else { &[None] };
        // `ELEC_ROUNDS=<ms>`: in rounds from then on (the board's own plan)
        let rounds_at = std::env::var("ELEC_ROUNDS").ok().and_then(|v| v.parse::<f64>().ok()).map(|ms| ms * 1e-3);
        // `ELEC_S3=alt`: both off against both on, alternating
        let pairs: Vec<(Option<bool>, Option<bool>)> = if std::env::var("ELEC_S3").as_deref() == Ok("alt") {
            [false, true, false, true].iter().map(|&on| (Some(on), Some(on))).collect()
        } else {
            tiers.iter().flat_map(|&t| bypass.iter().map(move |&b| (t, b))).collect()
        };
        for &res in &ress {
            for &profile in profiles {
                for &(tiers, bypass) in &pairs {
                    for single in single_grades() {
                        // `ELEC_TWICE=1`: each run twice, the second as the host's next of the same board
                        // (Record after the board opened): its planner's states shared (`plan_states`)
                        for _ in 0..if std::env::var_os("ELEC_TWICE").is_some() { 2 } else { 1 } {
                            drum(secs, DrumRun { res: res * 1e-6, profile, digest, tiers, bypass, single, snapshot, rounds: rounds_at, ..DrumRun::default() });
                        }
                    }
                }
            }
        }
        return;
    }
    let b = board();
    devices(&b.circuit);
    if what == "partition" {
        partitions(&b);
        return;
    }
    if what == "held" {
        helds(&b, args.get(1).map(String::as_str) == Some("shares"));
        return;
    }
    if what == "compare" {
        // variants against the default: `compare <window> <mode> <res us,..> <variant,..>` (1: the dense LU forced)
        let list = |k: usize, d: &str| -> Vec<f64> { args.get(k).map_or(d, String::as_str).split(',').filter_map(|v| v.parse().ok()).collect() };
        let ress = list(3, "0,100");
        let variants = list(4, "1");
        for &breathe in modes {
            for &res in &ress {
                let (base, names) = live(&b, breathe, res * 1e-6, true, false, secs, 0, true);
                for &e in &variants {
                    let (cand, _) = live(&b, breathe, res * 1e-6, true, false, secs, e as u32, true);
                    println!("  {}", compare(&base, &cand, &names));
                }
            }
        }
        return;
    }
    for &breathe in modes {
        if matches!(what, "avr" | "all") {
            avr(&b, breathe, 1.0);
        }
        if matches!(what, "live" | "all") {
            // Exact; Fast at the live default with no scope open (max_step, 100 us); at a scope
            // of 100 us and of 25 us a column (res a quarter column); the default without the trace
            for (res, trace) in [(0.0, true), (100e-6, true), (25e-6, true), (6.25e-6, true), (100e-6, false)] {
                live(&b, breathe, res, trace, false, secs, 0, false);
                live(&b, breathe, res, trace, true, secs, 0, false);
            }
        }
    }
}
