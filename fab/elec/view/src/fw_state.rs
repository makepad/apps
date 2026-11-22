//! The Firmware pane's state panel as text: the co-simulated device's state at the newest
//! publication (`SimLive::co`, a [`FwState`]) as the lines the panel shows, section by section.
//!
//! Every line is a value of the state (`elec.fw.state` reads the same struct): where the device
//! stands (its handler and the statement's line, the runtime routine, asleep until a wake, halted),
//! the analog time and cycle, the program's variables, the pins with their use, level and PWM duty,
//! the timers, the pending interrupts and received bytes, a stop of the radio on a lost or
//! overwritten timer, the radio restart count and last cause, the faults the image reported, and
//! the registers. Faults are tallied across radio starts (`FwState::fault_counts`), one line per
//! kind and site with its count, so a fault in a publication the pane never read still shows.
//!
//! Each publication the pane reads formats the variables (including their array words), pins,
//! timers, pending work, fault kinds and registers once. An unchanged publication costs nothing.
//!
//! What the source shows of the device (Q2), two visuals that never mix: while playing, a
//! [`Heat`] (each statement that began since the publication the pane read before pulses and
//! fades in wall time, by the device's cumulative counts, so a statement that ran only in a
//! publication the pane never read still pulses); paused or stopped, one [`marker`] on the
//! statement about to execute, hollow on the last one that ran while the device sleeps. Per
//! publication O(statements that have run); per frame O(hot lines).
//!
//! Beside them, whatever the run does (Q3): each condition the firmware branched on in the colour
//! of its newest value ([`cond_marks`], true or false), and the lines of statements that never ran
//! on the device dimmed ([`unrun_lines`]). Per publication O(statements + conditions), both
//! placed through the source's line starts ([`SourceLines`], O(text) once per text).

use crate::live::{sim::{NetworkLive, RadioLive, RadioState, TokenLive}, strip::format_time};
use fab_elec_mcu_cosim::{net, watch, At, FwCondCount, FwPin, FwState, NetStatus, RadioStarts};
use std::ops::Range;
use fab_elec_mcu_emu::{FwScalar, FwTimer, FwTy, FwValue, FwVar, HaltKind, Pending, PinUse, TimerEvent};

/// The radio pack's manifest, read once: the Radio row states its facts from it, as Fab's licences
/// page does, so the two never disagree.
pub fn radio_pack() -> &'static fab_elec_mcu_pack::Manifest {
    static PACK: std::sync::OnceLock<fab_elec_mcu_pack::Manifest> = std::sync::OnceLock::new();
    PACK.get_or_init(fab_elec_mcu_pack::espressif_s3)
}

/// The Radio row's tooltip: where the libraries come from and their licence, from the manifest.
pub fn radio_source() -> String {
    let pack = radio_pack();
    let from = pack.repositories.iter().map(|r| {
        // url_base is https://<host>/<owner>/<name>/<commit>/
        let owner = r.url_base.split('/').nth(3).unwrap_or("");
        format!("{owner}/{} {}", r.name, &r.commit[..r.commit.len().min(7)])
    }).collect::<Vec<_>>().join(" and ");
    format!("Espressif's {} Wi-Fi and PHY libraries, {}, from {from}", pack.chip, pack.license)
}

/// The status beside the Firmware pane's Radio controls, in decimal KB and MB.
pub fn radio_face(r: &RadioLive) -> String {
    fn size(bytes: u64) -> String {
        match bytes {
            0..=999 => format!("{bytes} B"),
            1000..=999_999 => format!("{} KB", (bytes + 500) / 1000),
            _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
        }
    }
    match &r.state {
        RadioState::Absent => format!("Wi-Fi libraries (Espressif, {}, {}): not installed", radio_pack().license, size(r.total_bytes)),
        RadioState::Fetching { done, file } => {
            let percent = u128::from((*done).min(r.total_bytes)) * 100 / u128::from(r.total_bytes.max(1));
            if file.is_empty() { format!("getting {percent}%") } else { format!("getting {percent}%: {file}") }
        }
        RadioState::Installing => "installing".into(),
        RadioState::Linking => "linking".into(),
        RadioState::Ready { island_bytes } => format!(
            "ready: radio island {}; the simulator answers its calls with Fab's stand-in, and Espressif's code runs only on the board",
            size(*island_bytes)),
        RadioState::Failed(reason) => format!("failed: {reason}"),
        RadioState::Cancelled => "cancelled".into(),
    }
}

/// One name and value at the playhead.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StateRow {
    pub name: String,
    /// A scalar's value; a block's size (`[128]` elements of an array, `[12 B]` of another layout).
    pub value: String,
    pub truth: Option<bool>,
    /// An array's elements or another layout's bytes, which the panel folds to one line.
    pub block: Option<Block>,
}

/// A block value's cells, each `width` characters: an array's elements (bools as 0 and 1, ints
/// in hex as wide as the widest needs, floats as decimals) by index, or another layout's bytes in
/// hex by hex offset.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Block {
    /// The cells back to back, each right-aligned in `width` characters.
    pub cells: String,
    pub width: usize,
    /// Offsets count bytes in hex; else elements in decimal.
    pub bytes: bool,
}

