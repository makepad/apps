//! The sequencer on the MCU alone, with a clocked chain and diode key matrix.

use fab_elec_mcu_avr::machine::AvrMachine;
use fab_elec_mcu_emit_avr::firmware;
use fab_elec_mcu_emu::{Drive, DriveEdge, ExecCounts, Lockstep, Machine, PcSet, Pull, Ran, SampleAt, Until};
use std::time::Instant;

/// The firmware as the drum16 project holds it.
fn source() -> std::sync::Arc<str> {
    let sources = fab_elec_fixtures::docs("drum16").expect("drum16 is a stock project");
    let id = sources.find("drum16.fw.splash").expect("drum16 has its firmware");
    sources.doc(id).expect("its document").text.clone()
}
const HZ: u64 = 16_000_000;
const US: u64 = HZ / 1_000_000;
const STEP: u64 = 125_000 * US;
/// The TEMPO pot's wiper (ADC7) where the firmware reads 120 BPM: 322 of 1023 against 5 V.
const TEMPO_120: f64 = 1.575;

#[derive(Clone, Copy, Debug)]
struct Frame {
    start: u64,
    tick: u64,
    bits: u32,
}

struct Board {
    cpu: Lockstep<AvrMachine>,
    marks: PcSet,
    levels: Vec<bool>,
    rows: [usize; 5],
    data: usize,
    clock: usize,
    latch: usize,
    /// ADC7 and ADC6, the TEMPO and SHUFFLE pots' wipers, and the voltages the board holds on them.
    tempo: usize,
    tempo_volts: f64,
    shuffle: usize,
    shuffle_volts: f64,
    samples: Vec<SampleAt>,
    shift: u32,
    latched: u32,
    clocks: u32,
    transfer_start: u64,
    keys: u32,
    edges: Vec<DriveEdge>,
    frames: Vec<Frame>,
    trace: Vec<(u64, u32)>,
}

impl Board {
    fn new(words: &[u16], marks: PcSet) -> Self {
        let mut cpu = Lockstep::alternating(AvrMachine::new(words, HZ));
        let pin = |name| cpu.pins().iter().position(|p| *p == name).expect("package pin");
        let rows = ["PC2", "PC3", "PC4", "PC5", "PB4"].map(pin);
        let (data, clock, latch) = (pin("PB3"), pin("PB5"), pin("PB2"));
        let (tempo, shuffle) = (pin("ADC7"), pin("ADC6"));
        let levels = vec![false; cpu.pins().len()];
        for p in 0..cpu.pins().len() {
            cpu.set_input(p, true);
        }
        Self {
            cpu, marks, levels, rows, data, clock, latch, tempo, tempo_volts: TEMPO_120, shuffle, shuffle_volts: 0.0,
            samples: Vec::new(), shift: 0, latched: 0,
            clocks: 0, transfer_start: 0, keys: 0, edges: Vec::new(),
            frames: Vec::new(), trace: Vec::new(),
        }
    }

    fn inputs(&mut self) {
        for (r, &pin) in self.rows.iter().enumerate() {
            let low = (0..4).any(|c| {
                self.keys & (1 << (r * 4 + c)) != 0 && self.latched & (1 << (20 + c)) == 0
            });
            self.cpu.set_input(pin, !low);
        }
    }

    fn hold(&mut self, keys: u32) {
        self.keys = keys;
        self.inputs();
    }

    fn hold_for(&mut self, keys: u32, ticks: u64) {
        self.hold(keys);
        self.run_to(self.cpu.now() + ticks);
    }

    fn idle_frame(&self) -> Frame {
        *self.frames.iter().rev().find(|f| f.bits >> 20 == 0)
            .expect("the chain must latch an image with idle columns")
    }

