//! Flash to: the boards the running image can be flashed to, and the flash itself.
//!
//! The list is read on the task pool: at once when an image that can be flashed is built, then
//! every [`LIST_EVERY`] while one is. It holds this PC's USB serial devices from the OS's registry
//! (`makepad_serial::devices`, which never opens a port) and, when a devhub runs on this PC, the
//! devices its `serial` service lists. On the master's machine those are every serving node's,
//! named by their PC and reached through a claim's socket as a `RemoteLine`. On a node's own
//! machine they are its own: the flash claims one there before it opens the port itself, so no
//! devhub session opens it meanwhile. Without a devhub the list is this PC's alone. Each board
//! carries its serial.conf verdict, its holder and whether this write reaches it, so a board that
//! may not be flashed shows greyed with why.
//!
//! Read reads the chosen board's chip and its whole flash's MD5 the same way, writing nothing.
//! A flash runs on the pool, one at a time, with its claim beaten on the pool beside it: it
//! resolves a local device again (gone, moved or turned off since the list is an error), opens
//! the line, writes the image's regions through the ESP ROM loader with each region's MD5
//! checked, boots it, and sends each step back to the UI thread.
//!
//! Monitor watches the chosen board on the pool: it claims and opens the port as a read does,
//! restarts the board through its USB-Serial-JTAG so the stream starts at the image's Hello, and
//! decodes it as the CLI's `watch` does (`fab_elec_mcu_flash::watch`), each line to a log file of
//! its own and, in batches of one frame's lines, to the UI thread, which hands them to the pane's
//! list through a [`MonitorFeed`]. It holds the port until Stop or until the board goes away. A
//! flash or a read meanwhile pauses it: the monitor lets go (within a second, its read timeout),
//! the job runs, and the monitor comes back on the same feed, so the list shows the new image boot
//! below the lines before it. No list is read while the monitor holds the board (its own claim
//! would show the board held).
//!
//! Cost on the UI thread: one channel poll per frame and one `FlashLive` comparison (a few short
//! strings per board); a new `FlashLive` only when a list, a step or a monitor state arrives; per
//! monitor line one level test and one push onto the feed. Listing, asking devhub, flashing and
//! monitoring never run on it.

use fab_elec_mcu_flash::esp_rom::{esp_md5, esp_read_back, esp_write, write_backup, Entry, EspRegion, EspWrite, Exit, Grant, Layout as ReadLayout, ReadStep, Waiver, WriteStep};
use fab_elec_mcu_flash::md5;
use fab_elec_mcu_flash::watch::{watch_until, Reset};
use fab_elec_view::live::sim::{FlashDevice, FlashLive, FlashState, MonitorFeed, MonitorState};
use makepad_remote::conn::CLOSE_NORMAL;
use makepad_remote::link::{Client, Target};
use makepad_remote::Event as LinkEvent;
use makepad_serial::devices::{self, Conf, SerialDevice, Verdict};
use makepad_serial::line::{Line, TtyLine};
use makepad_widgets::makepad_platform::home::{app_data_dir, makepad_home};
use makepad_widgets::makepad_platform::log::LogLevel;
use makepad_widgets::makepad_platform::thread::{Class, TaskHandle, TaskPool, ToUIReceiver, ToUISender};
use makepad_widgets::{log, Cx, Event, Timer};
use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often the list is read again while it is offered (seconds): a board plugged in appears
/// within this.
const LIST_EVERY: f64 = 2.0;

/// The S3's USB-Serial-JTAG while its ROM loader or our runtime runs, as a voicebox S3 board
/// enumerated on the Mac (2026-11-17: "USB JTAG/serial debug unit", 303a:1001). The write below
/// resets into the loader over this port's DTR/RTS, so it reaches no other kind of device.
const S3_USB_SERIAL_JTAG: (u16, u16) = (0x303a, 0x1001);

/// The S3's chip id in the ROM loader's security info, as that board's loader answered it
/// (2026-11-17: chip_id 9, eco 0).
const S3_CHIP_ID: u32 = 9;

/// The line speed the port opens at. The S3's USB-Serial-JTAG ignores it.
const BAUD: u32 = 115_200;

/// devhub's service for USB serial devices on its control socket.
const SERIAL: &str = "serial";

/// How long one answer of this PC's devhub may take. The master's list asks every serving node
/// at once; past this the list shows this PC's boards with the hub's silence as its hint, and the
/// next list asks again.
const HUB_ANSWER: Duration = Duration::from_secs(30);

/// What a job does with the board.
enum Op {
    /// Write the image (its flash bytes from address zero) and a radio image's island (written
    /// only when the board's differs), verify each region, boot it.
    Write(Arc<[u8]>, Option<Arc<Vec<u8>>>),
    /// Read the chip and the whole flash's MD5, then boot the board's own firmware: nothing written.
    Read,
    /// Read the whole flash back, verified block by block against the ROM's MD5, into a new folder
    /// of Fab's data (`backups/<seconds since 1970>`), then boot the board's own firmware. The flag
    /// ends it between 64 KiB blocks.
    Backup(Arc<AtomicBool>),
}

impl Op {
    fn kind(&self) -> Job {
        match self { Op::Write(..) => Job::Flash, Op::Read => Job::Read, Op::Backup(_) => Job::Backup }
    }
}