impl Block {
    pub fn len(&self) -> usize {
        self.cells.len() / self.width.max(1)
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn cell(&self, i: usize) -> &str {
        &self.cells[i * self.width..(i + 1) * self.width]
    }

    /// The offset label of the cell at `i`.
    pub fn offset(&self, i: usize) -> String {
        if self.bytes { format!("{i:x}") } else { i.to_string() }
    }

    fn new(texts: Vec<String>, bytes: bool) -> Block {
        let width = texts.iter().map(|t| t.len()).max().unwrap_or(1).max(1);
        let mut cells = String::with_capacity(width * texts.len());
        for t in &texts {
            cells.extend(std::iter::repeat_n(' ', width - t.len()));
            cells.push_str(t);
        }
        Block { cells, width, bytes }
    }
}

/// A block's size as the panel and the source show it: an array's elements (`[128]`), another
/// layout's bytes (`[12 B]`).
fn block_size(var: &FwVar, words: &[u32]) -> String {
    match var.ty {
        FwTy::Array(_, n) => format!("[{n}]"),
        _ => format!("[{} B]", words.len() * 4),
    }
}

/// A variable's row: a scalar's value, or a block's size and cells.
fn var_row(var: &FwVar, value: &FwValue) -> StateRow {
    let truth = if let FwValue::Bool(b) = value { Some(*b) } else { None };
    let row = |value: String, block| StateRow { name: var.name.clone(), value, truth, block };
    let FwValue::Words(words) = value else { return row(value_text(value), None) };
    match var.ty {
        FwTy::Array(elem, _) => {
            let texts = match elem {
                FwScalar::Bool => words.iter().map(|w| if *w != 0 { "1".to_string() } else { "0".to_string() }).collect(),
                FwScalar::F32 => words.iter().map(|w| f32::from_bits(*w).to_string()).collect(),
                FwScalar::I32 => {
                    let digits = words.iter().map(|w| (32 - w.leading_zeros()).div_ceil(4) as usize).max().unwrap_or(0).max(2);
                    words.iter().map(|w| format!("{w:0digits$x}")).collect()
                }
            };
            row(block_size(var, words), Some(Block::new(texts, false)))
        }
        _ => {
            let texts = words.iter().flat_map(|w| w.to_le_bytes()).map(|b| format!("{b:02x}")).collect();
            row(block_size(var, words), Some(Block::new(texts, true)))
        }
    }
}

/// The panel's heading, rows and section text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateText {
    pub head: String,
    pub rows: Vec<StateRow>,
    /// Where the device stands.
    pub at: String,
    pub pins: Vec<String>,
    /// The pin each line of `pins` shows, by its pin table name (`FwPin::pin`).
    pub pin_names: Vec<&'static str>,
    /// The idle pins, inputs in their reset state that nothing drives, folded into one line naming
    /// each (`idle  PB0 PC0 ...`); empty when there are none. `pins` leaves them out.
    pub idle_pins: String,
    pub timers: Vec<String>,
    pub pending: Vec<String>,
    pub faults: Vec<String>,
    /// The host end of the image's network ports: the link, each connection, the traffic.
    pub net: Vec<String>,
    pub regs: Vec<String>,
}

impl StateText {
    /// The panel before the bench runs firmware.
    pub fn idle() -> StateText {
        StateText { head: "AT THE PLAYHEAD".into(), at: "Not running: Play runs the firmware".into(), ..StateText::default() }
    }
}