    fn run_to(&mut self, tick: u64) {
        let mut edges = Vec::new();
        while self.cpu.now() < tick {
            let ran = self.cpu.run(&Until {
                tick, marks: &self.marks, stops: &[], stops_after: 0, on_drive: true,
            });
            assert!(matches!(ran, Ran::Tick | Ran::Drive { .. }),
                "firmware execution stopped: {ran:?}, {:?}", self.cpu.status());
            // the ADC samples a pot's wiper at its sample-and-hold: the board answers with its voltage
            self.cpu.take_samples(&mut self.samples);
            for at in self.samples.drain(..) {
                let pin = usize::from(at.pin);
                assert!(pin == self.tempo || pin == self.shuffle, "the firmware samples only the TEMPO and SHUFFLE pots");
                self.cpu.set_sample(at, if pin == self.tempo { self.tempo_volts } else { self.shuffle_volts }, 5.0);
            }
            self.cpu.take_drives(&mut edges);
            for edge in edges.drain(..) {
                let p = edge.pin as usize;
                let high = edge.drive == Drive::High;
                let rising = high && !self.levels[p];
                self.levels[p] = high;
                if p == self.clock && rising {
                    if self.clocks == 0 { self.transfer_start = edge.tick; }
                    self.shift = ((self.shift << 1) | u32::from(self.levels[self.data])) & 0xffffff;
                    self.clocks += 1;
                }
                if p == self.latch && rising {
                    assert_eq!(self.clocks, 24, "each latch must follow exactly 24 rising clocks");
                    self.clocks = 0;
                    self.latched = self.shift;
                    self.frames.push(Frame { start: self.transfer_start, tick: edge.tick, bits: self.latched });
                    self.inputs();
                }
                self.edges.push(edge);
            }
            self.cpu.take_trace(&mut self.trace);
        }
    }

    fn pulses(&self, name: &str) -> Vec<(u64, u64)> {
        let pin = self.cpu.pins().iter().position(|p| *p == name).expect("pulse pin");
        let mut up = None;
        let mut pulses = Vec::new();
        for e in self.edges.iter().filter(|e| e.pin as usize == pin) {
            if e.drive == Drive::High {
                assert!(up.replace(e.tick).is_none(), "{name}: duplicate rising edge");
            } else if e.drive == Drive::Low {
                if let Some(start) = up.take() { pulses.push((start, e.tick)); }
            }
        }
        assert!(up.is_none(), "{name}: pulse remained high at the end");
        pulses
    }
}