/// The kind of job under way: its failure's words and its progress state.
#[derive(Clone, Copy, PartialEq)]
enum Job { Flash, Read, Backup }

impl Job {
    fn failed(self, why: String) -> FlashState {
        match self { Job::Flash => FlashState::Failed(why), Job::Read => FlashState::ReadFailed(why), Job::Backup => FlashState::BackupFailed(why) }
    }

    fn running(self, step: &str) -> FlashState {
        match self {
            Job::Flash => FlashState::Flashing(step.into()),
            Job::Read => FlashState::Reading(step.into()),
            Job::Backup => FlashState::BackingUp(step.into()),
        }
    }
}

enum Msg {
    Listed(Listing),
    Step(WriteStep),
    /// A back-up's progress, in a few words.
    Backing(String),
    Done(Result<String, String>),
    /// The monitor's port is open and the board restarted: its log file's path, if one opened.
    Monitoring(Option<String>),
    /// One frame's worth of the monitor's lines.
    Lines(Vec<String>),
    /// The monitor let go of the port: Ok with why when the stream ended by itself or was stopped.
    MonitorDone(Result<String, String>),
}

struct Listing {
    boards: Vec<Board>,
    hint: Option<String>,
}

/// A board in the list.
#[derive(Clone)]
enum Board {
    /// A port on this PC. `hub`: its claim argument in this PC's devhub when the hub lists it
    /// (the flash claims it so no session opens it meanwhile); `held`: who holds it there.
    Here { device: SerialDevice, hub: Option<String>, held: Option<String> },
    /// A device on another PC of the fleet, reached through a claim's socket.
    Fleet { node: String, id: String, label: String, off: Option<String>, held: Option<String> },
    /// A PC that offers no devices now, and why.
    Silent { node: String, why: String },
}

impl Board {
    /// The key the choice follows across lists.
    fn key(&self) -> String {
        match self {
            Board::Here { device, .. } => device.path.clone(),
            Board::Fleet { node, id, .. } => format!("{node}/{id}"),
            Board::Silent { node, .. } => format!("{node}/"),
        }
    }

    fn label(&self) -> String {
        match self {
            Board::Here { device, .. } => device.label(),
            Board::Fleet { node, label, .. } => format!("{label} on {node}"),
            Board::Silent { node, .. } => node.clone(),
        }
    }

    /// The closed list's few words: the chip a write needs (an ESP32-S3's USB-Serial-JTAG port)
    /// or the device's name, and its port here or its PC (`label` stays the list's full words).
    fn face(&self) -> String {
        match self {
            Board::Here { device, .. } => {
                let kind = if (device.ident.vendor, device.ident.product) == S3_USB_SERIAL_JTAG {
                    "ESP32-S3".to_owned()
                } else {
                    device.name.clone().unwrap_or_else(|| format!("{:04x}:{:04x}", device.ident.vendor, device.ident.product))
                };
                let port = device.path.rsplit('/').next().unwrap_or(&device.path);
                let port = port.strip_prefix("cu.").or_else(|| port.strip_prefix("tty.")).unwrap_or(port);
                format!("{kind} \u{b7} {port}")
            }
            Board::Fleet { node, id, label, .. } => {
                let kind = if kind_of(id) == Some(S3_USB_SERIAL_JTAG) { "ESP32-S3" } else { label.split(" on ").next().unwrap_or(label) };
                format!("{kind} \u{b7} {node}")
            }
            Board::Silent { node, .. } => node.clone(),
        }
    }

    /// Why this write cannot go to the board now. None: it can.
    fn unflashable(&self) -> Option<String> {
        let (kind, off, held) = match self {
            Board::Here { device, held, .. } => {
                let off = match &device.verdict { Verdict::Off(why) => Some(why.clone()), Verdict::On => None };
                (Some((device.ident.vendor, device.ident.product)), off, held)
            }
            Board::Fleet { id, off, held, .. } => (kind_of(id), off.clone(), held),
            Board::Silent { why, .. } => return Some(why.clone()),
        };
        off.or_else(|| held.clone())
            .or_else(|| (kind != Some(S3_USB_SERIAL_JTAG)).then(|| "not an ESP32-S3's USB-Serial-JTAG port".to_owned()))
    }
}

/// `<vid>:<pid>` from a devhub device id (`<vid>:<pid>/<serial=S|port=P>[#n]`).
fn kind_of(id: &str) -> Option<(u16, u16)> {
    let (vid, rest) = id.split_once(':')?;
    let pid = rest.split('/').next()?;
    Some((u16::from_str_radix(vid, 16).ok()?, u16::from_str_radix(pid, 16).ok()?))
}