/// The panel's lines for `s`.
pub fn state_text(s: &FwState) -> StateText {
    let debug = s.debug.as_deref();
    let mut at = match s.at {
        At::Statement(id) => match debug.and_then(|d| d.stmts.get(id as usize).map(|st| (d, st))) {
            Some((d, st)) => match d.owner_name(st.owner) {
                Some(h) => format!("{}, line {}", h, st.line),
                None => format!("line {}", st.line),
            },
            None => format!("statement {id}"),
        },
        At::Runtime(routine) => format!("In the runtime: {routine}"),
        At::Sleeping => match s.wake {
            Some(w) => format!("Sleeping until {} at {}", w.source, format_time(s.t_reset + w.tick as f64 / s.f_clk)),
            None => "Sleeping".into(),
        },
        At::Halted(h) => {
            let what = match (h.kind, s.halt_detail.as_deref()) {
                (HaltKind::Unmodeled, Some(detail)) => detail,
                _ => h.what,
            };
            format!("Halted at 0x{:04x}: {}", h.pc, halt_text(h.kind, what))
        }
        At::Reset => "At reset".into(),
        At::Unmapped => "Running (the image has no statement table)".into(),
    };
    if s.stopped.is_some() {
        // A step stops before its statement: name the one about to execute, as the marker does.
        at = match debug.and_then(|d| about_to_run(d, s.pc).map(|st| (d, st))) {
            Some((d, st)) => match d.owner_name(st.owner) {
                Some(h) => format!("Stopped before {}, line {}", h, st.line),
                None => format!("Stopped before line {}", st.line),
            },
            None => format!("Stopped: {at}"),
        };
    }
    let vars = debug.map_or(&[][..], |d| &d.vars[..]);
    let mut rows = Vec::with_capacity(vars.len().min(s.vars.len()) + 2);
    for (var, value) in vars.iter().zip(&s.vars) {
        rows.push(var_row(var, value));
    }
    rows.push(StateRow { name: "PC".into(), value: format!("0x{:04X}", s.pc), ..StateRow::default() });
    rows.push(StateRow { name: "cycle".into(), value: s.cycle.to_string(), ..StateRow::default() });
    let idle = |p: &&FwPin| p.mode == PinUse::Input && p.level.is_none();
    let pins = s.pins.iter().filter(|p| !idle(p)).map(pin_text).collect();
    let pin_names = s.pins.iter().filter(|p| !idle(p)).map(|p| p.pin).collect();
    let idle_names: Vec<&str> = s.pins.iter().filter(idle).map(|p| p.pin).collect();
    let idle_pins = if idle_names.is_empty() { String::new() } else { format!("idle  {}", idle_names.join(" ")) };
    let timers = s.timers.iter().flat_map(|t| timer_text(t, s.cycle)).collect();
    let pending = s.pending.iter().map(|p| match *p {
        Pending::Irq { source, vector } => format!("{source} (vector {vector})"),
        Pending::Rx { link, bytes } => format!("link {link}: {bytes} bytes received"),
    }).collect();
    let degraded = s.radio_starts.degraded.map(|fault| {
        let cause = if fault.code == watch::FAULT_RADIO_START_FAILED { start_failure(fault.arg) }
            else { "a radio timer was lost or overwritten".into() };
        format!("radio stopped: {cause}; it restarts by itself")
    });
    let crypto = s.radio_illegal_pc.and_then(|pc| {
        // The stub is ENTRY, MOVI(row), ILL: only the ILL at entry + 6 identifies a hit.
        let map = &debug?.runtime;
        let index = map.binary_search_by_key(&pc.checked_sub(6)?, |r| r.0).ok()?;
        map[index].1.strip_prefix("wifi_crypto_fault_").map(|field| format!("Wi-Fi crypto {field} is not available yet"))
    });
    let faults = degraded.into_iter().chain(crypto).chain(radio_restart_line(&s.radio_starts)).chain(s.fault_counts.iter().map(|c| {
        let times = if watch::FAULT_KERNEL_DOUBLING.contains(&c.code) {
            // n reports of a doubling code stand for 2^(n-1) to 2^n - 1 failures
            match c.count {
                0 | 1 => String::new(),
                n if n < 64 => format!(" \u{d7}{}\u{2013}{}", 1u64 << (n - 1), (1u64 << n) - 1),
                _ => " \u{d7}2^63 or more".to_string(),
            }
        } else if c.count > 1 { format!(" \u{d7}{}", c.count) } else { String::new() };
        if c.site == watch::SITE_RADIO_SCHEDULER {
            format!("{} in the radio scheduler{times}, last at cycle {}", report_text(c.code, c.last.arg), c.last.cycle)
        } else if watch::FAULT_KERNEL_REPORT.contains(&c.code) {
            // Each failed start restarts the radio under its backoff (runtime.rs radio_restart).
            let then = if c.code == watch::FAULT_RADIO_START_FAILED { "; restarting" } else { "" };
            format!("{} at site {}{times}, last at cycle {}{then}", report_text(c.code, c.last.arg), c.site, c.last.cycle)
        } else if c.code == watch::FAULT_KERNEL_BLOCK_IN_CRITICAL {
            format!("{} at site {}{times}, last at cycle {} (export {})", fault_text(c.code), c.site, c.last.cycle, c.last.arg)
        } else {
            format!("{} at site {}{times}, last at cycle {} (arg {})", fault_text(c.code), c.site, c.last.cycle, c.last.arg)
        }
    })).collect();
    let mut regs = Vec::new();
    for row in s.regs.chunks(4) {
        let cells: Vec<String> = row.iter().map(|(name, v)| if *v <= 0xff { format!("{name} {v:02x}") } else { format!("{name} {v:04x}") }).collect();
        regs.push(cells.join("  "));
    }
    StateText {
        head: "AT THE PLAYHEAD".into(),
        rows,
        at,
        pins,
        pin_names,
        idle_pins,
        timers,
        pending,
        faults,
        net: s.net.as_ref().map(net_lines).unwrap_or_default(),
        regs,
    }
}

