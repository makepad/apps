//! Sand's board check: the handheld's Sand with a pour every two seconds and a report every second
//! (`handheld_check.fw.splash`), run on its emulator twin and read back from the real board, so the
//! runtime's paths that only a board can prove (the SPI and I2C waits in WAITI) are checked against
//! the twin report by report. Both run the same image; a report is keyed by its step count, so the
//! two need not start together. The image's panel reports its init (`Panel::report`): the expander's
//! Input, Output and Config before and after the reset pulse, and each stage's time, as `panel` lines.
//!
//! - `twin <secs> <dir>`: builds the check image for the handheld's sand bench as Fab does, writes
//!   its flash regions (`<dir>/head.bin` at 0x0 and `<dir>/slot.bin` at slot A, the regions Fab's
//!   Flash to writes), then runs the twin on the live circuit for `secs` simulated seconds. Each
//!   report prints as one line and goes to `<dir>/twin.txt`, fault frames among them.
//! - `board <dir> <watched>`: the board's frames as `fab-elec-mcu-flash watch` printed them (from a
//!   reset, under a devhub serial claim), into the same lines (`<dir>/board.txt`) and compared with
//!   `<dir>/twin.txt` step by step: the grid's hash, the grain count and the rows presented must
//!   agree. Tilt and touches are the board's own (the twin lies flat and untouched); they are
//!   printed, not compared.
//! - `pictures <dir>`: builds the handheld's pictures firmware (`handheld_pictures.fw.splash`, the
//!   picture receiver `fab-elec-mcu-flash tether` streams to) the same way, into the same two regions.
//! - `rate <dir>` builds `tether_rate.fw.splash` for `fab-elec-mcu-flash tether <port> --rate`
//!   into the same flash regions.
//!
//! `cargo run --release -p fab-elec-fixtures --example board_check -- twin 40 <dir>`

use fab_elec_bench::{lower, Lowered, ModelLibs};
use fab_elec_fw::{compile, maker, Built, Chip, FwInput};
use fab_elec_lang::{evaluate, std_libs};
use fab_elec_mcu_cosim::{watch, FwFrame};
use fab_elec_mcu_emit_s3::{flash_regions, runtime::PANEL_PROBES, wiring};
use fab_elec_sim::livesim::{LiveSim, LiveSpec};
use fab_elec_sim::Sink;
use fab_elec_simhost::events::{EventLog, EventSource};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Discard;
impl Sink for Discard {}

/// One report: probes 1..=8 in the firmware's order.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Report { steps: u16, hash: u16, grains: u16, presents: u16, tilt: [i16; 3], touches: u16 }

impl Report {
    fn line(&self) -> String {
        format!("steps={} hash={} grains={} presents={} tilt={},{},{} touches={}", self.steps, self.hash,
            self.grains, self.presents, self.tilt[0], self.tilt[1], self.tilt[2], self.touches)
    }
}

/// Probe frames into reports; any other frame that matters, as a line of its own.
#[derive(Default)]
struct Reports { current: Option<Report>, lines: Vec<String>, reports: Vec<Report> }

impl Reports {
    fn event(&mut self, event: &watch::Event) {
        match *event {
            watch::Event::Probe { id, value } => {
                if (PANEL_PROBES..PANEL_PROBES + 10).contains(&id) {
                    self.lines.push(format!("panel id={id} value={value:#06x}"));
                    return;
                }
                if id == 1 { self.current = Some(Report { steps: value, ..Report::default() }); }
                let Some(r) = self.current.as_mut() else { return };
                match id {
                    2 => r.hash = value,
                    3 => r.grains = value,
                    4 => r.presents = value,
                    5..=7 => r.tilt[id as usize - 5] = value as i16,
                    8 => {
                        r.touches = value;
                        let r = *r;
                        self.lines.push(r.line());
                        self.reports.push(r);
                        self.current = None;
                    }
                    _ => {}
                }
            }
            watch::Event::Fault { code, site, arg } => self.lines.push(format!("fault code={code} site={site} arg={arg}")),
            watch::Event::Hello { image, .. } => self.lines.push(format!("hello image={image}")),
            _ => {}
        }
    }
}