pub struct Flasher {
    boards: Vec<Board>,
    hint: Option<String>,
    /// This PC's devhub: a link made on its first use, and again after it drops.
    hub: Client,
    listing: Option<TaskHandle<()>>,
    /// A list is due: at the start, and when the timer fires.
    due: bool,
    timer: Timer,
    /// The board chosen, by key: it follows the board across lists while it stays listed.
    chosen: Option<String>,
    /// Whether the person chose (a choice made for them gives way to theirs and to a new list).
    picked: bool,
    job: Option<TaskHandle<()>>,
    /// The kind of job under way (its failure's words).
    kind: Job,
    /// A back-up's stop flag while one runs.
    backup: Option<Arc<AtomicBool>>,
    state: FlashState,
    /// The monitor's task while it holds the board, and the flag that ends it.
    monitor: Option<(TaskHandle<()>, Arc<AtomicBool>)>,
    monitor_state: MonitorState,
    feed: Option<Arc<MonitorFeed>>,
    /// A flash or a read waiting for the monitor to let go of the board; the monitor comes back
    /// after it.
    waiting: Option<Op>,
    /// The monitor comes back when the job under way ends.
    resume: bool,
    rx: ToUIReceiver<Msg>,
    live: Option<FlashLive>,
}

impl Default for Flasher {
    fn default() -> Self {
        let socket = makepad_network::fleet::makepad_home().join("devhub").join("devhub.sock");
        Self {
            boards: Vec::new(), hint: None, hub: Client::new(Target::Unix(socket), "fab"), listing: None, due: true,
            timer: Timer::empty(), chosen: None, picked: false, job: None, kind: Job::Read, backup: None, state: FlashState::Idle,
            monitor: None, monitor_state: MonitorState::Off, feed: None, waiting: None, resume: false,
            rx: ToUIReceiver::default(), live: None,
        }
    }
}

impl Flasher {
    /// What the pane shows; None while nothing can be flashed.
    pub fn live(&self) -> Option<&FlashLive> {
        self.live.as_ref()
    }

    /// Once per event: the list timer makes a list due on the next frame.
    pub fn event(&mut self, cx: &mut Cx, event: &Event) {
        if self.timer.is_event(event).is_some() {
            self.timer = Timer::empty();
            self.due = true;
            cx.new_next_frame();
        }
    }

    /// The person chooses the board with `key`.
    pub fn choose(&mut self, key: &str) {
        if self.boards.iter().any(|board| board.key() == key) {
            self.chosen = Some(key.to_owned());
            self.picked = true;
            self.publish();
        }
    }