/// The network section: the link and each connection as the host end last told the device, then
/// the traffic, what the host refused, and whether the run went back over its real connections.
pub fn net_lines(n: &NetStatus) -> Vec<String> {
    fn why(code: u8, detail: u32) -> String {
        if code == net::code::NONE { String::new() }
        else if detail == 0 { format!(": {}", net::code_text(code)) }
        else { format!(": {} ({detail})", net::code_text(code)) }
    }
    let mut lines = vec![match n.link {
        None => "link  waiting for the image's Hello".to_string(),
        Some((net::LINK_UP, _, _)) => "link  up".to_string(),
        Some((net::LINK_JOINING, _, _)) => "link  joining".to_string(),
        Some((_, code, detail)) => format!("link  down{}", why(code, detail)),
    }];
    for c in &n.conns {
        let state = match c.state {
            net::READY => "ready".to_string(),
            net::CONNECTING => "connecting".to_string(),
            _ => format!("offline{}", why(c.code, c.detail)),
        };
        lines.push(format!("c{}  {state}", c.c));
    }
    lines.push(format!("up    {} messages, {} B of data", n.up, n.bytes_up));
    lines.push(format!("down  {} messages, {} B of data", n.down, n.bytes_down));
    if let Some(refusal) = &n.last_refusal {
        lines.push(format!("the host refused {} of the image's messages, the last as {refusal:?}", n.refused));
    }
    if n.cuts > 0 {
        let times = if n.cuts == 1 { "once".to_string() } else { format!("{} times", n.cuts) };
        lines.push(format!("the run went back and ran differently {times}: its real connections were reset"));
    }
    lines
}

/// The hosts the bench's servers name, once each, in their order.
pub fn network_hosts(n: &NetworkLive) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::with_capacity(n.servers.len());
    for server in &n.servers {
        if !hosts.contains(&server.host) {
            hosts.push(server.host.clone());
        }
    }
    hosts
}

/// The Network row's face: what the twin's connections reach now.
pub fn network_face(n: &NetworkLive) -> String {
    let hosts = network_hosts(n);
    if let Some(why) = &n.held {
        return format!("the bench's fakes: {why}");
    }
    if !n.on {
        return format!("the bench's fakes; the real network would reach {}", hosts.join(", "));
    }
    let allowed = |host: &String| n.servers.iter().any(|s| &s.host == host && s.approved);
    let (yes, no): (Vec<String>, Vec<String>) = hosts.into_iter().partition(allowed);
    let mut face = if yes.is_empty() { "on, but no host the bench names is allowed".to_string() } else { format!("real: {}", yes.join(", ")) };
    if !no.is_empty() {
        face += &format!("; {} not allowed, so its fake plays", no.join(", "));
    }
    match (&n.last_used, &n.since) {
        (Some(at), _) => face += &format!("; last used {at}"),
        (None, Some(since)) => face += &format!("; on since {since}"),
        (None, None) => {}
    }
    face
}

/// What a named endpoint's token is, in this computer's settings.
pub fn token_text(token: &TokenLive) -> String {
    match token {
        TokenLive::NoName => "no token (unnamed)".into(),
        TokenLive::Missing => "no token saved".into(),
        TokenLive::Saved => "token saved".into(),
        TokenLive::OtherHost(host) => format!("its token was saved for {host}, so it is not sent"),
        TokenLive::Unreadable(why) => format!("the tokens cannot be read: {why}"),
    }
}

/// One line per server the bench names: its connection, endpoint, address, whether its host is
/// allowed, and its token.
pub fn network_servers(n: &NetworkLive) -> Vec<String> {
    n.servers.iter().map(|s| {
        let name = s.name.as_deref().map_or(String::new(), |name| format!(" {name}"));
        let allowed = if s.approved { "allowed" } else { "not allowed" };
        format!("c{}{name}  {}  {allowed}, {}", s.c, s.address, token_text(&s.token))
    }).collect()
}

/// The line under `y` of `lines` equal lines that fill `height` from `top` (None outside them):
/// which pin of the state panel's pins the pointer is over.
pub fn line_at(top: f64, height: f64, lines: usize, y: f64) -> Option<usize> {
    if lines == 0 || !(height > 0.0) || !(y >= top) {
        return None;
    }
    let line = ((y - top) * lines as f64 / height) as usize;
    (line < lines).then_some(line)
}

/// The statement a stop at `pc` is about to execute: the innermost one that begins there or, on a
/// handler or function entry, that body's first in address order.
/// O(statements) on an entry, which only a stop asks.
fn about_to_run(d: &fab_elec_mcu_emu::FwDebug, pc: u32) -> Option<&fab_elec_mcu_emu::FwStmt> {
    if let Some(st) = d.starting_at(pc).last().and_then(|&id| d.stmts.get(id as usize)) {
        return Some(st);
    }
    let map = d.map.as_ref()?;
    let owner = if let Ok(k) = map.entries.binary_search_by_key(&pc, |e| e.0) {
        fab_elec_mcu_emu::Owner::Handler(map.entries[k].1)
    } else {
        let k = map.funcs.binary_search_by_key(&pc, |f| f.0).ok()?;
        fab_elec_mcu_emu::Owner::Func(map.funcs[k].2)
    };
    d.stmts.iter().filter(|st| st.owner == owner).filter_map(|st| Some((st.addr?, st))).min_by_key(|e| e.0).map(|e| e.1)
}

/// The marker on the source while the run is paused or stopped: a 0-based line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The statement about to execute (a step's stop) or executing.
    Solid(usize),
    /// The last statement that ran: the device sleeps, or a stop names no statement.
    Hollow(usize),
}

impl Mark {
    pub fn line(self) -> usize {
        match self {
            Mark::Solid(line) | Mark::Hollow(line) => line,
        }
    }
}

