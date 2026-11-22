//! The Scope's protocol decoders (Fable step 3): each decodes some of the record's logic lanes
//! with a `fab_elec_bench::decode` decoder into annotation rows drawn under its lanes.
//!
//! A decoder runs on the Scope's decode worker, one long-lived thread. The UI hands it the newest
//! record and the decoders as set up (O(1), a slot swap and a wake that never blocks); the worker
//! keeps a cursor per line into the record's logic traces (they keep their whole history, so an
//! entry's index never moves), feeds each decoder what is new in time order, and publishes the
//! rows as an immutable snapshot ([`Decoded`]) the UI only clones. A new record generation, a
//! rewind of the record (the run went back to a snapshot), or a changed decoder, starts that
//! decoder over; a long record decodes in slices, published as they
//! go. A spilled chunk of a line (`fab_elec_sim::spill`) is a gap to the decoder: reset and marked.
//!
//! Rows keep their annotations in sealed chunks of [`ANN_CHUNK`] with a summary each, so a frame
//! reads the window's chunks, draws a chunk too dense for its pixels from its summary
//! ([`Piece::Dense`]) and walks only the boxes it shows: O(chunks and boxes in the window).
//!
//! Options left to the record (`None`) are found from it once: a UART's baud from its first
//! edges, an SPI word's bits from the clocks of its first whole chip-select transfer.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use fab_elec_bench::decode::{flags, i2c, spi, uart, Ann, Decoder, DecoderSpec, Input};
use fab_elec_bench::edges::Threshold;
use fab_elec_bench::uart::{estimate_baud, Parity, UartFormat};
use fab_elec_sim::live::Latest;
use fab_elec_sim::trace::{LogicEdge, LogicTrace, SignalId, TraceStore, LOGIC_CHUNK};
use makepad_widgets_core::*;

/// Annotations a sealed row chunk holds.
pub const ANN_CHUNK: usize = 1024;
/// Entries a decode slice feeds before it publishes and looks for newer work.
const SLICE: usize = 1 << 16;
/// Edges a UART's baud is measured from; with fewer, at least [`BAUD_EARLY`] that give a standard
/// rate (fewer could take two bits for one).
const BAUD_EDGES: usize = 64;
const BAUD_EARLY: usize = 24;
/// Chip-select transfers looked at for an SPI word's bits before taking eight.
const BITS_TRANSFERS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocol {
    /// Eight bits, no parity, one stop bit; `baud` None: measured.
    Uart { baud: Option<f64> },
    I2c,
    /// `mode` 0 to 3 (CPOL is bit 1, CPHA bit 0), chip select active low; `bits` None: found.
    Spi { bits: Option<u8>, mode: u8, msb_first: bool },
}

/// The options a decoder found in the record.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Found {
    pub baud: f64,
    pub bits: u8,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [
        Protocol::Uart { baud: None },
        Protocol::I2c,
        Protocol::Spi { bits: None, mode: 0, msb_first: true },
    ];

    pub fn spec(&self) -> &'static DecoderSpec {
        match self {
            Protocol::Uart { .. } => &uart::SPEC,
            Protocol::I2c => &i2c::SPEC,
            Protocol::Spi { .. } => &spi::SPEC,
        }
    }

    pub fn name(&self) -> &'static str {
        self.spec().name
    }

    fn uart_format(baud: f64) -> UartFormat {
        UartFormat { baud, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } }
    }

    fn spi_format(bits: u8, mode: u8, msb_first: bool) -> spi::SpiFormat {
        spi::SpiFormat { cpol: mode & 2 != 0, cpha: mode & 1 != 0, bits, msb_first, cs_active_low: true }
    }

    /// The decoder with what was found filled in.
    pub fn decoder(&self, found: Found) -> Box<dyn Decoder> {
        match *self {
            Protocol::Uart { baud } => Box::new(uart::Uart::new(Self::uart_format(baud.unwrap_or(found.baud)))),
            Protocol::I2c => Box::new(i2c::I2c::new()),
            Protocol::Spi { bits, mode, msb_first } => Box::new(spi::Spi::new(Self::spi_format(bits.unwrap_or(found.bits), mode, msb_first))),
        }
    }

    /// The rows a decoder of these lines fills: SPI's MISO row and transfers only with MISO and
    /// chip select.
    pub fn rows(&self, lines: &[Option<SignalId>]) -> Vec<u8> {
        let has = |role: u8| lines.get(role as usize).is_some_and(Option::is_some);
        match self {
            Protocol::Spi { .. } => [(spi::MOSI_ROW, true), (spi::MISO_ROW, has(spi::MISO)), (spi::TRANSFERS, has(spi::CS))]
                .into_iter().filter_map(|(row, on)| on.then_some(row)).collect(),
            _ => (0..self.spec().rows.len() as u8).collect(),
        }
    }

    /// The decoder's title with its options: `UART 115200`, `SPI 24 bit mode 0`.
    pub fn title(&self, found: Option<Found>) -> String {
        match *self {
            Protocol::Uart { baud } => match (baud, found) {
                (Some(baud), _) => format!("UART {baud}"),
                (None, Some(found)) => format!("UART {} (auto)", found.baud),
                (None, None) => "UART (measuring baud)".into(),
            },
            Protocol::I2c => "I2C".into(),
            Protocol::Spi { bits, mode, msb_first } => {
                let bits = match (bits, found) {
                    (Some(bits), _) => format!("{bits} bit"),
                    (None, Some(found)) => format!("{} bit (auto)", found.bits),
                    (None, None) => "(counting bits)".into(),
                };
                format!("SPI {bits} mode {mode}{}", if msb_first { "" } else { " LSB first" })
            }
        }
    }
}

