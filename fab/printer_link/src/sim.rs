//! A simulated printer for tests and demos: `FAB_PRINTER_SIM=<file>` makes Fab link to a file
//! instead of a printer. Everything above the connection is the real link, with its checks, job
//! tracker and answers. Only the printer's end of the connection and its storage are fakes, in
//! memory, so nothing leaves the machine and what a card says is what the link would say.
//!
//! `FAB_PRINTER_SIM_MODEL` selects the family and the reported model. For the first family,
//! each line of the file is one report as the printer publishes it
//! (`{"print":{"command":"push_status","msg":0,...}}`, deltas with `"msg":1`). The file is followed:
//! lines appended later arrive as new reports (a test or script appends them to step the job), and
//! a file that shrinks is read from its start again. Blank lines and lines starting with `#` are
//! skipped. The job's upload takes [`SIM_UPLOAD`], the start command is written as it went out next
//! to the report file ([`started_path`]), and what the printer then does comes from the report file
//! like the rest. A storage listing is refused.
//!
//! For the second family (Moonraker), each JSON object is a status delta, merged into the
//! in-memory printer. `@refuse <reason>` refuses the next start;
//! `@head <h> <present 0|1> <type> <RRGGBBAA>` sets a zero-based head. Other directives and
//! invalid JSON are skipped, logging only their line number. Accepted starts are appended,
//! one command per line, to [`started_path`]. No clock advances the job: reports drive it.
//!
//! For the first family, lines starting with `@` say what the printer does from there on:
//!
//! - `@answer success`, `@answer silence`, `@answer echo` (repeats the command and says nothing
//!   about it), `@answer refuse <reason>`: how it answers commands. It starts with success.
//! - `@drop`: the connection breaks. The link connects again by itself.
//! - `@down`, `@up`: the connection breaks and none is taken until `@up`.
//! - `@writes-fail`: this connection's writes fail while its reads go on.
//! - `@read-wait <ms>`: how long a read waits when there is nothing to read. It starts at the real
//!   connection's read timeout, and a longer one lets what is pressed gather into one batch.
//! - `@capture-upload`: keep a successful upload beside the report file for byte comparisons in tests.
//! - `@upload <ms>`: how long an upload takes, and how long a cancelled one takes to close.

use crate::link::{spawn_transfer, Storage, Transfer};
use crate::model::Reports;
use crate::mqtt::fake::FakePrinter;
use makepad_strict_json::{self as json, Value};
use std::cell::RefCell;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