/// Where the paused device stands in the source, if the image has a table that places it.
pub fn marker(s: &FwState) -> Option<Mark> {
    let d = s.debug.as_deref()?;
    let line = |id: u32| d.stmts.get(id as usize).map(|st| st.line.saturating_sub(1) as usize);
    let last = s.last.and_then(line);
    if s.stopped.is_some() {
        return match about_to_run(d, s.pc) {
            Some(st) => Some(Mark::Solid(st.line.saturating_sub(1) as usize)),
            None => last.map(Mark::Hollow),
        };
    }
    match s.at {
        At::Statement(id) => line(id).map(Mark::Solid),
        // In the runtime on a statement's behalf, or halted in it.
        At::Runtime(_) | At::Halted(_) => last.map(Mark::Solid),
        At::Sleeping => last.map(Mark::Hollow),
        At::Reset | At::Unmapped => None,
    }
}

/// The 0-based lines of the statements that began since `seen`, the publication the reader read
/// before `s`, ascending, each once: each statement whose count in `FwState::ran_counts` grew, so
/// a statement that ran only in a publication replaced unread still counts. Without a `seen` to
/// compare with (none, another image, or another device: a count went down), the statements `s`
/// itself names (`FwState::ran`). O(statements that have run).
pub fn ran_lines(s: &FwState, seen: Option<&FwState>, out: &mut Vec<usize>) {
    out.clear();
    let Some(d) = s.debug.as_deref() else { return };
    let line = |id: u32| d.stmts.get(id as usize).map(|st| st.line.saturating_sub(1) as usize);
    let same_image = |p: &&FwState| (p.image_id, p.source_rev) == (s.image_id, s.source_rev);
    if !seen.filter(same_image).is_some_and(|p| grown(&p.ran_counts, &s.ran_counts, &line, out)) {
        out.clear();
        out.extend(s.ran.iter().filter_map(|&(id, _)| line(id)));
    }
    out.sort_unstable();
    out.dedup();
}

/// The lines of the statements counted more in `now` than in `before` (both ascending by
/// statement) into `out`; false when `now` cannot follow `before` on one device (a count went down
/// or a statement counted before is gone).
fn grown(before: &[(u32, u64)], now: &[(u32, u64)], line: &impl Fn(u32) -> Option<usize>, out: &mut Vec<usize>) -> bool {
    let mut before = before.iter().peekable();
    for &(id, n) in now {
        let mut had = 0;
        while let Some(&&(b, count)) = before.peek() {
            if b > id {
                break;
            }
            if b < id {
                return false;
            }
            had = count;
            before.next();
        }
        if n < had {
            return false;
        }
        if n > had {
            out.extend(line(id));
        }
    }
    before.next().is_none()
}

/// A text's line starts: where a byte of it is, by line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceLines {
    starts: Vec<usize>,
    len: usize,
}

impl SourceLines {
    pub fn new(text: &str) -> SourceLines {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(k, _)| k + 1)).collect();
        SourceLines { starts, len: text.len() }
    }

    /// The 0-based line holding `byte`, and the byte's offset in it.
    pub fn at(&self, byte: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|&s| s <= byte).saturating_sub(1);
        (line, byte - self.starts[line])
    }

    /// The 0-based lines a byte span covers, one line for an empty span.
    pub fn lines(&self, span: (u32, u32)) -> Range<usize> {
        let first = self.at(span.0 as usize).0;
        let last = self.at((span.1 as usize).saturating_sub(1).max(span.0 as usize)).0;
        first..last + 1
    }

    /// A byte span within the text, as (0-based line, byte range in that line) pieces, one per
    /// line it covers; nothing for a span outside the text.
    fn pieces(&self, span: (u32, u32), mut piece: impl FnMut(usize, Range<usize>)) {
        let (start, end) = (span.0 as usize, span.1 as usize);
        if start >= end || end > self.len {
            return;
        }
        let ((first, from), (last, to)) = (self.at(start), self.at(end));
        for line in first..=last {
            let a = if line == first { from } else { 0 };
            let b = if line == last { to } else { self.starts.get(line + 1).map_or(self.len, |n| n - 1) - self.starts[line] };
            if b > a {
                piece(line, a..b);
            }
        }
    }
}

/// A source condition on one line, in the colour of its newest value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CondMark {
    pub line: usize,
    /// The condition's bytes in that line.
    pub bytes: Range<usize>,
    pub value: bool,
}

/// The conditions the device has branched on, each in its newest value, by line: of the copies of
/// one condition (inlined code has one per call) the one that branched most since `seen`, the
/// publication the reader read before `s`, else the one that branched most. O(conditions).
pub fn cond_marks(s: &FwState, seen: Option<&FwState>, lines: &SourceLines, out: &mut Vec<CondMark>) {
    out.clear();
    let Some(d) = s.debug.as_deref() else { return };
    let before: &[FwCondCount] = match seen {
        Some(p) if (p.image_id, p.source_rev) == (s.image_id, s.source_rev) => &p.cond_counts,
        _ => &[],
    };
    // (span, (branches since `seen`, branches), newest value)
    let mut spans: Vec<((u32, u32), (u64, u64), bool)> = s.cond_counts.iter().filter_map(|c| {
        let cond = d.conds.get(c.cond as usize)?;
        let total = c.trues + c.falses;
        let had = before.binary_search_by_key(&c.cond, |b| b.cond).map_or(0, |k| before[k].trues + before[k].falses);
        Some((cond.span, (total.saturating_sub(had), total), c.last))
    }).collect();
    spans.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    spans.dedup_by_key(|c| c.0);
    for (span, _, value) in spans {
        lines.pieces(span, |line, bytes| out.push(CondMark { line, bytes, value }));
    }
}