    /// Takes what the pool sent and lists when due; true when the pane's view changed.
    /// `image`: the running image's flash bytes from address zero, when it can be flashed.
    pub fn frame(&mut self, cx: &mut Cx, image: Option<&Arc<[u8]>>) -> bool {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Listed(listing) => {
                    self.listing = None;
                    self.boards = listing.boards;
                    self.hint = listing.hint;
                    self.follow_choice();
                    if image.is_some() {
                        self.timer = cx.start_timeout(LIST_EVERY);
                    }
                }
                Msg::Step(step) => self.state = FlashState::Flashing(step_text(step)),
                Msg::Backing(step) => self.state = FlashState::BackingUp(step),
                Msg::Done(result) => {
                    self.job = None;
                    self.backup = None;
                    if let Some(feed) = self.feed.as_ref().filter(|_| self.resume) {
                        let (level, text) = match &result { Ok(text) => (LogLevel::Log, text.clone()), Err(why) => (LogLevel::Error, why.clone()) };
                        feed.push(level, format!("Fab: {text}"));
                    }
                    self.state = match result {
                        Ok(text) => FlashState::Done(text),
                        Err(text) => self.kind.failed(text),
                    };
                    if std::mem::take(&mut self.resume) {
                        self.start_monitor(cx);
                    }
                }
                Msg::Monitoring(file) => {
                    self.monitor_state = MonitorState::On;
                    if let Some(feed) = &self.feed {
                        feed.push(LogLevel::Log, match file {
                            Some(file) => format!("Fab: monitoring; every line also goes to {file}"),
                            None => "Fab: monitoring; no log file could be opened (Fab's log says why)".into(),
                        });
                    }
                }
                Msg::Lines(lines) => {
                    if let Some(feed) = &self.feed {
                        for line in lines {
                            feed.push(level_of(&line), line);
                        }
                    }
                    // the pane takes them on its next frame
                    cx.new_next_frame();
                }
                Msg::MonitorDone(result) => {
                    let stopped = self.monitor.take().is_some_and(|(_, stop)| stop.load(Ordering::Relaxed));
                    match self.waiting.take() {
                        Some(op) => {
                            if !self.run_job(cx, op) && std::mem::take(&mut self.resume) {
                                // the job was refused: the monitor comes back at once
                                self.start_monitor(cx);
                            }
                        }
                        None if stopped => self.monitor_state = MonitorState::Off,
                        None => self.monitor_state = MonitorState::Ended(match result { Ok(why) | Err(why) => why }),
                    }
                }
            }
        }
        if image.is_none() {
            // listed again at once when an image comes back
            self.due = true;
        } else if self.due && self.listing.is_none() && self.job.is_none() && self.monitor.is_none() {
            // no list while a flash or the monitor runs: its own claim would show its board as held
            self.due = false;
            let tx = self.rx.sender();
            let hub = self.hub.clone();
            match cx.task_pool().submit_named(Class::Background, "elec flash list", move || {
                let _ = tx.send(Msg::Listed(list(&hub)));
            }) {
                Ok(task) => self.listing = Some(task),
                Err(error) => {
                    log!("electronics flash: the list did not start: {error}; listing again in {LIST_EVERY} s");
                    self.timer = cx.start_timeout(LIST_EVERY);
                }
            }
        }
        self.publish_for(image.is_some())
    }

    /// Flash `image` to the chosen board; true when the pane's view changed. A board that is
    /// missing, off, held or out of this write's reach, or a flash already running, is refused in
    /// the state's words.
    /// `island`: a radio image's linked island (`SimRadio::island`).
    pub fn flash(&mut self, cx: &mut Cx, image: &Arc<[u8]>, island: Option<&Arc<Vec<u8>>>) -> bool {
        self.start(cx, Op::Write(image.clone(), island.cloned()))
    }

    /// Read the chosen board's chip and its whole flash's MD5, nothing written; true when the
    /// pane's view changed. Refused as a flash is.
    pub fn read(&mut self, cx: &mut Cx) -> bool {
        self.start(cx, Op::Read)
    }

    /// Back up the chosen board's whole flash to a new folder of Fab's data, or stop the back-up
    /// under way (it ends after the block it reads); true when the pane's view changed.
    pub fn backup(&mut self, cx: &mut Cx) -> bool {
        if let Some(stop) = &self.backup {
            stop.store(true, Ordering::Relaxed);
            self.state = FlashState::BackingUp("stopping after this block".into());
            return self.publish();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let changed = self.start(cx, Op::Backup(stop.clone()));
        if self.job.is_some() || matches!(self.waiting, Some(Op::Backup(_))) {
            self.backup = Some(stop);
        }
        changed
    }

    /// Start the monitor on the chosen board, or stop it; true when the pane's view changed. A
    /// start empties the pane's list (a new feed); a board that may not be flashed is refused as a
    /// flash is, in the monitor's state.
    pub fn monitor(&mut self, cx: &mut Cx) -> bool {
        if let Some((_, stop)) = &self.monitor {
            // it ends within its read timeout; a job waiting for it still runs, without it after
            stop.store(true, Ordering::Relaxed);
            self.resume = false;
            if self.waiting.is_some() {
                self.monitor_state = MonitorState::Off;
            }
            return self.publish();
        }
        if self.job.is_some() {
            // a flash or a read holds the board: the monitor starts (or no longer comes back) after it
            self.resume = !self.resume;
            if self.resume && self.monitor_state == MonitorState::Off {
                self.feed = Some(Arc::new(MonitorFeed::default()));
            }
            self.monitor_state = if self.resume { MonitorState::Paused } else { MonitorState::Off };
            return self.publish();
        }
        self.feed = Some(Arc::new(MonitorFeed::default()));
        self.start_monitor(cx);
        self.publish()
    }

    fn start_monitor(&mut self, cx: &mut Cx) {
        let board = match self.chosen_board() {
            None => Err("choose a board".to_owned()),
            Some(board) => match board.unflashable() {
                Some(why) => Err(format!("{} cannot be monitored: {why}", board.label())),
                None => Ok(board.clone()),
            },
        };
        let board = match board {
            Ok(board) => board,
            Err(why) => {
                self.monitor_state = MonitorState::Ended(why);
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        let tx = self.rx.sender();
        let hub = self.hub.clone();
        let pool = cx.task_pool();
        let flag = stop.clone();
        match cx.task_pool().submit_named(Class::Background, "elec monitor", move || {
            let mut job = MonitorJob { stop: flag, sink: LineSink::new(tx.clone()) };
            let result = on_board(&board, &hub, &pool, false, &mut job);
            let _ = tx.send(Msg::MonitorDone(result));
        }) {
            Ok(task) => {
                self.monitor = Some((task, stop));
                self.monitor_state = MonitorState::Starting;
            }
            Err(error) => self.monitor_state = MonitorState::Ended(format!("the monitor did not start: {error}")),
        }
    }

    fn start(&mut self, cx: &mut Cx, op: Op) -> bool {
        if self.job.is_some() || self.waiting.is_some() {
            return false;
        }
        if let Some((_, stop)) = &self.monitor {
            // the monitor lets go of the board first; the job starts when it has
            stop.store(true, Ordering::Relaxed);
            self.monitor_state = MonitorState::Paused;
            self.resume = true;
            self.state = op.kind().running("pausing the monitor");
            self.waiting = Some(op);
            return self.publish();
        }
        self.run_job(cx, op);
        self.publish()
    }

    /// Start a flash, a read or a back-up on the pool; false when it was refused (the state says
    /// why).
    fn run_job(&mut self, cx: &mut Cx, op: Op) -> bool {
        let kind = op.kind();
        let refused = |why: String| kind.failed(why);
        let board = match self.chosen_board() {
            None => Err("choose a board".to_owned()),
            Some(board) => match board.unflashable() {
                Some(why) => Err(format!("{} is not flashable: {why}", board.label())),
                None => Ok(board.clone()),
            },
        };
        let board = match board {
            Ok(board) => board,
            Err(why) => {
                self.state = refused(why);
                self.backup = None;
                return false;
            }
        };
        let tx = self.rx.sender();
        let hub = self.hub.clone();
        let pool = cx.task_pool();
        match cx.task_pool().submit_named(Class::Background, "elec flash", move || {
            let flash = matches!(op, Op::Write(..));
            let mut job = OpJob { op: &op, tx: tx.clone() };
            let result = on_board(&board, &hub, &pool, flash, &mut job);
            let _ = tx.send(Msg::Done(result));
        }) {
            Ok(task) => {
                self.job = Some(task);
                self.kind = kind;
                self.state = kind.running(match kind {
                    Job::Flash => "opening the port",
                    Job::Read => "the chip and the whole flash's MD5",
                    Job::Backup => "the ROM's MD5 of every 64 KiB block",
                });
                true
            }
            Err(error) => {
                self.state = refused(format!("the job did not start: {error}"));
                self.backup = None;
                false
            }
        }
    }

    /// The person's choice holds while its board is listed; otherwise the only board this write
    /// can reach is chosen for them (with several, or none, the person chooses).
    fn follow_choice(&mut self) {
        if self.picked && self.chosen_board().is_some() {
            return;
        }
        self.picked = false;
        let mut reachable = self.boards.iter().filter(|board| board.unflashable().is_none());
        self.chosen = match (reachable.next(), reachable.next()) {
            (Some(only), None) => Some(only.key()),
            _ => None,
        };
    }

    fn chosen_board(&self) -> Option<&Board> {
        let chosen = self.chosen.as_deref()?;
        self.boards.iter().find(|board| board.key() == chosen)
    }

    fn publish(&mut self) -> bool {
        let offered = self.live.is_some();
        self.publish_for(offered)
    }

    fn publish_for(&mut self, offered: bool) -> bool {
        let live = offered.then(|| FlashLive {
            devices: self.boards.iter().map(|board| FlashDevice {
                path: board.key(),
                label: board.label(),
                face: board.face(),
                off: board.unflashable(),
            }).collect(),
            chosen: self.chosen_board().map(Board::key),
            hint: self.hint.clone().or_else(|| self.boards.is_empty().then(|| "no USB serial board on this PC".to_owned())),
            state: self.state.clone(),
            monitor: self.monitor_state.clone(),
            feed: self.feed.clone(),
        });
        if live == self.live {
            return false;
        }
        self.live = live;
        true
    }
}

/// This PC's boards and, when a devhub answers, the fleet's. A devhub that is not there leaves
/// the list this PC's; one that fails says why in the hint.
fn list(hub: &Client) -> Listing {
    let mut hint = None;
    let mut boards: Vec<Board> = match devices::list(&Conf::read()) {
        Ok(devices) => devices.into_iter().map(|device| Board::Here { device, hub: None, held: None }).collect(),
        Err(err) => {
            hint = Some(format!("this PC's USB devices do not list: {err}"));
            Vec::new()
        }
    };
    let rows = match ask(hub, "list") {
        Ok(rows) => rows,
        // no devhub on this PC (or one without the serial service): its own boards only
        Err(Asked::NoHub) => return Listing { boards, hint },
        Err(Asked::Failed(why)) => {
            hint.get_or_insert(format!("devhub's device list: {why}"));
            return Listing { boards, hint };
        }
    };
    for row in rows {
        let text = |key: &str| row.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        let node = text("node");
        if let Some(why) = text("none") {
            boards.push(Board::Silent { node: node.unwrap_or_default(), why });
            continue;
        }
        if let Some(why) = text("err") {
            hint.get_or_insert(format!("devhub's device list: {why}"));
            continue;
        }
        let (Some(id), Some(label), Some(path)) = (text("id"), text("label"), text("path")) else { continue };
        let held = text("holder").map(|holder| {
            let since = row.get("held_s").and_then(|v| v.as_u64()).unwrap_or(0);
            format!("claimed by {holder} ({since} s ago)")
        });
        // a row of this PC: its own hub's (no node), or the master's when this PC is a node too
        let here = boards.iter().position(|board| match board {
            Board::Here { device, .. } => {
                device.path == path && device.label() == label && kind_of(&id) == Some((device.ident.vendor, device.ident.product))
                    && device.ident.serial.as_ref().is_none_or(|serial| id.contains(&format!("/serial={serial}")))
            }
            _ => false,
        });
        match (here, node) {
            (Some(at), node) => {
                if let Board::Here { hub, held: holder, .. } = &mut boards[at] {
                    *hub = Some(match node { Some(node) => format!("{node}/{id}"), None => id });
                    *holder = held;
                }
            }
            (None, Some(node)) => boards.push(Board::Fleet { node, id, label, off: text("off"), held }),
            // this PC's hub lists a port the registry no longer did: the next list settles it
            (None, None) => {}
        }
    }
    Listing { boards, hint }
}

enum Asked {
    /// No devhub answers on this PC's control socket, or it has no serial service.
    NoHub,
    Failed(String),
}

/// One request on devhub's `serial` service: its answer's lines, as JSON objects.
fn ask(hub: &Client, request: &str) -> Result<Vec<makepad_strict_json::Value>, Asked> {
    let channel = hub.open(SERIAL, "").map_err(|_| Asked::NoHub)?;
    channel.send(format!("{request}\n").into_bytes()).map_err(|err| Asked::Failed(format!("the request did not go: {err}")))?;
    let mut rows = Vec::new();
    let mut part = Vec::new();
    loop {
        match channel.recv_timeout(HUB_ANSWER) {
            Ok(LinkEvent::Data(data)) => {
                part.extend_from_slice(&data);
                while let Some(at) = part.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<u8> = part.drain(..=at).collect();
                    let line = &line[..at];
                    if line.starts_with(b"{\"end\":") {
                        channel.close(CLOSE_NORMAL, "answered");
                        return Ok(rows);
                    }
                    if !line.iter().all(u8::is_ascii_whitespace) {
                        rows.push(makepad_strict_json::parse(line)
                            .map_err(|err| Asked::Failed(format!("an answer line that is no JSON ({err})")))?);
                    }
                }
            }
            Ok(LinkEvent::Opened(_)) => {}
            Ok(LinkEvent::Closed(_, why)) => return Err(Asked::Failed(format!("devhub ended the answer: {why}"))),
            Err(true) => {
                channel.close(CLOSE_NORMAL, "no answer");
                return Err(Asked::Failed(format!("no answer to `{request}` within {} s", HUB_ANSWER.as_secs())));
            }
            Err(false) => return Err(Asked::Failed("the link to devhub dropped".into())),
        }
    }
}

/// The first answer line of a request, or its `err` as the failure.
fn ask_one(hub: &Client, request: &str) -> Result<makepad_strict_json::Value, String> {
    let row = match ask(hub, request) {
        Ok(rows) => rows.into_iter().next().ok_or_else(|| format!("devhub gave no answer to `{request}`"))?,
        Err(Asked::NoHub) => return Err("devhub does not answer on this PC".into()),
        Err(Asked::Failed(why)) => return Err(why),
    };
    match row.get("err").and_then(|v| v.as_str()) {
        Some(err) => Err(err.to_owned()),
        None => Ok(row),
    }
}

/// A devhub claim, beaten on the pool while it lives and released when it is dropped.
struct Claim {
    hub: Client,
    claim: String,
    socket: Option<String>,
    /// set when the claim ends; wakes the beat at once
    ended: Arc<(Mutex<bool>, Condvar)>,
    beat: Option<TaskHandle<()>>,
}

impl Claim {
    /// `flash`: a write, which holds devhub's restarts while it runs.
    fn take(hub: &Client, pool: &TaskPool, device: &str, flash: bool) -> Result<Claim, String> {
        let holder = format!("Fab on {}", makepad_devhub_conf::host_name());
        let answer = ask_one(hub, &format!("claim {device} {holder}{}", if flash { " --flash" } else { "" }))?;
        let text = |key: &str| answer.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        let claim = text("claim").ok_or("devhub's claim answer names no claim")?;
        let every = Duration::from_secs(answer.get("beat_s").and_then(|v| v.as_u64()).unwrap_or(2).max(1));
        let ended = Arc::new((Mutex::new(false), Condvar::new()));
        let (beat_hub, beat_claim, beat_ended) = (hub.clone(), claim.clone(), ended.clone());
        let beat = pool.submit_named(Class::Background, "elec flash beat", move || {
            let (lock, wake) = &*beat_ended;
            let mut done = lock.lock().unwrap();
            loop {
                done = wake.wait_timeout(done, every).unwrap().0;
                if *done {
                    return;
                }
                if let Err(err) = ask_one(&beat_hub, &format!("beat {beat_claim}")) {
                    // an open session keeps its claim through a gap in beats; the flash's own
                    // errors say what ended it
                    log!("electronics flash: the claim's beat: {err}");
                }
            }
        });
        let beat = beat.map_err(|err| {
            let _ = ask_one(hub, &format!("release {claim}"));
            format!("the claim's beat did not start: {err}")
        })?;
        Ok(Claim { hub: hub.clone(), claim, socket: text("socket"), ended, beat: Some(beat) })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        *self.ended.0.lock().unwrap() = true;
        self.ended.1.notify_all();
        if let Some(beat) = self.beat.take() {
            beat.detach();
        }
        if let Err(err) = ask_one(&self.hub, &format!("release {}", self.claim)) {
            log!("electronics flash: releasing claim {}: {err}", self.claim);
        }
    }
}

fn step_text(step: WriteStep) -> String {
    match step {
        WriteStep::Connected => "in the ROM loader".into(),
        WriteStep::Fingerprinted => "the board's flash checked".into(),
        WriteStep::Writing { index, of, done, size } => format!("writing {}/{of}: {}%", index + 1, u64::from(done) * 100 / u64::from(size.max(1))),
        WriteStep::Verified { index } => format!("region {} verified", index + 1),
        WriteStep::Unchanged { index } => format!("region {} unchanged", index + 1),
        WriteStep::Booting => "starting the image".into(),
    }
}

/// What a job does with the board's line once it is claimed and open.
trait OnLine {
    fn run<L: Line>(&mut self, line: &mut L, label: &str) -> Result<String, String>;
}

/// `flash`: a write, whose claim holds devhub's restarts while it runs.
fn on_board(board: &Board, hub: &Client, pool: &TaskPool, flash: bool, job: &mut impl OnLine) -> Result<String, String> {
    let label = board.label();
    match board {
        Board::Here { device, hub: claim, .. } => {
            let _claim = claim.as_deref().map(|id| Claim::take(hub, pool, id, flash)).transpose()
                .map_err(|err| format!("{label}: {err}"))?;
            let path = devices::resolve(device).map_err(|err| err.to_string())?;
            let mut line = TtyLine::open(&path, BAUD).map_err(|err| format!("{label}: {err}"))?;
            job.run(&mut line, &label)
        }
        Board::Fleet { node, id, .. } => {
            let claim = Claim::take(hub, pool, &format!("{node}/{id}"), flash).map_err(|err| format!("{label}: {err}"))?;
            remote_op(&claim, node, &label, job)
        }
        Board::Silent { why, .. } => Err(format!("{label}: {why}")),
    }
}

#[cfg(unix)]
fn remote_op(claim: &Claim, node: &str, label: &str, job: &mut impl OnLine) -> Result<String, String> {
    use makepad_serial::remote::{Mode, RemoteLine};
    let socket = claim.socket.as_deref().ok_or_else(|| format!("{label}: devhub's claim answer has no socket"))?;
    let stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|err| format!("{label}: the claim's socket does not answer: {err}"))?;
    let mut line = RemoteLine::open(stream, node, Mode::Serial, BAUD).map_err(|err| format!("{label}: {err}"))?;
    job.run(&mut line, label)
}