/// The report file `FAB_PRINTER_SIM` names, if it is set.
pub fn sim_path() -> Option<PathBuf> {
    std::env::var_os("FAB_PRINTER_SIM").filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// What a simulated link answers to a storage listing.
pub const SIM_REFUSAL: &str = "simulated printer: nothing is sent";

/// How long a simulated upload takes.
pub const SIM_UPLOAD: Duration = Duration::from_secs(3);

/// Where the simulated printer writes the start command it was given.
pub fn started_path(reports: &std::path::Path) -> PathBuf {
    reports.with_extension("started")
}

/// The real connection's read timeout: how long a read with nothing to read waits.
const READ_WAIT: Duration = Duration::from_millis(250);

/// How often the printer reports with nothing new, as a real one keeps doing.
const REPORT_EVERY: Duration = Duration::from_secs(1);

/// Follows one report file from where it was read last.
pub struct SimFeed {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl SimFeed {
    pub fn new(path: PathBuf) -> Self { Self { path, offset: 0, partial: Vec::new() } }

    /// Every complete line written since the last call, without the blank ones and comments.
    /// Err: the file cannot be read (it may appear later).
    pub fn poll(&mut self) -> Result<Vec<String>, String> {
        let mut file = std::fs::File::open(&self.path).map_err(|_| "simulated report file not found".to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        if len < self.offset {
            *self = Self::new(std::mem::take(&mut self.path));
        }
        if len == self.offset { return Ok(Vec::new()); }
        file.seek(SeekFrom::Start(self.offset)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        self.offset += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else { return Ok(Vec::new()) };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        Ok(complete.split(|b| *b == b'\n').map(|line| String::from_utf8_lossy(line.trim_ascii()).into_owned()).filter(|line| !line.is_empty() && !line.starts_with('#')).collect())
    }
}

/// How the printer answers commands.
enum Answer { Success, Silence, Echo, Refuse(String) }

/// The printer itself: what lasts from one connection to the next.
pub(crate) struct SimPrinter {
    path: PathBuf,
    feed: SimFeed,
    reports: Reports,
    answer: Answer,
    down: bool,
    read_wait: Duration,
    upload: Duration,
    capture_upload: bool,
}

impl SimPrinter {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { feed: SimFeed::new(path.clone()), path, reports: Reports::default(), answer: Answer::Success, down: false, read_wait: READ_WAIT, upload: SIM_UPLOAD, capture_upload: false }
    }

    /// What the file says next. A subscribed connection hears the reports; without one they only
    /// add to the next full report, as a printer goes on while nobody listens.
    fn follow(&mut self, connection: Option<&FakePrinter>) {
        let Ok(lines) = self.feed.poll() else { return };
        for line in lines {
            if let Some(order) = line.strip_prefix('@') {
                self.obey(order, connection);
                continue;
            }
            match self.reports.apply(line.as_bytes()) {
                Ok(_) => if let Some(connection) = connection { connection.report(&line); },
                Err(e) => eprintln!("fab: simulated report: {e}: {line}"),
            }
        }
    }

    fn obey(&mut self, order: &str, connection: Option<&FakePrinter>) {
        let (word, rest) = order.split_once(' ').unwrap_or((order, ""));
        let millis = rest.trim().parse().ok().map(Duration::from_millis);
        match (word, rest.split_once(' ').unwrap_or((rest, ""))) {
            ("answer", ("success", _)) => self.answer = Answer::Success,
            ("answer", ("silence", _)) => self.answer = Answer::Silence,
            ("answer", ("echo", _)) => self.answer = Answer::Echo,
            ("answer", ("refuse", why)) => self.answer = Answer::Refuse(why.to_string()),
            ("drop", _) => if let Some(connection) = connection { connection.drop_link(); },
            ("down", _) => {
                self.down = true;
                if let Some(connection) = connection { connection.drop_link(); }
            }
            ("up", _) => self.down = false,
            ("writes-fail", _) => if let Some(connection) = connection { connection.break_writes(); },
            ("read-wait", _) if millis.is_some() => self.read_wait = millis.unwrap_or(READ_WAIT),
            ("capture-upload", _) => self.capture_upload = true,
            ("upload", _) if millis.is_some() => self.upload = millis.unwrap_or(SIM_UPLOAD),
            _ => eprintln!("fab: simulated printer: no such order: @{order}"),
        }
    }
}

/// One connection to the printer: the link reads and writes MQTT here as it does to a real one.
pub(crate) struct SimStream {
    printer: Rc<RefCell<SimPrinter>>,
    wire: FakePrinter,
    /// How many of the wire's requests the printer has heard.
    heard: usize,
    /// The link has asked for everything: it is subscribed, and reports reach it from here on.
    asked: bool,
    reported: Instant,
}

impl SimStream {
    pub(crate) fn connect(printer: &Rc<RefCell<SimPrinter>>) -> Result<Self, &'static str> {
        let mut p = printer.borrow_mut();
        p.follow(None);
        if !p.path.exists() { return Err("simulated printer: no reports yet"); }
        if p.down { return Err("simulated printer: it refuses the connection"); }
        Ok(Self { printer: printer.clone(), wire: FakePrinter::default(), heard: 0, asked: false, reported: Instant::now() })
    }

    fn follow(&mut self) {
        self.printer.borrow_mut().follow(Some(&self.wire).filter(|_| self.asked));
    }

    /// What the printer does with one request.
    fn hear(&mut self, request: &str) {
        let Ok(parsed) = json::parse(request.as_bytes()) else { return };
        let Value::Obj(modules) = &parsed else { return };
        let Some((module, fields)) = modules.first() else { return };
        let command = fields.get("command").and_then(Value::as_str).unwrap_or_default();
        let id = fields.get("sequence_id").and_then(Value::as_str).unwrap_or_default();
        let printer = self.printer.borrow();
        match command {
            "pushall" => {
                self.asked = true;
                if let Some(full) = printer.reports.full_report() { self.wire.report(&full); }
            }
            "get_version" => self.wire.report(&format!(r#"{{"{module}":{{"command":"get_version","sequence_id":"{id}","module":[]}}}}"#)),
            _ => {
                if command == "project_file" { let _ = std::fs::write(started_path(&printer.path), request); }
                let said = match &printer.answer {
                    Answer::Silence => return,
                    Answer::Success => r#","result":"success","reason":"SUCCESS""#.to_string(),
                    Answer::Echo => String::new(),
                    Answer::Refuse(why) => format!(r#","result":"failed","reason":{}"#, Value::Str(why.clone()).to_json()),
                };
                self.wire.report(&format!(r#"{{"{module}":{{"command":"{command}","sequence_id":"{id}"{said}}}}}"#));
            }
        }
    }
}

impl Read for SimStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self.wire.read(out) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            done => return done,
        }
        let wait = self.printer.borrow().read_wait;
        std::thread::sleep(wait);
        self.follow();
        if self.asked && self.reported.elapsed() >= REPORT_EVERY {
            self.reported = Instant::now();
            if let Some(full) = self.printer.borrow().reports.full_report() { self.wire.report(&full); }
        }
        self.wire.read(out)
    }
}

impl Write for SimStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.wire.write(bytes) }

    fn flush(&mut self) -> io::Result<()> {
        // What the file says up to now comes first: it was said before this reached the printer.
        self.follow();
        self.wire.flush()?;
        let requests = self.wire.published();
        for request in &requests[self.heard..] { self.hear(request); }
        self.heard = requests.len();
        Ok(())
    }
}

/// The printer's storage: an upload takes its time, and nothing is kept.
pub(crate) struct SimStorage(pub(crate) Rc<RefCell<SimPrinter>>);

impl Storage for SimStorage {
    fn upload(&self, _file: String, job: crate::safety::ValidatedJob) -> Option<Arc<Transfer>> {
        let printer = self.0.borrow();
        let takes = printer.upload;
        let capture = printer.capture_upload.then(|| printer.path.with_extension("uploaded"));
        spawn_transfer("simulated", move |_, transfer| {
            let started = Instant::now();
            let total = job.bytes().len() as f64;
            let result = loop {
                if transfer.cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(takes);
                    break Err("upload cancelled".to_string());
                }
                let done = started.elapsed().as_secs_f64() / takes.as_secs_f64().max(f64::MIN_POSITIVE);
                if done >= 1.0 {
                    break capture.as_ref().map_or(Ok(()), |path| std::fs::write(path, job.bytes()).map_err(|e| e.to_string()));
                }
                transfer.sent.store((total * done) as u64, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(20));
            };
            *transfer.upload.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        })
    }

    fn list(&self) -> Option<Arc<Transfer>> {
        spawn_transfer("simulated", |_, transfer| {
            *transfer.listing.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(SIM_REFUSAL.into()));
        })
    }
}