/// One decoder as the Scope set it up: its lines by role, the record's logic signals (None for an
/// absent optional role, or a lane with no logic trace).
#[derive(Clone, Debug, PartialEq)]
pub struct Setup {
    pub id: u64,
    pub protocol: Protocol,
    pub lines: Vec<Option<SignalId>>,
}

/// A row chunk's summary: its first box's start, the latest end, its boxes, and its errors with the
/// first and last error's start.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sum {
    pub t0: f64,
    pub t1: f64,
    pub count: u32,
    pub errors: u32,
    pub err0: f64,
    pub err1: f64,
}

impl Sum {
    fn of(anns: &[Ann]) -> Sum {
        let mut sum = Sum { t0: anns.first().map_or(0.0, |a| a.t0), t1: f64::NEG_INFINITY, count: anns.len() as u32, ..Sum::default() };
        for ann in anns {
            sum.t1 = sum.t1.max(ann.t1);
            if ann.flags & flags::ERROR != 0 {
                if sum.errors == 0 {
                    sum.err0 = ann.t0;
                }
                (sum.errors, sum.err1) = (sum.errors + 1, ann.t0);
            }
        }
        sum
    }
}

/// One row as published: sealed chunks with their summaries, then the open tail.
#[derive(Clone, Debug, Default)]
pub struct Row {
    pub row: u8,
    pub sealed: Arc<Vec<Arc<[Ann]>>>,
    pub sums: Arc<Vec<Sum>>,
    pub tail: Arc<[Ann]>,
    pub tail_sum: Sum,
}

/// What a row shows of a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Piece {
    Box(Ann),
    /// A chunk whose boxes average under the size asked, drawn from its summary.
    Dense(Sum),
}

impl Row {
    pub fn len(&self) -> usize {
        self.sealed.len() * ANN_CHUNK + self.tail.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The pieces over `[t0, t1]` in time order: each chunk whose boxes average under `dense`
    /// seconds as one [`Piece::Dense`], the others' boxes one by one. O(log chunks + chunks and
    /// boxes in the window).
    pub fn pieces(&self, t0: f64, t1: f64, dense: f64, out: &mut Vec<Piece>) {
        let first = self.sums.partition_point(|sum| sum.t1 < t0);
        let chunks = self.sealed[first..].iter().zip(&self.sums[first..]).map(|(c, s)| (&c[..], s))
            .chain((!self.tail.is_empty()).then_some((&self.tail[..], &self.tail_sum)));
        for (chunk, sum) in chunks {
            if sum.t0 > t1 {
                break;
            }
            if sum.t1 < t0 {
                continue;
            }
            if sum.t1 - sum.t0 < dense * f64::from(sum.count) {
                out.push(Piece::Dense(*sum));
                continue;
            }
            let from = chunk.partition_point(|ann| ann.t1 < t0);
            for ann in &chunk[from..] {
                if ann.t0 > t1 {
                    break;
                }
                out.push(Piece::Box(*ann));
            }
        }
    }
}

/// One decoder's rows as published.
#[derive(Clone, Debug)]
pub struct DecodedOne {
    pub setup: Setup,
    pub generation: u64,
    pub found: Option<Found>,
    pub rows: Vec<Row>,
    /// Decoded up to here, seconds.
    pub head: f64,
    /// Why it decodes nothing yet, when it does not.
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Decoded {
    pub decoders: Vec<DecodedOne>,
}

impl Decoded {
    pub fn get(&self, id: u64) -> Option<&DecodedOne> {
        self.decoders.iter().find(|one| one.setup.id == id)
    }
}

#[derive(Default)]
struct RowBuild {
    row: u8,
    sealed: Arc<Vec<Arc<[Ann]>>>,
    sums: Arc<Vec<Sum>>,
    tail: Vec<Ann>,
}

impl RowBuild {
    fn push(&mut self, ann: Ann) {
        self.tail.push(ann);
        if self.tail.len() == ANN_CHUNK {
            let chunk: Arc<[Ann]> = std::mem::take(&mut self.tail).into();
            Arc::make_mut(&mut self.sums).push(Sum::of(&chunk));
            Arc::make_mut(&mut self.sealed).push(chunk);
        }
    }