#[cfg(not(unix))]
fn remote_op(claim: &Claim, _node: &str, label: &str, _job: &mut impl OnLine) -> Result<String, String> {
    let socket = claim.socket.as_deref().unwrap_or("none");
    Err(format!("{label}: a board on another PC is reached through the claim's Unix socket ({socket}), which Fab does not open on this OS yet"))
}

/// A flash, a read or a back-up; its steps go to the UI thread.
struct OpJob<'a> {
    op: &'a Op,
    tx: ToUISender<Msg>,
}

impl OnLine for OpJob<'_> {
    fn run<L: Line>(&mut self, line: &mut L, label: &str) -> Result<String, String> {
        match self.op {
            Op::Write(flash, island) => write_s3(line, label, flash, island.as_deref().map(Vec::as_slice), &mut |step| { let _ = self.tx.send(Msg::Step(step)); }),
            Op::Read => read_s3(line, label),
            Op::Backup(stop) => backup_s3(line, label, stop, &self.tx),
        }
    }
}

/// The monitor: the board restarted through its USB-Serial-JTAG and its watch stream decoded until
/// `stop` or until the port goes away. Ok says why it ended.
struct MonitorJob {
    stop: Arc<AtomicBool>,
    sink: LineSink,
}

impl OnLine for MonitorJob {
    fn run<L: Line>(&mut self, line: &mut L, label: &str) -> Result<String, String> {
        let file = self.sink.open_file(label);
        let _ = self.sink.tx.send(Msg::Monitoring(file));
        let stop = self.stop.clone();
        let watched = watch_until(line, Reset::Usj, None, &mut self.sink, move || stop.load(Ordering::Relaxed));
        let _ = self.sink.flush();
        watched.map_err(|err| format!("{label}: {err}"))?;
        Ok(if self.stop.load(Ordering::Relaxed) { "stopped".into() } else { format!("{label} went away") })
    }
}