#[cfg(test)]
mod through_the_real_link {
    use super::*;
    use crate::{Command, CommandState, ConnectionStatus, JobStage, PrintJob, PrinterLink, Snapshot};
    use crate::model::TrayId;
    use std::io::Write;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn unclaimed_simulator_is_offline() {
        let mut link = PrinterLink::simulate_model(PathBuf::new(), "Unclaimed".into());
        let snapshot = link.poll().expect("offline snapshot");
        assert_eq!(snapshot.status, ConnectionStatus::Offline(makepad_fab_settings::family::UNSUPPORTED));
    }

    /// A printer that passes the job checks, in the given state.
    fn full(state: &str) -> String {
        format!(r#"{{"print":{{"command":"push_status","msg":0,"gcode_state":"{state}","subtask_name":"last","sdcard":true,"device":{{"nozzle":{{"info":[{{"id":0,"diameter":0.4}},{{"id":1,"diameter":0.4}}]}}}},"ams":{{"tray_exist_bits":"1","ams":[{{"id":"0","info":"1003","tray":[{{"id":"0","tray_type":"PLA","tray_color":"FF0000FF"}},{{"id":"1"}}]}}]}}}}}}"#)
    }

    fn cube(plate_clear: bool) -> Command {
        let mut print = PrintJob { job: crate::safety::tests::checked_square("cube"), trays: vec![Some(TrayId { ams: 0, slot: 0 })], used: vec![0], options: Default::default() };
        print.options.plate_clear = plate_clear;
        Command::Print(print)
    }

    struct Sim { dir: PathBuf, path: PathBuf, link: PrinterLink, last: Arc<Snapshot> }

    impl Sim {
        fn start(tag: &str, lines: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!("fab-sim-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("reports.jsonl");
            std::fs::write(&path, format!("@read-wait 5\n{}\n", lines.join("\n"))).unwrap();
            let model = crate::safety::tests::profile().printer_model;
            let link = PrinterLink::simulate_model(path.clone(), model);
            Self { dir, path, link, last: Default::default() }
        }
        fn say(&self, line: &str) {
            writeln!(std::fs::OpenOptions::new().append(true).open(&self.path).unwrap(), "{line}").unwrap();
        }
        fn until(&mut self, what: &str, done: impl Fn(&Snapshot) -> bool) {
            let started = Instant::now();
            loop {
                if let Some(snapshot) = self.link.poll() { self.last = snapshot; }
                if done(&self.last) { return; }
                assert!(started.elapsed() < Duration::from_secs(20), "never {what}: {:?} {:?} {:?}", self.last.status, self.last.command, self.last.job);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn shows(&mut self, command: &'static str, state: CommandState) {
            self.until(&format!("{command}: {state:?}"), |s| s.command == Some((command, state.clone())));
        }
        fn says(&mut self, command: &'static str, words: &str) {
            self.until(&format!("{command}: {words}"), |s| matches!(&s.command, Some((c, CommandState::Failed(why))) if *c == command && why.contains(words)));
        }
        fn stage(&mut self, what: &str, done: impl Fn(&JobStage) -> bool) {
            self.until(what, |s| s.job.as_ref().is_some_and(|j| done(&j.stage)));
        }
        fn connected(&mut self, state: &str) {
            self.until(&format!("connected and {state}"), |s| s.status == ConnectionStatus::Connected && s.state.gcode_state.as_deref() == Some(state));
        }
    }

    impl Drop for Sim {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.dir); }
    }

    #[test]
    fn its_controls_get_the_real_links_answers() {
        if !crate::printing_supported() { return; }
        let mut sim = Sim::start("answers", &[&full("RUNNING")]);
        sim.connected("RUNNING");
        sim.link.send(Command::Pause);
        sim.shows("pause", CommandState::Done);

        sim.say("@answer refuse busy");
        sim.link.send(Command::Stop);
        sim.shows("stop", CommandState::Failed("busy".into()));

        // It stays silent: the stop waits, and what is pressed over it says it was not sent.
        sim.say("@answer silence");
        sim.link.send(Command::Stop);
        sim.shows("stop", CommandState::Pending);
        sim.link.send(Command::Pause);
        sim.says("pause", "not sent: a stop is waiting");

        // The connection breaks. What the printer did meanwhile comes with the next full report.
        sim.say("@drop");
        sim.says("stop", "connection dropped before the printer answered");
        sim.say(r#"{"print":{"msg":1,"gcode_state":"PAUSE"}}"#);
        sim.connected("PAUSE");

        // Writes fail while reads go on.
        sim.say("@answer success");
        sim.say("@writes-fail");
        sim.link.send(Command::Stop);
        sim.says("stop", "did not reach the printer");
    }

    #[test]
    fn a_job_goes_through_the_real_upload_and_start() {
        if !crate::printing_supported() { return; }
        let mut sim = Sim::start("start", &[&full("IDLE"), "@upload 50"]);
        sim.connected("IDLE");
        sim.link.send(cube(false));
        sim.stage("uploading", |s| matches!(s, JobStage::Uploading { .. }));
        sim.stage("accepted", |s| *s == JobStage::Accepted);
        let started = std::fs::read_to_string(started_path(&sim.path)).unwrap();
        assert!(started.contains("\"command\":\"project_file\"") && started.contains("\"sequence_id\""), "the command as it went out: {started}");
    }

    #[test]
    fn a_cancelled_upload_takes_its_time_to_close() {
        if !crate::printing_supported() { return; }
        let mut sim = Sim::start("closing", &[&full("IDLE"), "@upload 1000"]);
        sim.connected("IDLE");
        sim.link.send(cube(false));
        sim.stage("uploading", |s| matches!(s, JobStage::Uploading { .. }));
        sim.link.send(Command::Stop);
        sim.stage("cancelled", |s| *s == JobStage::Cancelled);
        sim.link.send(cube(false));
        sim.says("print", "still closing");
    }

    #[test]
    fn a_plate_confirmation_does_not_outlast_an_outage() {
        if !crate::printing_supported() { return; }
        let mut sim = Sim::start("plate", &[&full("FINISH"), "@upload 200"]);
        sim.connected("FINISH");
        sim.link.send(cube(true));
        sim.stage("uploading", |s| matches!(s, JobStage::Uploading { .. }));
        sim.say("@drop");
        sim.stage("refused", |s| matches!(s, JobStage::Failed(why) if why.contains("uploaded, not started") && why.contains("confirm the plate is clear")));
        assert!(!started_path(&sim.path).exists(), "nothing started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PrinterState;

    /// The state after what the file says next, merged as the link merges it.
    fn next(feed: &mut SimFeed, reports: &mut Reports) -> Option<PrinterState> {
        feed.poll().unwrap().iter().filter_map(|line| reports.apply(line.as_bytes()).unwrap()).last()
    }

    #[test]
    fn follows_appended_reports_and_starts_over_when_rewritten() {
        let dir = std::env::temp_dir().join(format!("fab-sim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reports.jsonl");
        let full = r#"{"print":{"command":"push_status","msg":0,"gcode_state":"RUNNING","subtask_name":"block","layer_num":1,"total_layer_num":60,"mc_percent":1,"mc_remaining_time":30}}"#;
        std::fs::write(&path, format!("# a synthetic job\n{full}\n")).unwrap();
        let (mut feed, mut reports) = (SimFeed::new(path.clone()), Reports::default());
        let s = next(&mut feed, &mut reports).unwrap();
        assert_eq!((s.layer, s.total_layers, s.job_name.as_deref()), (Some(1), Some(60), Some("block")));
        assert!(next(&mut feed, &mut reports).is_none(), "nothing new");
        // A delta, written in two pieces: only the complete line counts.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"print":{"msg":1,"3D":{"layer_num":7}"#).unwrap();
        assert!(next(&mut feed, &mut reports).is_none());
        f.write_all(b",\"mc_percent\":12}}\n").unwrap();
        let s = next(&mut feed, &mut reports).unwrap();
        assert_eq!((s.layer, s.progress, s.job_name.as_deref()), (Some(1), Some(12.0), Some("block")), "the top-level layer_num leads the 3D mirror");
        // Rewritten shorter: read from the start again.
        std::fs::write(&path, format!("{}\n", full.replace("\"layer_num\":1", "\"layer_num\":3"))).unwrap();
        assert_eq!(next(&mut feed, &mut reports).unwrap().layer, Some(3));
        std::fs::remove_file(&path).unwrap();
        assert!(feed.poll().is_err());
        std::fs::remove_dir(&dir).unwrap();
    }
}