/// The 0-based lines of statements that never ran on the device (`FwState::ran_counts`), as
/// ascending, apart ranges: each line a statement with code that never began covers, unless a
/// statement that did begins on it (a copy of inlined code that ran, or the code before a block
/// that never ran). O(statements).
pub fn unrun_lines(s: &FwState, lines: &SourceLines, out: &mut Vec<Range<usize>>) {
    out.clear();
    let Some(d) = s.debug.as_deref() else { return };
    let ran = |id: usize| s.ran_counts.binary_search_by_key(&(id as u32), |r| r.0).is_ok_and(|k| s.ran_counts[k].1 > 0);
    let (mut dim, mut lit): (Vec<Range<usize>>, Vec<usize>) = (Vec::new(), Vec::new());
    for (id, st) in d.stmts.iter().enumerate() {
        if st.addr.is_none() || st.span.0 >= st.span.1 {
            continue;
        }
        if ran(id) {
            lit.push(lines.at(st.span.0 as usize).0);
        } else {
            dim.push(lines.lines(st.span));
        }
    }
    lit.sort_unstable();
    let mut dimmed: Vec<usize> = dim.into_iter().flatten().filter(|line| lit.binary_search(line).is_err()).collect();
    dimmed.sort_unstable();
    dimmed.dedup();
    for line in dimmed {
        match out.last_mut() {
            Some(last) if last.end == line => last.end += 1,
            _ => out.push(line..line + 1),
        }
    }
}

/// How long a pulse of heat takes to fade, in wall seconds.
pub const HEAT_FADE: f64 = 0.6;
/// A line's wash right after its statement ran.
pub const HEAT_PEAK: f32 = 0.55;
/// The solid marker's wash.
pub const MARK_WASH: f32 = 0.75;
/// A condition's wash in its value's colour.
pub const COND_WASH: f32 = 0.35;

/// The source's heat while the run plays: per line, the wall time its statement last ran.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Heat {
    /// (0-based line, wall seconds of its last pulse), ascending by line.
    lines: Vec<(usize, f64)>,
}

impl Heat {
    /// The lines `ran` pulse to full heat at `now`.
    pub fn pulse(&mut self, ran: &[usize], now: f64) {
        for &line in ran {
            match self.lines.binary_search_by_key(&line, |e| e.0) {
                Ok(k) => self.lines[k].1 = now,
                Err(k) => self.lines.insert(k, (line, now)),
            }
        }
    }

    /// The washes at `now` (line range, opacity), each fading linearly from [`HEAT_PEAK`] to
    /// nothing over [`HEAT_FADE`]; lines gone cold are dropped.
    pub fn washes(&mut self, now: f64) -> Vec<(Range<usize>, f32)> {
        self.levels(now).into_iter().map(|(line, level)| (line..line + 1, HEAT_PEAK * level)).collect()
    }

    /// Each hot line's heat at `now` (line, 1 at its pulse fading linearly to nothing over
    /// [`HEAT_FADE`]), ascending by line; lines gone cold are dropped.
    pub fn levels(&mut self, now: f64) -> Vec<(usize, f32)> {
        self.lines.retain(|e| now - e.1 < HEAT_FADE);
        self.lines.iter().map(|&(line, at)| (line, 1.0 - ((now - at) / HEAT_FADE).max(0.0) as f32)).collect()
    }

    pub fn is_hot(&self) -> bool {
        !self.lines.is_empty()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }
}

fn value_text(v: &FwValue) -> String {
    match v {
        FwValue::I32(i) => i.to_string(),
        FwValue::F32(f) => f.to_string(),
        FwValue::Bool(b) => b.to_string(),
        FwValue::Words(w) => w.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "),
        FwValue::Unread => "?".into(),
    }
}

/// The state panel's width as one column, and as two (of [`STATE_COLUMN`] each).
pub const STATE_PANEL: f64 = 280.0;
pub const STATE_PANEL_TWO: f64 = 420.0;
pub const STATE_COLUMN: f64 = 191.0;
/// The source keeps at least this width beside a two-column panel.
pub const SOURCE_MIN: f64 = 460.0;

/// Whether the state panel lays its sections out in two columns (where the device is and its
/// variables; then pins and the rest): when the pane is wide enough for both beside the source, and
/// one column is taller than the panel. `two` is the current layout, `pane` the pane's width,
/// `visible` the panel's height and `need` its content's height as one column (the columns' heights
/// summed). Two columns go back to one only once the content fits with room to spare, so a panel
/// at the edge does not flip from frame to frame. A panel that was not drawn (folded away, or not
/// yet) keeps its layout.
pub fn state_columns(two: bool, pane: f64, visible: f64, need: f64) -> bool {
    if pane < SOURCE_MIN + STATE_PANEL_TWO {
        return false;
    }
    if !(visible > 0.0) {
        return two;
    }
    if two { need + 24.0 > visible } else { need > visible }
}