/// The monitor's output as lines: each to the log file as it completes, and one frame's lines to
/// the UI thread at each flush (the watch flushes after every frame it prints).
struct LineSink {
    tx: ToUISender<Msg>,
    file: Option<File>,
    part: Vec<u8>,
    batch: Vec<String>,
}

impl LineSink {
    fn new(tx: ToUISender<Msg>) -> Self {
        Self { tx, file: None, part: Vec::new(), batch: Vec::new() }
    }

    /// A new log file in Fab's data folder (`monitor/<seconds since 1970>.log`): its path, or
    /// None (logged) when it cannot be made; the lines still reach the pane.
    fn open_file(&mut self, label: &str) -> Option<String> {
        let dir = app_data_dir("fab").unwrap_or_else(|| makepad_home().join("fab")).join("monitor");
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let file_path = dir.join(format!("{secs}.log"));
        let opened = std::fs::create_dir_all(&dir).and_then(|()| File::create(&file_path)).and_then(|mut file| {
            writeln!(file, "Fab monitor of {label}")?;
            Ok(file)
        });
        match opened {
            Ok(file) => {
                self.file = Some(file);
                Some(file_path.display().to_string())
            }
            Err(err) => {
                log!("electronics monitor: no log file at {}: {err}", file_path.display());
                None
            }
        }
    }
}