#[test]
fn sequencer_timing_and_matrix() {
    let wall = Instant::now();
    let fw = firmware(&source(), HZ as f64, &[("bpm", 120.0), ("autoplay", 1.0)])
        .unwrap_or_else(|diags| panic!("firmware must compile without diagnostics: {diags:#?}"));
    // the notes state the ADC's timing for TEMPO's read and the EEPROM's charged write time
    assert!(fw.notes.iter().all(|n| n.starts_with("analog inputs: ") || n.starts_with("EEPROM: ")),
        "firmware must have no other notes: {:?}", fw.notes);
    let map = fw.debug.map.as_ref().expect("firmware must supply its handler map");
    let mut marks = PcSet::new(0, fw.words.len() as u32);
    for pc in map.entries.iter().map(|e| e.0).chain(map.returns.iter().copied()) { marks.insert(pc); }
    let mut board = Board::new(&fw.words, marks);
    board.run_to(HZ / 5);
    let first = board.pulses("PC1").first().expect("demo starts with accent").0;
    // Two walks fit before the low tom step. The held interval crosses that
    // step's deadline, testing the poll's room guard as well as debouncing.
    let press = first + 10 * STEP - 80_000 * US;
    let release = press + 150_000 * US;
    board.run_to(press);
    board.hold(1 << 2);
    board.run_to(release);
    board.hold(0);
    // Include step 3 of the following bar, then its falling edges and LED latch.
    board.run_to(first + 18 * STEP + 20_000 * US);
    for &pin in &board.rows {
        assert_eq!(board.cpu.drive(pin), Drive::Input(Pull::Up), "matrix rows must retain their internal pull-ups");
    }

    let names = ["PD4", "PD5", "PD6", "PD7", "PB0", "PB1", "PC0", "PC1"];
    let pulses: Vec<_> = names.iter().map(|name| board.pulses(name)).collect();
    let mut max_error = 0;
    let mut min_width = u64::MAX;
    let mut max_width = 0;
    let mut spread = 0;
    let mut held_delay = 0;
    for pin in &pulses {
        for &(up, down) in pin {
            let n = (up - first + STEP / 2) / STEP;
            max_error = max_error.max(up.abs_diff(first + n * STEP));
            min_width = min_width.min(down - up);
            max_width = max_width.max(down - up);
        }
    }
    for n in 0..19 {
        let grid = first + n * STEP;
        let edges: Vec<_> = pulses.iter().flat_map(|p| p.iter().map(|p| p.0))
            .filter(|&at| at >= grid.saturating_sub(STEP / 2) && at < grid + STEP / 2).collect();
        // A step that strikes nothing has no edge to measure.
        let (Some(&lo), Some(&hi)) = (edges.iter().min(), edges.iter().max()) else { continue };
        spread = spread.max(hi - lo);
        if (press..release).contains(&grid) { held_delay = held_delay.max(lo.saturating_sub(grid)); }
    }

    let mut walks = Vec::new();
    let mut i = 0;
    while i < board.frames.len() {
        let f = board.frames[i];
        if f.bits >> 20 != 0 {
            assert!(i + 4 < board.frames.len(), "column walk must restore idle columns");
            for (j, cols) in [14, 13, 11, 7, 0].into_iter().enumerate() {
                let next = board.frames[i + j];
                assert_eq!(next.bits >> 20, cols, "column walk must select each column then restore idle");
                assert_eq!(next.bits & 0xfffff, f.bits & 0xfffff, "column walking must preserve every LED");
            }
            walks.push((f.start, board.frames[i + 4].tick));
            i += 5;
        } else { i += 1; }
    }
    assert!(walks.len() >= 2, "held key must cause at least two complete column walks");
    assert!(walks[1].0 - walks[0].0 >= 10_000 * US, "debounce walks must be at least 10 ms apart: {:?} us",
        walks.iter().map(|w| ((w.0 - press) / US, (w.1 - w.0) / US)).collect::<Vec<_>>());
    assert!(walks.iter().all(|w| w.0 >= press && w.0 < release), "idle polls must not clock the chain");
    let edit_ready = walks[1].1;
    let led_at = board.frames.iter().find(|f| f.tick > press && f.bits >> 20 == 0 && f.bits & 4 != 0)
        .expect("step key 3 must become visible in the latched LEDs").tick;
    let longest_walk = walks.iter().map(|w| w.1 - w.0).max().unwrap();
    println!("sequencer: max grid offset {:.3} us; widths {:.3}..{:.3} us; max step spread {:.3} us; held-key first-edge delay {:.3} us; key-to-LED {:.3} ms; longest walk {:.3} ms; image {} bytes; wall {:.3} s",
        max_error as f64 / US as f64, min_width as f64 / US as f64, max_width as f64 / US as f64,
        spread as f64 / US as f64, held_delay as f64 / US as f64,
        (led_at - press) as f64 / (1000 * US) as f64, longest_walk as f64 / (1000 * US) as f64,
        fw.words.len() * 2, wall.elapsed().as_secs_f64());

    assert!(max_error <= 1000 * US, "trigger grid error exceeds 1 ms: {max_error} cycles");
    assert!(min_width >= 950 * US && max_width <= 1050 * US,
        "trigger widths must be 1 ms within 50 us: {min_width}..{max_width} cycles");
    assert!(spread <= 1000 * US, "voices of a step must start within 1 ms: {spread} cycles");
    let patterns: [&[u64]; 8] = [
        &[0, 4, 8, 12], &[4, 12], &[10], &[11], &[0, 2, 4, 6, 8, 10, 12], &[14],
        &[0], &[0, 8],
    ];
    for (p, name) in names.iter().enumerate() {
        let actual: Vec<_> = pulses[p].iter().map(|e| (e.0 - first + STEP / 2) / STEP).collect();
        let expected: Vec<_> = (0..19).filter(|n| patterns[p].contains(&(n % 16)) || (p == 0 && *n == 18)).collect();
        assert_eq!(actual, expected, "{name}: demo steps or the edited next-bar kick differ");
    }
    for &(up, _) in &pulses[7] {
        let kick = pulses[0].iter().find(|p| p.0 >= up && p.0 - up <= 1000 * US)
            .expect("accent must rise before the accented kick");
        assert!(kick.0 > up, "accent must be written before the kick");
    }
    assert!(led_at >= edit_ready, "key must not edit the LEDs before its second walk");
    for f in board.frames.iter().filter(|f| f.start > first) {
        if !walks.iter().any(|w| f.start >= w.0 && f.tick <= w.1) {
            let n = (f.start - first) / STEP;
            assert!(f.tick < first + n * STEP + 20_000 * US,
                "idle polling must not transfer LED images between step tails");
        }
    }
    for n in 0..19 {
        let grid = first + n * STEP;
        // a step that strikes nothing still runs its tail: its image follows the grid
        let falls = pulses.iter().flat_map(|p| p.iter()).filter(|p| p.0.abs_diff(grid) < STEP / 2)
            .map(|p| p.1).max().unwrap_or(grid);
        let f = board.frames.iter().find(|f| f.tick > falls && f.bits >> 20 == 0)
            .expect("every step must latch its LEDs after the pulses");
        assert!(f.tick < grid + 20_000 * US, "step {n}: LED update must follow in the short gap");
        let kick = if grid > edit_ready { 0x1115 } else { 0x1111 };
        let expected = (kick ^ (1 << (n % 16))) | (1 << 16);
        assert_eq!(f.bits, expected, "step {n}: kick LEDs, running inversion, PLAY, or idle columns differ");
        assert!(!walks.iter().any(|w| w.0 < falls && w.1 > grid), "step {n}: a column walk overlaps its pulses");
    }
    assert!(!board.trace.is_empty(), "machine must record handler execution");

    // Continue on the same board. Hold a modifier through its two debounce walks before
    // adding another key; a full step leaves room for both walks even near a step deadline.
    // Releases allow a walk already in progress to finish, then two idle reads and a repaint.
    const PLAY: u32 = 1 << 16;
    const SHIFT: u32 = 1 << 17;
    const INST: u32 = 1 << 18;
    let check_leds = |board: &Board, track: u32, held: u32| {
        let f = board.idle_frame();
        let n = (f.start - first) / STEP;
        assert_eq!(f.bits, (track ^ (1 << (n % 16))) | PLAY | held,
            "selected track, running inversion, held modifiers, or reserved LED differ");
    };
    board.hold_for(INST, STEP);
    check_leds(&board, 0x1115, INST);
    board.hold_for(INST | (1 << 1), STEP);
    check_leds(&board, 0x1010, INST);
    board.hold_for(0, 50_000 * US);
    check_leds(&board, 0x1010, 0);

    // Step 9 is still ahead in this bar. A quick 50 ms tap edits it (two reads 10 ms apart agree
    // well inside it), and editing the selected snare must leave the kick alone.
    board.hold_for(1 << 8, 50_000 * US);
    board.hold(0);
    board.run_to(first + 24 * STEP + 20_000 * US);
    check_leds(&board, 0x1110, 0);
    let snare_steps: Vec<_> = board.pulses("PD5").iter()
        .map(|p| (p.0 - first + STEP / 2) / STEP).filter(|&n| n >= 19).collect();
    assert_eq!(snare_steps, [20, 24], "the snare must fire on its edited step 9");
    let kick_steps: Vec<_> = board.pulses("PD4").iter()
        .map(|p| (p.0 - first + STEP / 2) / STEP).filter(|&n| n >= 19).collect();
    assert_eq!(kick_steps, [20, 24], "editing the snare must preserve the kick pattern");

    board.hold_for(SHIFT, STEP);
    check_leds(&board, 0x1110, SHIFT);
    board.hold_for(SHIFT | PLAY, STEP);
    check_leds(&board, 0, SHIFT);
    board.hold_for(0, 50_000 * US);
    check_leds(&board, 0, 0);
    board.run_to(first + 28 * STEP + 20_000 * US);
    check_leds(&board, 0, 0);
    assert!(!board.pulses("PD5").iter().any(|p| p.0 >= first + 25 * STEP),
        "clearing the selected snare must remove its later programmed hit");
    assert!(board.pulses("PD4").iter().any(|p| p.0.abs_diff(first + 28 * STEP) < 1000 * US),
        "clearing the snare must leave the kick playing");

    board.hold_for(PLAY, STEP);
    assert_eq!(board.idle_frame().bits, 0, "PLAY stops, removes the running inversion, and goes dark");
    board.hold_for(0, 50_000 * US);
    for name in names { board.pulses(name); }
    let quiet_from = board.edges.len();
    board.run_to(board.cpu.now() + STEP + 20_000 * US);
    for edge in &board.edges[quiet_from..] {
        assert!(!names.contains(&board.cpu.pins()[edge.pin as usize]),
            "PLAY stopped: no trigger edge may occur for a full step");
    }

    let restart_press = board.cpu.now();
    board.hold_for(PLAY, STEP);
    board.hold_for(0, 50_000 * US);
    board.run_to(restart_press + STEP + 100_000 * US);
    let restarted: Vec<_> = board.pulses("PC1").into_iter().filter(|p| p.0 >= restart_press).collect();
    assert_eq!(restarted.len(), 1, "PLAY must restart the step clock");
    let accent = restarted[0].0;
    for name in ["PD4", "PB0", "PC0"] {
        assert!(board.pulses(name).iter().any(|p| p.0 > accent && p.0 - accent < 1000 * US),
            "{name}: restarting must play step 1, with accent before the voices");
    }
    for name in ["PD5", "PD6", "PD7", "PB1"] {
        assert!(!board.pulses(name).iter().any(|p| p.0 >= restart_press),
            "{name}: restarting must preserve the cleared track and the other patterns");
    }
    assert_eq!(board.idle_frame().bits, PLAY | 1,
        "restart must show step 1 inverted on the still-selected, cleared snare");

    board.hold_for(SHIFT, STEP);
    assert_eq!(board.idle_frame().bits & 0xf0000, PLAY | SHIFT, "held SHIFT must light");
    board.hold_for(SHIFT | (1 << 15), STEP);
    board.hold_for(0, 50_000 * US);
    // TEMPO turned to 3.14 V (643 of 1023): 180 bpm from the step after the next, the pending
    // tick keeping its deadline. In this part of the restarted bar, the closed hat strikes every
    // other step (through step 13). Four rises give three two-step intervals; the pin's fixed
    // slot cancels out.
    let turned = board.cpu.now();
    board.tempo_volts = 3.14;
    let faster = (15_000_000 / 180) * US;
    let settled = turned + 2 * STEP;
    board.run_to(settled + 8 * faster + 20_000 * US);
    let hats: Vec<_> = board.pulses("PB0").into_iter().filter(|p| p.0 >= settled).collect();
    assert!(hats.len() >= 4, "turning TEMPO must leave four closed-hat hits in the observation window");
    let recent = &hats[hats.len() - 4..];
    for pair in recent.windows(2) {
        let gap = pair[1].0 - pair[0].0;
        assert!(gap.abs_diff(2 * faster) <= 250 * US,
            "TEMPO at 3.14 V must play 180 bpm: {gap} cycles between hats, expected {}", 2 * faster);
    }
    let f = board.idle_frame();
    assert_eq!(f.bits & 0xf0000, PLAY, "released modifiers and the reserved LED must be dark");
    assert_eq!((f.bits & 0xffff).count_ones(), 1,
        "SHIFT with a step key must leave the cleared snare empty, with only its running step lit");
    for name in names { board.pulses(name); }

    // SHUFFLE turned to 2.5 V (511 of 1023): 479 counts past its straight bottom, so the step after
    // each odd-numbered one comes 479/2048 of a step late and the next one as much early. The high
    // tom on step 12 sits between closed hats on steps 11 and 13. The swing holds a few steps after
    // the turn: the ADC's read, the next read, then the pending tick.
    let turned = board.cpu.now();
    board.shuffle_volts = 2.5;
    board.run_to(turned + 5 * faster + 16 * faster + 20_000 * US);
    let late = (faster / US) as i64 * 479 / 2048 * US as i64;
    let tom = board.pulses("PD7").into_iter().find(|p| p.0 >= turned + 5 * faster).expect("the high tom strikes in the swung bar").0;
    let hats = board.pulses("PB0");
    let before = hats.iter().rev().find(|p| p.0 < tom).expect("a closed hat on step 11").0;
    let after = hats.iter().find(|p| p.0 > tom).expect("a closed hat on step 13").0;
    let (long, short) = ((tom - before) as i64, (after - tom) as i64);
    assert!((long - (faster as i64 + late)).abs() <= 250 * US as i64 && (short - (faster as i64 - late)).abs() <= 250 * US as i64,
        "SHUFFLE at 2.5 V swings step 12 late by {} us: {} us after step 11, {} us before step 13",
        late / US as i64, long / US as i64, short / US as i64);
    assert!(((after - before) as i64 - 2 * faster as i64).abs() <= 250 * US as i64, "a swung pair keeps the tempo");
    for name in names { board.pulses(name); }

    // Power cycle: a new chip given this one's EEPROM plays and shows the edited pattern (the kick
    // with step 3, the snare cleared), not the built-in one, and reset selects the kick again.
    let mut again = Board::new(&fw.words, board.marks.clone());
    again.cpu.both(|m| m.set_eeprom(board.cpu.eeprom()));
    again.run_to(HZ / 5);
    let first = again.pulses("PC1").first().expect("the restored pattern starts with accent").0;
    again.run_to(first + 16 * STEP + 20_000 * US);
    let steps = |name: &str| -> Vec<u64> { again.pulses(name).iter().map(|p| (p.0 - first + STEP / 2) / STEP).collect() };
    assert_eq!(steps("PD4"), [0, 2, 4, 8, 12, 16], "the kick's step 3 must survive the power cycle");
    assert!(steps("PD5").is_empty(), "the cleared snare must stay cleared after the power cycle");
    assert_eq!(steps("PB0"), [0, 2, 4, 6, 8, 10, 12, 16], "the untouched closed hat must keep its pattern");
    let f = again.idle_frame();
    let n = (f.start - first) / STEP;
    assert_eq!(f.bits, (0x1115 ^ (1 << (n % 16))) | PLAY, "after the power cycle the LEDs must show the restored kick");
    let mut counts = ExecCounts::default();
    again.cpu.take_counts(&mut counts);
    assert!(!counts.blocks.is_empty() && !counts.conds.is_empty(), "the restored sequencer reports block entries and conditional outcomes");

    // every poll run, walking, painting or saving a byte, ends inside the 2 ms it leaves before a step
    let poll = fw.debug.handlers.iter().position(|h| h.name == "poll")
        .unwrap_or_else(|| panic!("poll must be a handler: {:?}", fw.debug.handlers)) as u32;
    let poll_pc = map.entries.iter().find(|e| e.1 == poll).expect("poll must have an entry").0;
    let longest_poll = |trace: &[(u64, u32)]| {
        let (mut start, mut longest, mut runs) = (None, 0, 0);
        for &(tick, pc) in trace {
            if pc == poll_pc {
                start = Some(tick);
            } else if map.returns.binary_search(&pc).is_ok() {
                if let Some(at) = start.take() { longest = u64::max(longest, tick - at); runs += 1; }
            }
        }
        assert!(runs > 100, "the trace must hold the polls' runs: {runs}");
        longest
    };
    let longest = longest_poll(&board.trace).max(longest_poll(&again.trace));
    assert!(longest < 2000 * US, "a poll run must end inside the 2 ms it requires before a step: {} us", longest / US);
    println!("sequencer: controls and the power cycle checked; longest poll {:.3} ms; total wall {:.3} s",
        longest as f64 / (1000 * US) as f64, wall.elapsed().as_secs_f64());
}