/// Builds `text` for the handheld's sand bench as Fab does and writes its flash regions into `dir`.
fn build(text: &str, report: bool, dir: &Path) -> (Lowered, FwInput, Built) {
    let sources = fab_elec_fixtures::docs("handheld").expect("handheld is a stock project");
    let evaluated = evaluate(&sources, &std_libs()).unwrap_or_else(|d| panic!("handheld evaluates: {d:?}"));
    let bench = evaluated.benches.iter().find(|b| b.name == "sand").expect("the sand bench");
    let lowered = lower(&evaluated.design, bench.scope_id(), &bench.spec, &ModelLibs::generic());
    let binding = &lowered.firmware[0];
    let mut pins = wiring(&binding.bus.board(0)).expect("handheld bus wiring resolves");
    pins.panel.as_mut().expect("the handheld has a panel").report = report;
    let input = FwInput {
        part: binding.part.clone(), doc: binding.doc.clone(), rev: 1,
        text: text.into(),
        f_clk: binding.f_clk.expect("the main crystal"), params: binding.params.clone(),
        chip: Chip::S3, wiring: Some(pins), island: None, files: Vec::new(), net_files: Vec::new(),
    };
    let built = compile(&input).unwrap_or_else(|d| panic!("the firmware compiles: {d:?}"));
    let Built::S3(image) = &built else { panic!("the firmware builds an S3 image") };
    for note in &image.notes { println!("note: {note}"); }
    std::fs::create_dir_all(dir).expect("the output folder");
    for ((at, bytes), name) in flash_regions(&image.flash).into_iter().zip(["head.bin", "slot.bin", "bundle.bin"]) {
        std::fs::write(dir.join(name), bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        println!("region: {name} at 0x{at:x} ({} bytes)", bytes.len());
    }
    (lowered, input, built)
}

fn twin(secs: f64, dir: &Path) {
    let (lowered, input, built) = build(include_str!("../handheld_check.fw.splash"), true, dir);
    let (mut log, feed) = EventLog::<FwFrame>::new(secs + 1.0, |frame| {
        if frame.is_image() { EventSource::Image } else { EventSource::Trace }
    }, Arc::new(|| {}));
    let readers = Arc::new(Mutex::new(Vec::new()));
    let received = readers.clone();
    let (make, _) = maker(&input, built, feed, false, Arc::new(move |set| {
        *received.lock().expect("screen sink lock") = set;
    }), None);
    let device = make(&lowered, 0).expect("member 0 builds");
    let spec = LiveSpec { rate: f64::INFINITY, max_step: 1e-3, res: 0.0, trace: false, ..LiveSpec::default() };
    let mut sim = LiveSim::new(lowered.circuit.clone(), lowered.plan.clone(), lowered.index.generation, spec)
        .expect("the operating point converges");
    sim.attach(Some(device));
    let mut reports = Reports::default();
    let reader = log.store.register();
    while sim.t() < secs {
        sim.advance((sim.t() + 1e-3).min(secs), Some(Duration::from_millis(50)), &mut Discard)
            .unwrap_or_else(|d| panic!("the live circuit advances: {d}"));
        sim.values(0.0);
        log.drain();
        for e in log.store.read(reader) {
            if let FwFrame::Image(frame) = &e.frame {
                let before = reports.lines.len();
                reports.event(&frame.event);
                for line in &reports.lines[before..] { println!("{:.3} {line}", e.t); }
            }
        }
    }
    std::fs::write(dir.join("twin.txt"), reports.lines.join("\n") + "\n").expect("twin.txt");
    println!("twin: {} reports in {secs} simulated seconds", reports.reports.len());
}

fn board(dir: &Path, watched: &Path) {
    let text = std::fs::read_to_string(watched).expect("the board's watch output");
    let mut reports = Reports::default();
    let mut frames = 0;
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let number = |key: &str| words.iter().find_map(|w| w.strip_prefix(key)).and_then(|v| v.parse::<i64>().ok());
        match words.get(2).copied() {
            Some("probe") => {
                frames += 1;
                let (Some(id), Some(value)) = (number("id="), number("value=")) else { continue };
                reports.event(&watch::Event::Probe { id: id as u8, value: value as u16 });
            }
            Some("hello" | "fault") => { frames += 1; reports.lines.push(words[2..].join(" ")); }
            Some(_) if words[0] == "summary" => reports.lines.push(line.to_owned()),
            _ => {}
        }
    }
    std::fs::write(dir.join("board.txt"), reports.lines.join("\n") + "\n").expect("board.txt");
    for line in &reports.lines { println!("board {line}"); }
    let twin_text = std::fs::read_to_string(dir.join("twin.txt")).expect("twin.txt from `twin`");
    let field = |line: &str, key: &str| line.split(' ').find_map(|w| w.strip_prefix(key)).map(str::to_owned);
    let twin: BTreeMap<String, String> = twin_text.lines().filter_map(|l| Some((field(l, "steps=")?, l.to_owned()))).collect();
    let (mut matched, mut differ, mut unmatched) = (0, 0, 0);
    for r in &reports.reports {
        let Some(t) = twin.get(&r.steps.to_string()) else { unmatched += 1; continue };
        let same = ["hash=", "grains=", "presents="].iter()
            .all(|key| field(t, key) == field(&r.line(), key));
        if same { matched += 1 } else {
            differ += 1;
            println!("differs at steps={}:\n  board {}\n  twin  {t}", r.steps, r.line());
        }
    }
    println!("board: {} reports in {frames} frames, {matched} equal to the twin's at the same step, {differ} differ, \
              {unmatched} past the twin's run", reports.reports.len());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["twin", secs, dir] => twin(secs.parse().expect("seconds"), Path::new(dir)),
        ["board", dir, watched] => board(Path::new(dir), Path::new(watched)),
        ["pictures", dir] => { build(include_str!("../handheld_pictures.fw.splash"), false, Path::new(dir)); }
        ["rate", dir] => { build(include_str!("../tether_rate.fw.splash"), false, Path::new(dir)); }
        _ => {
            eprintln!("board_check twin <secs> <dir> | board <dir> <watched> | pictures <dir> | rate <dir>");
            std::process::exit(2);
        }
    }
}