impl Write for LineSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.part.extend_from_slice(bytes);
        while let Some(at) = self.part.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.part.drain(..=at).collect();
            if let Some(file) = &mut self.file {
                if let Err(err) = file.write_all(&line) {
                    // the pane keeps the lines; the file stops here, and says so in Fab's log
                    log!("electronics monitor: the log file stopped: {err}");
                    self.file = None;
                }
            }
            self.batch.push(String::from_utf8_lossy(&line[..at]).into_owned());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.batch.is_empty() {
            let _ = self.tx.send(Msg::Lines(std::mem::take(&mut self.batch)));
        }
        if let Some(file) = &mut self.file {
            if let Err(err) = file.flush() {
                log!("electronics monitor: the log file stopped: {err}");
                self.file = None;
            }
        }
        Ok(())
    }
}

/// The list's level for a watch line (`<seq> <tick> <event> ...`): faults are errors, lost
/// frames warnings, the rest notes.
fn level_of(line: &str) -> LogLevel {
    match line.split(' ').nth(2) {
        Some("fault") => LogLevel::Error,
        Some("lost") => LogLevel::Warning,
        _ => LogLevel::Log,
    }
}

/// `island`: a radio image's linked island, written after the app.
fn write_s3<L: Line>(line: &mut L, label: &str, flash: &[u8], island: Option<&[u8]>, watch: &mut dyn FnMut(WriteStep)) -> Result<String, String> {
    let mut regions: Vec<EspRegion> = fab_elec_mcu_emit_s3::flash_regions(flash).iter()
        .map(|&(offset, bytes)| EspRegion { offset, bytes: bytes.to_vec() }).collect();
    // The island goes last, after the app, its header sectors after its body (`island_regions`):
    // a write cut short never leaves the board a half-written island it would start. Like every
    // region each is written only when the board's differs; the island rarely changes.
    if let Some(island) = island.filter(|island| !island.is_empty()) {
        let island = fab_elec_mcu_emit_s3::island_regions(island).map_err(|err| format!("{label}: {err}"))?;
        regions.extend(island.into_iter().map(|(offset, bytes)| EspRegion { offset, bytes: bytes.to_vec() }));
    }
    let plan = EspWrite {
        regions,
        flash_size: fab_elec_mcu_emit_s3::VOICEBOX.flash_bytes,
        expect_chip_id: S3_CHIP_ID,
        grant: Grant { device: label.to_owned(), said_by: "the person, with Flash to in Fab".into() },
        waiver: Waiver { reason: "Fab writes the image's head, slot A, its file bundle and a radio image's island only; the board's whole-flash MD5 from before the write is in Fab's log".into() },
    };
    let report = esp_write(line, &plan, watch).map_err(|err| format!("{label}: {err}"))?;
    log!("electronics flash: {label}: the whole flash before the write had MD5 {}", md5::hex(&report.before));
    for region in &report.regions {
        let how = if report.unchanged.contains(&region.offset) { "unchanged" } else { "verified" };
        log!("electronics flash: {label}: 0x{:06x}, {} bytes, {how} MD5 {}", region.offset, region.size, md5::hex(&region.md5));
    }
    let unchanged = match report.unchanged.len() {
        0 => String::new(),
        n => format!(" ({n} unchanged, not written)"),
    };
    Ok(format!("flashed {label}: {} regions verified{unchanged}, running", report.regions.len()))
}