fn pin_text(p: &FwPin) -> String {
    let level = match p.level {
        Some(true) => "high",
        Some(false) => "low",
        None => "?",
    };
    match p.mode {
        // the duty is the share of time the pin is high; the level is the pin at the state's instant
        PinUse::Pwm => format!("{}  PWM {:.0} %, {level}", p.pin, p.duty * 100.0),
        PinUse::Input => format!("{}  in, {level}", p.pin),
        PinUse::PullUp => format!("{}  in, pull-up, {level}", p.pin),
        PinUse::Output => format!("{}  out, {level}", p.pin),
        PinUse::Peripheral => format!("{}  peripheral", p.pin),
    }
}

/// A timer in two or three lines: its mode and clock, its count and compares, its next event.
fn timer_text(t: &FwTimer, cycle: u64) -> Vec<String> {
    let clock = if t.prescale == 0 { "stopped".to_string() } else { format!("clk/{}", t.prescale) };
    let compares = t.compares.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ");
    let mut lines = vec![
        format!("{}  {}, {clock}", t.name, t.mode),
        if compares.is_empty() { format!("  count {} of {}", t.count, t.top) } else { format!("  count {} of {}, compare {compares}", t.count, t.top) },
    ];
    if let Some((event, tick)) = t.next {
        let what = match event {
            TimerEvent::Compare(ch) => format!("compare {}", (b'A' + ch) as char),
            TimerEvent::Overflow => "overflow".into(),
        };
        lines.push(format!("  next: {what} in {} cycles", tick.saturating_sub(cycle)));
    }
    lines
}

fn halt_text(kind: HaltKind, what: &str) -> String {
    match kind {
        HaltKind::Break => format!("a break ({what})"),
        HaltKind::BadInsn => format!("an instruction it cannot run ({what})"),
        HaltKind::BadAddress { addr } => format!("an access outside memory at 0x{addr:x} ({what})"),
        HaltKind::Overrun => format!("an overrun ({what})"),
        HaltKind::Watchdog => format!("the watchdog ({what})"),
        HaltKind::Unmodeled => format!("not modelled: {what}"),
    }
}

fn radio_restart_line(starts: &RadioStarts) -> Option<String> {
    let (start, _) = starts.latest?;
    let times = match starts.markers {
        0 | 1 => return None,
        2 => "once".to_string(),
        n => format!("{} times", n - 1),
    };
    let cause = match start.cause {
        watch::RADIO_START_CAUSE_REQUEST => "request".to_string(),
        watch::RADIO_START_CAUSE_FAULT => "radio fault".to_string(),
        watch::FAULT_RADIO_START_FAILED => "Wi-Fi start failed".to_string(),
        watch::FAULT_RADIO_PHY_INIT_FAILED => "PHY start failed".to_string(),
        watch::FAULT_KERNEL_TIMER_LOST => "timer lost".to_string(),
        watch::FAULT_KERNEL_TIMER_HANDLE_INVALID => "timer overwritten".to_string(),
        code if watch::FAULT_KERNEL_REPORT.contains(&code) => format!("report {code}"),
        code => format!("cause {code}"),
    };
    let mut line = format!("radio restarted {times}, last cause: {cause}");
    // The device saturates the count at 255.
    match start.consecutive {
        0 | 1 => {}
        u8::MAX => line.push_str(", 255 or more in a row"),
        n => { use std::fmt::Write; write!(line, ", {n} in a row").unwrap(); }
    }
    Some(line)
}

/// A failed Wi-Fi start's step and error (watch::FAULT_RADIO_START_FAILED's arg).
fn start_failure(arg: i32) -> String {
    let step = match (arg as u32) >> 24 { 1 => "init", 2 => "set mode", 3 => "start", _ => "unknown step" };
    format!("Wi-Fi start failed ({step} returned error 0x{:x})", arg as u32 & 0x00ff_ffff)
}