    fn row(&self) -> Row {
        Row { row: self.row, sealed: self.sealed.clone(), sums: self.sums.clone(), tail: self.tail.as_slice().into(), tail_sum: Sum::of(&self.tail) }
    }
}

struct Running {
    setup: Setup,
    generation: u64,
    /// The record's rewinds when it started (`TraceStore::rewinds`).
    rewinds: u64,
    found: Option<Found>,
    decoder: Option<Box<dyn Decoder>>,
    /// Per line, its next entry.
    cursors: Vec<usize>,
    rows: Vec<RowBuild>,
    head: f64,
    note: Option<String>,
}

impl Running {
    fn new(setup: &Setup, store: &TraceStore) -> Running {
        let rows = (0..setup.protocol.spec().rows.len() as u8).map(|row| RowBuild { row, ..RowBuild::default() }).collect();
        Running { setup: setup.clone(), generation: store.generation, rewinds: store.rewinds, found: None, decoder: None, cursors: vec![0; setup.lines.len()], rows, head: f64::NEG_INFINITY, note: None }
    }

    fn push(rows: &mut [RowBuild], decoder: &mut dyn Decoder, input: Input) {
        decoder.push(input, &mut |ann| {
            if let Some(row) = rows.get_mut(ann.row as usize) {
                row.push(ann);
            }
        });
    }

    /// Feeds what the record has up to `head` that this decoder has not seen, at most `budget`
    /// entries. True when it changed anything; `budget` keeps what is left.
    fn advance(&mut self, store: &TraceStore, head: f64, budget: &mut usize) -> bool {
        let traces: Vec<Option<&LogicTrace>> = self.setup.lines.iter().map(|line| line.and_then(|id| store.logic(id))).collect();
        let spec = self.setup.protocol.spec();
        if let Some(missing) = spec.inputs.iter().zip(&traces).find(|(input, trace)| !input.optional && trace.is_none()) {
            let note = format!("no logic lane for {}: decoders read lanes an MCU pin drives", missing.0.role);
            let changed = self.note.as_ref() != Some(&note);
            self.note = Some(note);
            return changed;
        }
        if self.found.is_none() {
            match find(&self.setup.protocol, &traces, head) {
                Ok(found) => (self.found, self.note) = (Some(found), None),
                Err(note) => {
                    let changed = self.note.as_ref() != Some(&note);
                    self.note = Some(note);
                    return changed;
                }
            }
        }
        let protocol = self.setup.protocol;
        let found = self.found.unwrap_or_default();
        let decoder = self.decoder.get_or_insert_with(|| protocol.decoder(found));
        let mut changed = false;
        loop {
            if *budget == 0 {
                return changed;
            }
            // the earliest next entry at or before the head, ties in line order
            let mut next: Option<(f64, usize)> = None;
            for (k, trace) in traces.iter().enumerate() {
                let Some(trace) = trace else { continue };
                let i = self.cursors[k];
                let t = match (trace.away(i), trace.get(i)) {
                    (Some((first, _)), _) => first,
                    (None, Some(entry)) => entry.t,
                    (None, None) => continue,
                };
                if t <= head && next.is_none_or(|(best, _)| t < best) {
                    next = Some((t, k));
                }
            }
            let Some((_, k)) = next else { break };
            let trace = traces[k].expect("a line with an entry");
            changed = true;
            if let Some((from, mut to)) = trace.away(self.cursors[k]) {
                // a spilled chunk: no data over it on any line, nor over another's it runs into
                loop {
                    let mut grew = false;
                    for trace in traces.iter().flatten() {
                        if let Some((first, last)) = trace.away(trace.upto(to)) {
                            if first <= to && last > to {
                                (to, grew) = (last, true);
                            }
                        }
                    }
                    if !grew {
                        break;
                    }
                }
                if to > head {
                    // its end is not in yet
                    break;
                }
                Self::push(&mut self.rows, decoder.as_mut(), Input::Gap { from, to });
                for (j, line) in traces.iter().enumerate() {
                    let Some(line) = line else { continue };
                    self.cursors[j] = line.upto(to);
                    if let Some(high) = line.level_at(to) {
                        Self::push(&mut self.rows, decoder.as_mut(), Input::Edge { line: j as u8, t: to, high });
                    }
                }
                *budget = budget.saturating_sub(1);
                continue;
            }
            let entry = trace.get(self.cursors[k]).expect("an entry in memory");
            self.cursors[k] += 1;
            *budget -= 1;
            Self::push(&mut self.rows, decoder.as_mut(), Input::Edge { line: k as u8, t: entry.t, high: entry.high });
        }
        if head > self.head {
            Self::push(&mut self.rows, decoder.as_mut(), Input::End { t: head });
            self.head = head;
            changed = true;
        }
        changed
    }