/// The chip and the whole flash's MD5 through the ROM loader, then the board's own firmware boots.
fn read_s3<L: Line>(line: &mut L, label: &str) -> Result<String, String> {
    let size = fab_elec_mcu_emit_s3::VOICEBOX.flash_bytes;
    let report = esp_md5(line, Entry::Reset, 0, size, size, Exit::Boot).map_err(|err| format!("{label}: {err}"))?;
    let md5 = md5::hex(&report.region.md5);
    log!("electronics flash: {label}: read {}; the whole flash ({size} bytes) has MD5 {md5}", report.security);
    Ok(format!("read {label}: chip id {}, the whole flash's MD5 {md5}, its firmware running again", report.security.chip_id))
}

/// The whole flash read back (the ROM loader's SPI1 reads, every 64 KiB block checked against the
/// ROM's MD5 and read again while a failed read differs), written to a new folder of Fab's data,
/// then the board's own firmware boots. About 23 KB/s on the S3's USB-Serial-JTAG (12 minutes for
/// 16 MB, 2026-11-17).
fn backup_s3<L: Line>(line: &mut L, label: &str, stop: &AtomicBool, tx: &ToUISender<Msg>) -> Result<String, String> {
    let size = fab_elec_mcu_emit_s3::VOICEBOX.flash_bytes;
    let mut progress = |step: ReadStep| {
        let text = match step {
            ReadStep::Fingerprinted { blocks } => format!("reading {blocks} blocks of 64 KiB"),
            ReadStep::Retry { offset } => format!("reading the block at 0x{offset:06x} again (it differed from the ROM's MD5)"),
            ReadStep::Read { done, size, rate } => format!("{} of {} MB, {} min left",
                done / (1024 * 1024), size / (1024 * 1024), (f64::from(size - done) / rate / 60.0).ceil()),
        };
        let _ = tx.send(Msg::Backing(text));
    };
    let back = match esp_read_back(line, Entry::Reset, size, ReadLayout::Low, Exit::Boot, &mut progress, &|| stop.load(Ordering::Relaxed)) {
        Ok(back) => back,
        // Cancel ends the read after its block, then the board boots as after any read.
        Err(_) if stop.load(Ordering::Relaxed) => return Ok(format!("back-up of {label} cancelled, nothing kept; its firmware running again")),
        Err(err) => return Err(format!("{label}: {err}")),
    };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    let dir = app_data_dir("fab").unwrap_or_else(|| makepad_home().join("fab")).join("backups").join(secs.to_string());
    write_backup(&dir, &back).map_err(|err| format!("{label}: the back-up was read but not written to {}: {err}", dir.display()))?;
    let md5 = md5::hex(&back.whole_md5);
    log!("electronics flash: {label}: backed up {size} bytes, MD5 {md5}, SHA-256 {}, to {}", back.sha256, dir.display());
    Ok(format!("backed up {label}: the whole flash's MD5 {md5}, in {}, its firmware running again", dir.display()))
}