/// A kernel report in words, with its argument where its code puts it.
fn report_text(code: u8, arg: i32) -> String {
    match code {
        watch::FAULT_KERNEL_TIMER_REFUSED => format!("radio timer refused (no radio memory; it stays pending): {arg} live timers"),
        watch::FAULT_KERNEL_CRIT_MISMATCH => format!("radio critical section mismatch: return 0x{:08x}", arg as u32),
        watch::FAULT_KERNEL_CRIT_SPAN => format!("radio masked span: {arg} cycles"),
        watch::FAULT_KERNEL_ISR_DEPTH => format!("radio ISR returned at depth {arg}"),
        watch::FAULT_RADIO_START_FAILED => start_failure(arg),
        watch::FAULT_RADIO_PHY_INIT_FAILED => format!("radio PHY init failed: {arg}"),
        watch::FAULT_RADIO_PHY_CAL_STORE_FAILED => format!("radio PHY calibration store failed: {arg}"),
        watch::FAULT_KERNEL_IRQ_LINE_REFUSED => format!("radio interrupt line {arg} refused (runtime-owned, not level 1, or out of range)"),
        watch::FAULT_KERNEL_IRQ_SOURCE_REFUSED => format!("radio interrupt source {arg} refused"),
        watch::FAULT_KERNEL_ISR_TABLE_FULL => format!("no radio ISR slot left for line {arg}"),
        watch::FAULT_KERNEL_ISR_ENTRY_REFUSED => format!("radio ISR entry 0x{:08x} outside the island code region", arg as u32),
        watch::FAULT_RADIO_ISLAND_MISMATCH => format!("radio island missing or doesn't match this firmware (header CRC 0x{:08x}); the radio stays off", arg as u32),
        watch::FAULT_KERNEL_TIMER_LOST => format!("radio timer 0x{:08x} lost (no radio memory when armed; it won't fire)", arg as u32),
        watch::FAULT_KERNEL_TIMER_UNSET => format!("radio timer 0x{:08x} armed before it was set (ignored)", arg as u32),
        watch::FAULT_KERNEL_HEAP_EXHAUSTED => format!("radio memory refused {arg} bytes"),
        watch::FAULT_KERNEL_TIMER_HANDLE_INVALID => format!("radio timer 0x{:08x} overwritten by the radio's code (refused)", arg as u32),
        watch::FAULT_RADIO_PHY_TABLE_REFUSED => format!("radio PHY function table at 0x{:08x} is outside ROM data RAM; restarts won't restore it", arg as u32),
        // ESP-IDF v6.1.1 components/esp_wifi/include/esp_wifi_types_generic.h:1270-1277.
        watch::FAULT_RADIO_EVENT => match arg as u32 {
            1 => "scan done".into(),
            2 => "station started".into(),
            3 => "station stopped".into(),
            4 => "connected to the access point".into(),
            5 => "disconnected from the access point".into(),
            u32::MAX => "event from another source".into(),
            id => format!("Wi-Fi event {id}"),
        },
        _ => format!("kernel report {code}: {arg}"),
    }
}

fn fault_text(code: u8) -> &'static str {
    match code {
        watch::FAULT_PIN => "pin fault",
        watch::FAULT_TIMER => "timer fault",
        watch::FAULT_OVERRUN => "overrun",
        watch::FAULT_LOOP_CAP => "loop cap reached",
        watch::FAULT_SERIAL => "serial fault",
        watch::FAULT_SERIAL_LOST => "serial frames lost",
        watch::FAULT_SERIAL_LOST_AT_LEAST => "serial frames lost (at least)",
        watch::FAULT_WATCHDOG => "watchdog reset",
        watch::FAULT_KERNEL_BLOCK_IN_CRITICAL => "radio export blocked in a critical section",
        watch::FAULT_DRAW => "draw refused",
        watch::FAULT_PANEL => "panel bring-up failed",
        watch::FAULT_RANGE => "argument out of range",
        watch::FAULT_ETH_LENGTH => "Ethernet length out of range",
        watch::FAULT_ETH_RANGE => "Ethernet buffer range invalid",
        watch::FAULT_SHIFT_BITS => "shift_out bit count out of range",
        watch::FAULT_I2C_NACK => "I2C device did not answer",
        watch::FAULT_I2C_TIMEOUT => "I2C transfer timed out",
        watch::FAULT_EEPROM_ADDR => "EEPROM address out of range",
        watch::FAULT_NET_HANDLE => "no such connection",
        watch::FAULT_NET_RANGE => "network buffer range invalid",
        watch::FAULT_AUDIO_UNAVAILABLE => "no audio device",
        watch::FAULT_SPEAKER_LOST => "speaker ran dry",
        watch::FAULT_SPEAKER_STALL => "speaker stalled",
        watch::FAULT_MIC_LOST => "mic lost samples",
        watch::FAULT_MIC_STALL => "mic stalled",
        _ => "fault",
    }
}

/// The separator between a declaration and its value at the playhead.
pub const INLAY_LEAD: &str = "  \u{27F5} ";

/// Each program variable's value at `s` as ghost text after its declaration's first line:
/// (0-based line, text), ascending by line, one per line (the first variable on a line wins).
pub fn value_inlays(s: &FwState, lines: &SourceLines, out: &mut Vec<(usize, String)>) {
    out.clear();
    let Some(debug) = &s.debug else { return };
    // Lowering records the program's variables in declaration order.
    for (var, value) in debug.vars.iter().zip(&s.vars) {
        let (start, end) = var.span;
        if start >= end || end as usize > lines.len {
            continue;
        }
        let line = lines.at(start as usize).0;
        if out.last().is_some_and(|entry| entry.0 == line) {
            continue;
        }
        let text = match value {
            FwValue::Words(words) => format!("{INLAY_LEAD}{}", block_size(var, words)),
            FwValue::Unread => format!("{INLAY_LEAD}?"),
            _ => {
                let mut text = value_text(value);
                text.insert_str(0, INLAY_LEAD);
                text
            }
        };
        out.push((line, text));
    }
}