    fn published(&self) -> DecodedOne {
        let shown = self.setup.protocol.rows(&self.setup.lines);
        DecodedOne {
            setup: self.setup.clone(),
            generation: self.generation,
            found: self.found,
            rows: self.rows.iter().filter(|row| shown.contains(&row.row)).map(RowBuild::row).collect(),
            head: self.head,
            note: self.note.clone(),
        }
    }
}

/// A line's edges in memory up to `head`, oldest first (its first entry is a level, no edge).
fn edges(trace: &LogicTrace, head: f64) -> impl Iterator<Item = LogicEdge> + '_ {
    (1..trace.len()).filter(|&i| trace.away(i).is_none()).filter_map(|i| trace.get(i)).take_while(move |e| e.t <= head)
}

/// The options left to the record, found from what it has up to `head`; Err says what it waits
/// for.
fn find(protocol: &Protocol, traces: &[Option<&LogicTrace>], head: f64) -> Result<Found, String> {
    match *protocol {
        Protocol::Uart { baud: Some(baud) } => Ok(Found { baud, bits: 8 }),
        Protocol::Uart { baud: None } => {
            let rx = traces[0].expect("checked");
            let edges: Vec<f64> = edges(rx, head).take(BAUD_EDGES).map(|e| e.t).collect();
            match estimate_baud(&edges) {
                Some(estimate) if edges.len() == BAUD_EDGES || (edges.len() >= BAUD_EARLY && estimate.nominal.is_some()) => {
                    Ok(Found { baud: estimate.nominal.unwrap_or(estimate.measured.round()), bits: 8 })
                }
                _ => Err(format!("measuring the baud: {} of {BAUD_EDGES} edges", edges.len())),
            }
        }
        Protocol::I2c => Ok(Found::default()),
        Protocol::Spi { bits: Some(bits), .. } => Ok(Found { baud: 0.0, bits }),
        Protocol::Spi { bits: None, mode, .. } => {
            let (Some(sclk), Some(Some(cs))) = (traces[spi::SCLK as usize], traces.get(spi::CS as usize)) else {
                return Ok(Found { baud: 0.0, bits: 8 });
            };
            // a transfer: the chip select low from one entry to the next; its sampling clock edges
            let sampling = mode == 0 || mode == 3;
            let mut low_from = cs.get(0).filter(|e| !e.high && cs.away(0).is_none()).map(|e| e.t);
            let mut transfers = 0;
            for e in edges(cs, head) {
                match (e.high, low_from) {
                    (false, _) => low_from = Some(e.t),
                    (true, Some(from)) => {
                        let (i0, i1) = (sclk.upto(from).max(1), sclk.upto(e.t));
                        // a transfer over a spilled clock chunk is not looked at (a time inside one
                        // counts up to its start)
                        let chunks = sclk.upto(from) / LOGIC_CHUNK..=i1 / LOGIC_CHUNK;
                        if chunks.into_iter().any(|c| sclk.away(c * LOGIC_CHUNK).is_some_and(|(first, last)| first <= e.t && last >= from)) {
                            low_from = None;
                            continue;
                        }
                        let count = (i0..i1).filter(|&i| sclk.away(i).is_none())
                            .filter(|&i| sclk.get(i).zip(sclk.get(i - 1)).is_some_and(|(at, before)| at.high == sampling && before.high != at.high))
                            .take(65).count();
                        if (1..=64).contains(&count) {
                            return Ok(Found { baud: 0.0, bits: count as u8 });
                        }
                        transfers += 1;
                        if transfers == BITS_TRANSFERS {
                            return Ok(Found { baud: 0.0, bits: 8 });
                        }
                        low_from = None;
                    }
                    (true, None) => {}
                }
            }
            Err("counting a transfer's bits: waiting for the chip select".into())
        }
    }
}

/// Every decoder of a Scope, brought up to the record a round at a time.
#[derive(Default)]
pub struct Decoders {
    running: Vec<Running>,
}

impl Decoders {
    /// Brings each of `setups` up to `store`, feeding at most `budget` entries in all: a new or
    /// changed setup, or a new record generation or rewind, starts over; a setup no longer asked for goes.
    /// Returns (changed, more to do). O(entries fed x lines).
    pub fn advance(&mut self, store: &TraceStore, setups: &[Setup], mut budget: usize) -> (bool, bool) {
        let mut changed = self.running.len() != setups.len();
        let mut old = std::mem::take(&mut self.running);
        for setup in setups {
            let kept = old.iter().position(|r| r.setup == *setup && r.generation == store.generation && r.rewinds == store.rewinds).map(|at| old.swap_remove(at));
            changed |= kept.is_none();
            self.running.push(kept.unwrap_or_else(|| Running::new(setup, store)));
        }
        let head = if store.is_empty() { f64::NEG_INFINITY } else { store.time.at(store.len() - 1) };
        let mut more = false;
        for running in &mut self.running {
            changed |= running.advance(store, head, &mut budget);
            more |= budget == 0;
        }
        (changed, more)
    }

    pub fn published(&self) -> Decoded {
        Decoded { decoders: self.running.iter().map(Running::published).collect() }
    }
}

type Job = (Arc<TraceStore>, Arc<[Setup]>);

/// The Scope's decode worker: its inbox, its wake and its output.
pub struct DecodeWorker {
    inbox: Arc<Mutex<Option<Job>>>,
    wake: SyncSender<()>,
    pub out: Arc<Latest<Decoded>>,
}

impl DecodeWorker {
    /// Starts the worker thread; it ends when this is dropped.
    pub fn start(cx: &mut Cx) -> Option<DecodeWorker> {
        let inbox: Arc<Mutex<Option<Job>>> = Arc::default();
        let out: Arc<Latest<Decoded>> = Arc::default();
        let (wake, woken) = mpsc::sync_channel(1);
        let (job, published) = (inbox.clone(), out.clone());
        let spawned = cx.thread_spawner().spawn_worker(
            ThreadOptions { name: Some("elec scope decoders".into()), ..Default::default() },
            Box::new(move || run(job, woken, published)),
        );
        match spawned {
            Ok(handle) => {
                handle.detach();
                Some(DecodeWorker { inbox, wake, out })
            }
            Err(e) => {
                log!("elec scope: no thread for the decoders ({e:?}); decoder rows stay empty");
                None
            }
        }
    }

    /// Hands the worker the newest record and the decoders: a slot swap and a wake that never
    /// waits (a wake already pending covers this one). O(1).
    pub fn ask(&self, store: Arc<TraceStore>, setups: Arc<[Setup]>) {
        let old = self.inbox.lock().unwrap_or_else(|e| e.into_inner()).replace((store, setups));
        let _ = self.wake.try_send(());
        drop(old);
    }
}

fn run(inbox: Arc<Mutex<Option<Job>>>, woken: Receiver<()>, out: Arc<Latest<Decoded>>) {
    let mut decoders = Decoders::default();
    let mut job: Option<Job> = None;
    let mut more = false;
    loop {
        if !more && woken.recv().is_err() {
            return;
        }
        if let Some(newer) = inbox.lock().unwrap_or_else(|e| e.into_inner()).take() {
            job = Some(newer);
        }
        let Some((store, setups)) = &job else {
            more = false;
            continue;
        };
        let changed;
        (changed, more) = decoders.advance(store, setups, SLICE);
        if changed {
            out.publish(Arc::new(decoders.published()));
            SignalToUI::set_ui_signal();
        }
    }
}
