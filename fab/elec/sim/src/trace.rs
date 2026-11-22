//! Trace data (E3 4.4, contract C10): what the solver records and the instruments read.
//!
//! Time is the solver's own non-uniform steps, shared by every signal of a store. Samples are
//! kept in chunks of `CHUNK` points; a closed chunk carries a min/max pyramid over sample index
//! (levels by 4) so a view draws any zoom from a few bins per pixel column. The producer (the live
//! worker, or a run's sink) appends; consumers receive `TraceDelta`s and apply them to their own
//! store, or take the whole store a `TracePublisher` keeps on the producer's thread. Chunks travel
//! as `Arc`, so a delta copies no closed samples.
//!
//! Logic traces (`TraceStore::logic`, a logic analyser's record): the levels an input reads on a
//! net, as the times it changed. The producer finds them (`livesim`'s logic taps, at each pad's
//! own input thresholds), so a consumer only appends. They keep the whole run. The newest
//! [`LOGIC_KEEP`] entries of each stay in memory. Older chunks spill to the producer's file
//! (`crate::spill`), keeping their summaries, and load back when a view zooms into them. Sealed
//! chunks travel as `Arc` like the samples' and are shared; the producer's spills and loads travel
//! in its deltas ([`LogicPage`]).
//!
//! A record keeps its whole signals across the span in [`Keep`]. Other signals keep a live front.
//! Their older closed positions share one [`absent_chunk`], whose samples read as NaN. Past the
//! closed-position byte budget, [`TraceStore::fold`] reduces pairs to half as many points, keeping
//! their time span and each present signal's extremes. Consumers thin by the same rule and take
//! the producer's folded chunks from its deltas ([`Fold`]).
//!
//! A live run that goes back to a snapshot (an edit behind its front) sends a [`Rewind`] at the
//! start of a delta: every store drops what came after the run's time then, opening the chunk the
//! cut falls in again, and the delta goes on from there.

use crate::live::Latest;
use std::collections::VecDeque;
use std::mem::{size_of, size_of_val};
use std::sync::{Arc, OnceLock};

/// Samples per chunk.
pub const CHUNK: usize = 4096;

use crate::budget::RECORD_FINE_BYTES;

/// Edges a logic trace seals into one shared chunk.
pub const LOGIC_CHUNK: usize = 4096;

/// The entries a logic trace keeps in memory: past them its oldest whole chunks spill to the
/// producer's file (`TraceStore::spill_logic`), their summaries kept, and come back while a view
/// shows them in detail. 2^20 entries (16 MB a signal): over a minute of a 7.8 kHz PWM, hours of
/// slower logic. A bound on memory that degrades to a file read on zoom, never a shorter history.
pub const LOGIC_KEEP: usize = 1 << 20;

/// A saved signal, by its index in the generation's signal list (the lowering names them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinMax {
    pub min: f32,
    pub max: f32,
}

/// `CHUNK` closed samples of one signal: display values, the exact copy when the signal is
/// measured, and the pyramid: level k holds `CHUNK / 4^(k+1)` bins, levels concatenated. The
/// pyramid is built once: as the chunk is made, or, for a store that leaves it to a background job
/// (`TraceStore::defer_pyramids`), there. Until it is home `level` has none and a reader reads `y`,
/// which is exact; every holder of the shared chunk sees it once it is built.
#[derive(Debug)]
pub struct Chunk {
    pub y: Box<[f32]>,
    pub exact: Option<Box<[f64]>>,
    pyr: OnceLock<Box<[MinMax]>>,
}

/// Values, exact copies and pyramids alike, the pyramids built for the comparison if they are not.
impl PartialEq for Chunk {
    fn eq(&self, other: &Chunk) -> bool {
        self.y == other.y && self.exact == other.exact && self.build() == other.build()
    }
}

impl Chunk {
    /// A chunk with its pyramid built. O(samples).
    pub fn new(y: Box<[f32]>, exact: Option<Box<[f64]>>) -> Chunk {
        let chunk = Chunk::unbuilt(y, exact);
        chunk.build();
        chunk
    }

    /// A chunk whose pyramid is built later (`build`). O(1).
    pub fn unbuilt(y: Box<[f32]>, exact: Option<Box<[f64]>>) -> Chunk {
        Chunk { y, exact, pyr: OnceLock::new() }
    }

    /// The pyramid, built here when it is not yet: O(samples) the first time, O(1) after. A
    /// background job's (and a comparison's): a call while another thread builds it waits for that
    /// one, so a reader that must not wait takes `pyramid` or `level`.
    pub fn build(&self) -> &[MinMax] {
        self.pyr.get_or_init(|| pyramid(&self.y))
    }

    /// The pyramid when it is built, else none. Never waits: O(1).
    pub fn pyramid(&self) -> Option<&[MinMax]> {
        self.pyr.get().map(|p| &p[..])
    }

    /// A pyramid's bins over `samples` values, built or not: `samples / 4 + samples / 16 + ...`.
    pub fn pyramid_len(samples: usize) -> usize {
        let mut len = samples / 4;
        let mut bins = len;
        while len >= 4 {
            len /= 4;
            bins += len;
        }
        bins
    }

    /// Pyramid level `k` (0: bins of 4 samples), none until the pyramid is built. Never waits:
    /// O(k).
    pub fn level(&self, k: usize) -> Option<&[MinMax]> {
        let pyr = self.pyramid()?;
        let mut start = 0;
        let mut len = self.y.len() / 4;
        for _ in 0..k {
            start += len;
            len /= 4;
        }
        Some(&pyr[start.min(pyr.len())..(start + len).min(pyr.len())])
    }
}

/// `y`'s pyramid (`Chunk`). O(samples).
fn pyramid(y: &[f32]) -> Box<[MinMax]> {
    let mut pyr = Vec::with_capacity(Chunk::pyramid_len(y.len()));
    let mut start = 0;
    let mut len = y.len() / 4;
    for i in 0..len {
        let b = &y[i * 4..i * 4 + 4];
        pyr.push(MinMax { min: b.iter().copied().fold(f32::INFINITY, f32::min), max: b.iter().copied().fold(f32::NEG_INFINITY, f32::max) });
    }
    while len >= 4 {
        let next = len / 4;
        for i in 0..next {
            let b = &pyr[start + i * 4..start + i * 4 + 4];
            let mm = MinMax { min: b.iter().map(|m| m.min).fold(f32::INFINITY, f32::min), max: b.iter().map(|m| m.max).fold(f32::NEG_INFINITY, f32::max) };
            pyr.push(mm);
        }
        start += len;
        len = next;
    }
    pyr.into_boxed_slice()
}

static ABSENT: OnceLock<Arc<Chunk>> = OnceLock::new();

/// The shared chunk for a signal absent from a closed position. Its NaN samples and pyramid are
/// allocated once. Each absent position holds just an `Arc` and adds zero payload bytes.
pub fn absent_chunk() -> &'static Arc<Chunk> {
    ABSENT.get_or_init(|| Arc::new(Chunk::new(vec![f32::NAN; CHUNK].into_boxed_slice(), None)))
}

/// Whether this is the shared absent chunk, by pointer identity.
pub fn is_absent(chunk: &Arc<Chunk>) -> bool {
    ABSENT.get().is_some_and(|absent| Arc::ptr_eq(chunk, absent))
}

fn chunk_bytes(chunk: &Arc<Chunk>) -> usize {
    if is_absent(chunk) {
        return 0;
    }
    // the pyramid counted as it is once built, so a store's bytes never wait on a job
    size_of_val(&*chunk.y) + Chunk::pyramid_len(chunk.y.len()) * size_of::<MinMax>() + chunk.exact.as_deref().map_or(0, size_of_val)
}

/// The time axis: closed chunks of `CHUNK` times and the open tail.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimeAxis {
    pub chunks: Vec<Arc<[f64]>>,
    pub open: Vec<f64>,
}

impl TimeAxis {
    pub fn len(&self) -> usize {
        self.chunks.len() * CHUNK + self.open.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn at(&self, i: usize) -> f64 {
        let (c, k) = (i / CHUNK, i % CHUNK);
        if c < self.chunks.len() {
            self.chunks[c][k]
        } else {
            self.open[i - self.chunks.len() * CHUNK]
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalTrace {
    pub id: SignalId,
    pub chunks: Vec<Arc<Chunk>>,
    /// The open tail's display values: as many as the time axis's open tail, or fewer when the
    /// store's writes left the signal out meanwhile (`TraceStore::fine`): NaN past its end.
    pub open: Vec<f32>,
    /// The open tail's exact copy, for a measured signal.
    pub open_exact: Option<Vec<f64>>,
}

impl SignalTrace {
    /// The display sample at `i`, NaN in an absent stretch.
    pub fn at(&self, i: usize) -> f32 {
        let (c, k) = (i / CHUNK, i % CHUNK);
        if c < self.chunks.len() {
            self.chunks[c].y[k]
        } else {
            self.open.get(i - self.chunks.len() * CHUNK).copied().unwrap_or(f32::NAN)
        }
    }

    /// The open tail (and its exact copy) as long as `len` points, the ones it was not written
    /// at a gap (NaN). O(points added).
    fn reach(&mut self, len: usize) {
        fill_gap(&mut self.open, len);
        if let Some(e) = &mut self.open_exact {
            if e.len() < len {
                e.resize(len, f64::NAN);
            }
        }
    }

    /// The open tail as a closed chunk of `CHUNK` points: the shared absent chunk when it holds
    /// no point, else its values with a gap to the chunk's end, its pyramid built unless `later`.
    fn close(&mut self, later: bool) -> Arc<Chunk> {
        if self.open.is_empty() {
            if let Some(e) = &mut self.open_exact {
                e.clear();
            }
            return absent_chunk().clone();
        }
        self.reach(CHUNK);
        let y = std::mem::take(&mut self.open).into_boxed_slice();
        let exact = self.open_exact.as_mut().map(|e| std::mem::take(e).into_boxed_slice());
        Arc::new(if later { Chunk::unbuilt(y, exact) } else { Chunk::new(y, exact) })
    }
}

/// Whether a store writing the signals `fine` (`TraceStore::fine`) writes signal `k`'s points:
/// every signal with no mask, else those it holds (past its end, none).
pub fn writes(fine: Option<&[bool]>, k: usize) -> bool {
    fine.is_none_or(|mask| mask.get(k).copied().unwrap_or(false))
}

/// Pads `v` with a gap (NaN) to `len` points; a longer `v` stays. O(points added).
pub fn fill_gap(v: &mut Vec<f32>, len: usize) {
    if v.len() < len {
        v.resize(len, f32::NAN);
    }
}

/// A logic level from `t` on. A trace's first entry is when its level became known; every later
/// one is an edge (the levels alternate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LogicEdge {
    pub t: f64,
    pub high: bool,
}

/// One signal's logic levels: sealed chunks of [`LOGIC_CHUNK`] entries and the open tail, in time
/// order. Its oldest chunks past [`LOGIC_KEEP`] entries are spilled (`spilled` of them): the
/// producer wrote them to its spill file (`crate::spill`) and a store holds only their summaries,
/// unless a view asked for them back ([`LogicPage`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogicTrace {
    pub id: SignalId,
    /// The sealed chunks, oldest first: None for a spilled chunk not in memory.
    pub chunks: Vec<Option<Arc<[LogicEdge]>>>,
    pub open: Vec<LogicEdge>,
    /// The oldest chunks past the in-memory bound: spilled, in memory only while a view wants them.
    pub spilled: usize,
    /// Per sealed chunk, side by side so a search over them stays in a few cache lines.
    spans: Vec<ChunkSpan>,
}

/// A sealed chunk's summary: its first and last entries, the longest time between two of its
/// entries, and its newest rising and falling edges (NaN for none; a trace's first entry is no
/// edge). What draws, searches and triggers use while the chunk is spilled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ChunkSpan {
    first: f64,
    last: f64,
    gap: f64,
    first_high: bool,
    last_high: bool,
    rise: f64,
    fall: f64,
}

impl ChunkSpan {
    /// `chunk`'s summary; `opening` when it is its trace's first chunk. O(chunk).
    fn of(chunk: &[LogicEdge], opening: bool) -> ChunkSpan {
        let gap = chunk.windows(2).map(|w| w[1].t - w[0].t).fold(0.0, f64::max);
        let newest = |high: bool| chunk.iter().enumerate().rev().find(|&(k, e)| e.high == high && (k > 0 || !opening)).map_or(f64::NAN, |(_, e)| e.t);
        let (first, last) = (chunk[0], chunk[chunk.len() - 1]);
        ChunkSpan { first: first.t, last: last.t, gap, first_high: first.high, last_high: last.high, rise: newest(true), fall: newest(false) }
    }

    /// The newest edge to a level `fits` takes. O(1).
    fn newest(&self, fits: &impl Fn(bool) -> bool) -> Option<f64> {
        [(self.rise, true), (self.fall, false)].into_iter().filter(|&(t, high)| !t.is_nan() && fits(high)).map(|(t, _)| t).reduce(f64::max)
    }
}

impl LogicTrace {
    pub fn len(&self) -> usize {
        self.chunks.len() * LOGIC_CHUNK + self.open.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Entry `i`; inside a spilled chunk not in memory only its first and last, which its summary
    /// holds (None for the rest, and past the end). O(1).
    pub fn get(&self, i: usize) -> Option<LogicEdge> {
        let (c, k) = (i / LOGIC_CHUNK, i % LOGIC_CHUNK);
        match self.chunks.get(c) {
            Some(Some(chunk)) => Some(chunk[k]),
            Some(None) => {
                let span = &self.spans[c];
                match k {
                    0 => Some(LogicEdge { t: span.first, high: span.first_high }),
                    k if k == LOGIC_CHUNK - 1 => Some(LogicEdge { t: span.last, high: span.last_high }),
                    _ => None,
                }
            }
            None => self.open.get(i - self.chunks.len() * LOGIC_CHUNK).copied(),
        }
    }

    /// The first and last times of entry `i`'s chunk when it is spilled and not in memory. O(1).
    pub fn away(&self, i: usize) -> Option<(f64, f64)> {
        let c = i / LOGIC_CHUNK;
        matches!(self.chunks.get(c), Some(None)).then(|| (self.spans[c].first, self.spans[c].last))
    }

    /// When the level became known (the first entry's time); None while empty.
    pub fn first_time(&self) -> Option<f64> {
        self.spans.first().map(|span| span.first).or_else(|| self.open.first().map(|e| e.t))
    }

    /// The entries at or before `t`: one search of the chunks' last times, then one in a chunk. A
    /// spilled chunk not in memory that `t` falls in (from its first entry to before its last)
    /// counts none of its entries: the answer is its start ([`Self::away`] says so). O(log n).
    pub fn upto(&self, t: f64) -> usize {
        let c = self.spans.partition_point(|span| span.last <= t);
        let within = match self.chunks.get(c) {
            Some(Some(chunk)) => chunk.partition_point(|e| e.t <= t),
            Some(None) => 0,
            None => self.open.partition_point(|e| e.t <= t),
        };
        c * LOGIC_CHUNK + within
    }

    /// [`Self::upto`] for a `t` whose answer is `from` or more: a few steps forward, else the search.
    pub fn upto_from(&self, from: usize, t: f64) -> usize {
        let end = (from + 8).min(self.len());
        for i in from..end {
            match self.get(i) {
                Some(e) if e.t > t => return i,
                Some(_) => {}
                None => return self.upto(t),
            }
        }
        if end == self.len() { end } else { self.upto(t) }
    }

    /// From entry `i` on, where its entries are never more than `gap` apart (within each sealed
    /// chunk and between them), the last time of that dense stretch, looked at up to `until`; None
    /// when entry `i`'s chunk is open or sparser. Every span longer than twice `gap` inside the
    /// stretch holds two entries or more. Summaries only, so spilled chunks answer too.
    /// O(chunks looked at).
    pub fn dense_until(&self, i: usize, gap: f64, until: f64) -> Option<f64> {
        let mut c = i / LOGIC_CHUNK;
        let mut last = self.spans.get(c).filter(|span| span.gap <= gap)?.last;
        while last < until {
            match self.spans.get(c + 1) {
                Some(next) if next.gap <= gap && next.first - last <= gap => last = next.last,
                _ => break,
            }
            c += 1;
        }
        Some(last)
    }

    /// The level at `t` (the newest entry at or before it); None before the first, and inside a
    /// spilled chunk not in memory. O(log n).
    pub fn level_at(&self, t: f64) -> Option<bool> {
        let c = self.spans.partition_point(|span| span.last <= t);
        if matches!(self.chunks.get(c), Some(None)) && t >= self.spans[c].first {
            return None;
        }
        self.upto(t).checked_sub(1).and_then(|i| self.get(i)).map(|e| e.high)
    }

    /// The newest edge in `[t0, t1]` to a level `fits` takes, at its exact time (a trace's first
    /// entry is no edge). A spilled chunk not in memory answers from its summary (its newest rise
    /// and fall); None when the answer would lie inside one that `t1` falls in before its newest
    /// such edge (unknown until it is loaded). O(log n + entries and chunks walked).
    pub fn last_edge(&self, t0: f64, t1: f64, fits: impl Fn(bool) -> bool) -> Option<f64> {
        let c = self.spans.partition_point(|span| span.last <= t1);
        let mut i = if matches!(self.chunks.get(c), Some(None)) && self.spans[c].first <= t1 {
            match self.spans[c].newest(&fits) {
                Some(e) if e <= t1 => return (e >= t0).then_some(e),
                Some(_) => return None,
                None => c * LOGIC_CHUNK,
            }
        } else {
            self.upto(t1)
        };
        while i > 1 {
            let c = (i - 1) / LOGIC_CHUNK;
            if matches!(self.chunks.get(c), Some(None)) {
                // a whole spilled chunk at or before t1: its summary
                let span = &self.spans[c];
                if span.last < t0 {
                    return None;
                }
                if let Some(e) = span.newest(&fits) {
                    return (e >= t0).then_some(e);
                }
                i = c * LOGIC_CHUNK;
                continue;
            }
            i -= 1;
            let edge = self.get(i)?;
            if edge.t < t0 {
                return None;
            }
            if fits(edge.high) {
                return Some(edge.t);
            }
        }
        None
    }

    /// The first times of the spilled chunks a view of `[t0, t1]` needs in memory: those it
    /// overlaps with an entry gap over `gap` (a view's half column: denser ones draw busy from
    /// their summaries). O(log n + chunks in the window).
    pub fn spilled_in(&self, t0: f64, t1: f64, gap: f64) -> impl Iterator<Item = f64> + '_ {
        let spans = &self.spans[..self.spilled.min(self.spans.len())];
        let from = spans.partition_point(|span| span.last < t0);
        spans[from..].iter().take_while(move |span| span.first <= t1).filter(move |span| span.gap > gap).map(|span| span.first)
    }

    /// The chunk that starts at `first`, by its summaries. O(log n).
    fn chunk_at(&self, first: f64) -> Option<usize> {
        let c = self.spans.partition_point(|span| span.first < first);
        self.spans.get(c).filter(|span| span.first == first).map(|_| c)
    }

    /// Appends `edge`; a tail reaching [`LOGIC_CHUNK`] seals into a chunk, `theirs` when it holds
    /// the same entries (the producer's, shared), else a new one. The sealed chunk is returned.
    fn push(&mut self, edge: LogicEdge, theirs: impl FnOnce(&[LogicEdge]) -> Option<Arc<[LogicEdge]>>) -> Option<Arc<[LogicEdge]>> {
        self.open.push(edge);
        if self.open.len() < LOGIC_CHUNK {
            return None;
        }
        let chunk = theirs(&self.open).unwrap_or_else(|| Arc::from(&self.open[..]));
        self.spans.push(ChunkSpan::of(&chunk, self.chunks.is_empty()));
        self.open.clear();
        self.chunks.push(Some(chunk.clone()));
        Some(chunk)
    }

    /// Keeps the entries before `end` (at or before it with `inclusive`) and drops the rest. The
    /// sealed chunk the cut falls in is open again with its kept entries; one not in memory takes
    /// them from `away` (the producer's file, by the chunk's first time), or without it is dropped
    /// whole. Returns that chunk's first time when it was not in memory and `away` gave it.
    /// O(log n + chunk).
    fn cut(&mut self, end: f64, inclusive: bool, away: impl FnOnce(f64) -> Option<Arc<[LogicEdge]>>) -> Option<f64> {
        let kept = |t: f64| if inclusive { t <= end } else { t < end };
        // the first sealed chunk with an entry past the cut
        let c = self.spans.partition_point(|span| kept(span.last));
        if c == self.chunks.len() {
            let n = self.open.partition_point(|e| kept(e.t));
            self.open.truncate(n);
            return None;
        }
        let first = self.spans[c].first;
        let mut from_away = None;
        let mut open = Vec::new();
        if kept(first) {
            let chunk = match &self.chunks[c] {
                Some(chunk) => Some(chunk.clone()),
                None => {
                    let chunk = away(first);
                    from_away = chunk.as_ref().map(|_| first);
                    chunk
                }
            };
            if let Some(chunk) = chunk {
                open.extend_from_slice(&chunk[..chunk.partition_point(|e| kept(e.t))]);
            }
        }
        self.chunks.truncate(c);
        self.spans.truncate(c);
        self.spilled = self.spilled.min(c);
        self.open = open;
        from_away
    }
}

/// A live run went back to its time `t` (`TraceDelta::rewind`): what a store drops before it
/// takes the rest of the delta.
#[derive(Clone, Debug, PartialEq)]
pub struct Rewind {
    /// The points, marks and logic entries after it go.
    pub t: f64,
    /// Logic traces whose entries go from a time on instead, by signal: the producer's cut fell in
    /// a chunk past its in-memory bound, which it read back from its file and opened again, and the
    /// delta carries that chunk's entries up to `t` again (a consumer holds only its summary).
    pub resent: Vec<(SignalId, f64)>,
}

/// A logic chunk past the producer's in-memory bound ([`LOGIC_KEEP`]), by its signal and first
/// time: spilled (`chunk` None: a store keeps its summary) or in memory again because a view asked
/// (`chunk` Some, the producer's, shared). A store applies them in order (`TraceStore::page_logic`).
#[derive(Clone, Debug, PartialEq)]
pub struct LogicPage {
    pub id: SignalId,
    pub first: f64,
    pub chunk: Option<Arc<[LogicEdge]>>,
}

/// What a record keeps ([`TraceStore::keep`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Keep {
    /// Simulated seconds behind the newest point to keep. Infinity keeps the run's whole span.
    pub span: f64,
    /// Closed-position byte budget. The oldest positions fold past it. `usize::MAX` never folds.
    pub budget: usize,
    /// Signals kept over the whole span, by index. Indices past the mask keep only the front.
    /// `None` keeps every signal over the whole span.
    pub whole: Option<Arc<[bool]>>,
    /// Simulated seconds of recent values kept for every other signal. Closed positions ending
    /// before this front have the shared absent chunk for those signals.
    pub front: f64,
}

impl Keep {
    /// Keeps every signal over the trailing `seconds`, folding past [`RECORD_FINE_BYTES`] when finite.
    pub fn last(seconds: f64) -> Keep {
        Keep { span: seconds, budget: if seconds.is_finite() { RECORD_FINE_BYTES } else { usize::MAX }, whole: None, front: f64::INFINITY }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TraceStore {
    pub time: TimeAxis,
    pub signals: Vec<SignalTrace>,
    /// The logic traces, by signal, in the order their first entries came.
    pub logic: Vec<LogicTrace>,
    pub generation: u64,
    /// The producer's sample index of this store's first point: 0, except for a consumer that
    /// started mid-stream.
    pub base: usize,
    /// Event times (`TraceDelta::marks`), seconds, increasing.
    pub marks: Vec<f64>,
    /// Per closed position, how many folds made it (0: whole, `k`: 2^k positions' span in one);
    /// never increasing from the oldest on.
    pub folds: Vec<u8>,
    /// Populated payload bytes at each closed position, and their sum for the fold budget.
    position_sizes: Vec<usize>,
    closed_bytes: usize,
    /// Number of leading closed positions already thinned for this whole-signal mask.
    thinned: usize,
    thinned_for: Option<Arc<[bool]>>,
    /// The rewinds the store took ([`Rewind`]): a reader keeping positions into it (a decoder's
    /// cursors) starts over when this changes, as on a new generation.
    pub rewinds: u64,
    /// The signals whose points `push` and `push_rows` write, by signal (a producer's
    /// `LiveSpec::fine`: the rows a view shows); None writes every signal. One left out reads NaN
    /// at the points pushed meanwhile, its closed positions there the shared absent chunk; one
    /// put back is written from its next point on. O(1) to change.
    pub fine: Option<Arc<[bool]>>,
    /// The folds this store made or applied: a reader keeping columns over the folded positions
    /// reduces them again when this changes.
    pub folds_made: u64,
    /// Whether the chunks this store closes or folds leave their pyramids to a background job
    /// (`defer_pyramids`), and those made since the last `take_unbuilt`.
    deferred: Deferred,
}

/// A store's chunks whose pyramids wait for a background job (`TraceStore::defer_pyramids`).
/// Bookkeeping, not content: two stores compare alike whatever it holds.
#[derive(Clone, Debug, Default)]
struct Deferred {
    on: bool,
    chunks: Vec<Arc<Chunk>>,
}

impl PartialEq for Deferred {
    fn eq(&self, _: &Deferred) -> bool {
        true
    }
}

/// What a consumer receives: the points appended since `from` (a sample index) and the chunks
/// that closed meanwhile, which replace the consumer's open tails up to their end.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceDelta {
    pub generation: u64,
    pub from: usize,
    pub time: Arc<[f64]>,
    /// By signal, the same length as `time`, or empty for a signal with no point written in
    /// this interval (`TraceStore::fine`): a gap there.
    pub values: Vec<Arc<[f32]>>,
    /// Chunks closed in this interval, with their pyramids, in order per signal.
    pub closed: Vec<(SignalId, Arc<Chunk>)>,
    /// The accepted event times in this interval, seconds, increasing: the stub sources' edges and
    /// the switching instants the stepper landed on exactly, which a trigger may lock to.
    pub marks: Vec<f64>,
    /// The logic entries found in this interval, by signal, in order.
    pub logic: Vec<(SignalId, Arc<[LogicEdge]>)>,
    /// Logic chunks the producer sealed in this interval, in order per signal: a consumer whose
    /// tail holds the same entries shares them.
    pub logic_closed: Vec<(SignalId, Arc<[LogicEdge]>)>,
    /// The producer's logic chunks that spilled or came back in this interval, in order: a
    /// consumer lets go of its copies and takes the ones back ([`TraceStore::page_logic`]).
    pub logic_pages: Vec<LogicPage>,
    /// The folds the producer made in this interval, in order ([`TraceStore::fold`]): a consumer
    /// holding the same positions makes them with the same chunks.
    pub folds: Vec<Fold>,
    /// The run went back first ([`Rewind`]): applied before everything else in the delta, which
    /// then continues the store from the time it went back to.
    pub rewind: Option<Rewind>,
}

/// One fold a producer made: its two positions spanning `first..=last` became one of `time` and
/// `chunks` (by signal), folded `level` times.
#[derive(Clone, Debug, PartialEq)]
pub struct Fold {
    pub first: f64,
    pub last: f64,
    pub level: u8,
    pub time: Arc<[f64]>,
    pub chunks: Vec<Arc<Chunk>>,
}

impl TraceStore {
    /// An empty store of `exact.len()` signals; `exact[i]` keeps an f64 copy of signal i.
    pub fn new(generation: u64, exact: &[bool]) -> TraceStore {
        let signals = exact
            .iter()
            .enumerate()
            .map(|(i, &e)| SignalTrace { id: SignalId(i as u32), chunks: Vec::new(), open: Vec::new(), open_exact: if e { Some(Vec::new()) } else { None } })
            .collect();
        TraceStore { signals, generation, ..TraceStore::default() }
    }

    /// From now on the chunks this store closes or folds leave their pyramids to a background job
    /// (`take_unbuilt`): a close then builds none on the producer's thread. Off builds each as it
    /// is made, and here those still waiting. A producer's with a pool to post to. O(1) on, O(the
    /// waiting chunks' samples) off.
    pub fn defer_pyramids(&mut self, on: bool) {
        self.deferred.on = on;
        if !on {
            for c in std::mem::take(&mut self.deferred.chunks) {
                c.build();
            }
        }
    }

    /// The chunks closed or folded since the last call whose pyramids wait for a background job,
    /// to build there (`Chunk::build`); a reader meanwhile reads their values. O(1).
    pub fn take_unbuilt(&mut self) -> Vec<Arc<Chunk>> {
        std::mem::take(&mut self.deferred.chunks)
    }

    pub fn len(&self) -> usize {
        self.time.len()
    }
    pub fn is_empty(&self) -> bool {
        self.time.is_empty()
    }

    /// Drops whole closed chunk positions whose last time is before `t`, leaving open tails
    /// intact. Advances `base` by the samples dropped, preserving `base + len()` and delta
    /// continuity. Shared chunks remain alive in other owners, including ghosts. Marks before the
    /// first kept time are dropped; logic traces keep their whole history. Returns the positions
    /// dropped.
    pub fn trim_before(&mut self, t: f64) -> usize {
        let count = self.time.chunks.partition_point(|chunk| chunk[CHUNK - 1] < t);
        if count > 0 {
            drop(self.time.chunks.drain(..count));
            for signal in &mut self.signals {
                drop(signal.chunks.drain(..count));
            }
            drop(self.folds.drain(..count.min(self.folds.len())));
            self.closed_bytes -= self.position_sizes.drain(..count).sum::<usize>();
            self.thinned = self.thinned.saturating_sub(count);
            self.base += count * CHUNK;
        }
        if self.is_empty() {
            self.marks.clear();
        } else {
            let first = self.time.at(0);
            let marks = self.marks.partition_point(|&time| time < first);
            drop(self.marks.drain(..marks));
        }
        count
    }

    /// Keeps every signal over the trailing `seconds` with [`Keep::last`]. Returns the number
    /// of closed positions trimmed from the span.
    pub fn keep_last(&mut self, seconds: f64) -> usize {
        self.keep(&Keep::last(seconds), &mut Vec::new())
    }

    /// Trims closed positions outside the span, thins signals behind the front, then folds to
    /// the byte budget. Returns the number of positions trimmed and appends new folds to `made`.
    /// Thinning costs O(signals) per position as it passes the front. Changing the mask revisits
    /// the older positions once in O(positions × signals). Newly whole signals keep whatever
    /// values are still present. Logic traces keep their own history ([`Self::spill_logic`]).
    pub fn keep(&mut self, keep: &Keep, made: &mut Vec<Fold>) -> usize {
        if self.is_empty() {
            return 0;
        }
        let newest = self.time.at(self.len() - 1);
        let dropped = self.trim_before(newest - keep.span);
        if self.thinned_for != keep.whole {
            self.thinned = 0;
            self.thinned_for.clone_from(&keep.whole);
        }
        if let Some(whole) = &keep.whole {
            let cutoff = newest - keep.front;
            while self.thinned < self.time.chunks.len() && self.time.chunks[self.thinned][CHUNK - 1] < cutoff {
                let p = self.thinned;
                let mut removed = 0;
                for (i, signal) in self.signals.iter_mut().enumerate() {
                    if !whole.get(i).copied().unwrap_or(false) {
                        let chunk = &mut signal.chunks[p];
                        removed += chunk_bytes(chunk);
                        *chunk = absent_chunk().clone();
                    }
                }
                self.closed_bytes -= removed;
                self.position_sizes[p] -= removed;
                self.thinned += 1;
            }
        }
        self.fold(keep.budget, made);
        dropped
    }

    /// Payload bytes of the oldest closed position, or the size of its time chunk when none is
    /// closed. Includes the values, pyramid and exact copy of each present signal. O(1).
    pub fn position_bytes(&self) -> usize {
        self.position_sizes.first().copied().unwrap_or(CHUNK * size_of::<f64>())
    }

    fn payload_at(&self, p: usize) -> usize {
        size_of_val(&*self.time.chunks[p]) + self.signals.iter().map(|s| chunk_bytes(&s.chunks[p])).sum::<usize>()
    }

    /// Records the size and fold level once all chunks of a new position have closed.
    fn close_position(&mut self) {
        let bytes = self.payload_at(self.position_sizes.len());
        self.closed_bytes += bytes;
        self.position_sizes.push(bytes);
        self.folds.push(0);
    }

    /// Folds the oldest closed positions while they hold more than `budget` bytes, one pair at a
    /// time: the oldest two folded alike, else the oldest two (a binary counter: resolution falls
    /// with age and the newest positions stay whole). A pair becomes one position of `CHUNK`
    /// points: each block of four at its first and last times, each signal's lowest and highest
    /// value there in the order they came (its exact copy alike), so every extreme a pyramid shows
    /// stays. `base` moves on by `CHUNK`, so `base + len()` (the deltas' continuity) holds.
    /// Each fold is added to `made`. A consumer mirrors its producer's folds as its deltas bring
    /// them ([`Self::apply`]) and folds by itself only what it could not mirror. Returns the pairs
    /// folded. The budget check is O(1). A fold reduces `CHUNK` points for the times and each
    /// signal present in either input. Two absent inputs share the absent chunk in O(1). Removing
    /// the second position also shifts up to O(positions) chunk pointers per signal.
    pub fn fold(&mut self, budget: usize, made: &mut Vec<Fold>) -> usize {
        if self.closed_bytes <= budget || self.time.chunks.len() < 2 {
            return 0;
        }
        self.folds.resize(self.time.chunks.len(), 0);
        let mut folded = 0;
        while self.time.chunks.len() >= 2 && self.closed_bytes > budget {
            let n = self.time.chunks.len();
            let p = (0..n - 1).find(|&p| self.folds[p] == self.folds[p + 1]).unwrap_or(0);
            let (time, chunks) = self.fold_pair(p);
            if self.deferred.on {
                self.deferred.chunks.extend(chunks.iter().filter(|c| !is_absent(c)).cloned());
            }
            let fold = Fold {
                first: self.time.chunks[p][0],
                last: self.time.chunks[p + 1][CHUNK - 1],
                level: self.folds[p].max(self.folds[p + 1]).saturating_add(1),
                time,
                chunks,
            };
            self.put_fold(p, &fold);
            made.push(fold);
            folded += 1;
        }
        folded
    }

    /// Positions `p` and `p + 1` become `fold`'s one.
    fn put_fold(&mut self, p: usize, fold: &Fold) {
        drop(self.time.chunks.splice(p..p + 2, [fold.time.clone()]));
        for (signal, chunk) in self.signals.iter_mut().zip(&fold.chunks) {
            drop(signal.chunks.splice(p..p + 2, [chunk.clone()]));
        }
        drop(self.folds.splice(p..p + 2, [fold.level]));
        let bytes = self.payload_at(p);
        self.closed_bytes -= self.position_sizes.splice(p..p + 2, [bytes]).sum::<usize>();
        self.closed_bytes += bytes;
        // Two thinned positions count as one. A pair straddling the front still needs thinning.
        if p < self.thinned {
            self.thinned -= 1;
        }
        self.base += CHUNK;
        self.folds_made += 1;
    }

    /// Makes the producer's `fold` here when this store holds the same two positions (the same
    /// first and last times, the same signals): the folded chunks are then shared. False when it
    /// does not (they were trimmed, or this store started elsewhere). The lookup is O(log positions).
    /// Replacing the pair shifts up to O(positions) chunk pointers per signal.
    fn mirror_fold(&mut self, fold: &Fold) -> bool {
        let n = self.time.chunks.len();
        let p = self.time.chunks.partition_point(|chunk| chunk[0] < fold.first);
        if p + 1 >= n || self.time.chunks[p][0] != fold.first || self.time.chunks[p + 1][CHUNK - 1] != fold.last || fold.chunks.len() != self.signals.len() {
            return false;
        }
        self.folds.resize(n, 0);
        self.put_fold(p, fold);
        true
    }

    /// Positions `p` and `p + 1` folded into one ([`Self::fold`]).
    fn fold_pair(&self, p: usize) -> (Arc<[f64]>, Vec<Arc<Chunk>>) {
        let (a, b) = (&self.time.chunks[p], &self.time.chunks[p + 1]);
        let t_at = |i: usize| if i < CHUNK { a[i] } else { b[i - CHUNK] };
        let time: Arc<[f64]> = (0..CHUNK / 2).flat_map(|k| [t_at(4 * k), t_at(4 * k + 3)]).collect();
        let chunks = self
            .signals
            .iter()
            .map(|signal| {
                let (ca, cb) = (&signal.chunks[p], &signal.chunks[p + 1]);
                if is_absent(ca) && is_absent(cb) {
                    return ca.clone();
                }
                let y_at = |i: usize| if i < CHUNK { ca.y[i] } else { cb.y[i - CHUNK] };
                let exact = ca.exact.as_deref().zip(cb.exact.as_deref());
                let mut y = Vec::with_capacity(CHUNK);
                let mut kept = exact.map(|_| Vec::with_capacity(CHUNK));
                for k in 0..CHUNK / 2 {
                    let (lo, hi) = block_extremes(4 * k, y_at);
                    for i in if lo <= hi { [lo, hi] } else { [hi, lo] } {
                        y.push(y_at(i));
                        if let (Some(kept), Some((ea, eb))) = (&mut kept, exact) {
                            kept.push(if i < CHUNK { ea[i] } else { eb[i - CHUNK] });
                        }
                    }
                }
                let (y, kept) = (y.into_boxed_slice(), kept.map(Vec::into_boxed_slice));
                Arc::new(if self.deferred.on { Chunk::unbuilt(y, kept) } else { Chunk::new(y, kept) })
            })
            .collect();
        (time, chunks)
    }

    /// The producer's bound on its logic traces' memory: each trace's sealed chunks older than its
    /// newest [`LOGIC_KEEP`] entries go to `spill` and leave memory, except those a view wants
    /// (`wanted`, by signal and first time: written all the same and kept). Each goes to `out` for
    /// the next delta. When the file cannot be written the chunks stay (the spill logs it).
    /// Returns the entries spilled. O(chunks spilled).
    pub fn spill_logic(&mut self, spill: &mut crate::spill::LogicSpill, wanted: &[(SignalId, f64)], out: &mut Vec<LogicPage>) -> usize {
        let mut count = 0;
        for trace in &mut self.logic {
            let cut = trace.len().saturating_sub(LOGIC_KEEP).div_ceil(LOGIC_CHUNK).min(trace.chunks.len());
            while trace.spilled < cut {
                let c = trace.spilled;
                let Some(chunk) = trace.chunks[c].clone() else {
                    trace.spilled += 1;
                    continue;
                };
                if !spill.write(trace.id, &chunk) {
                    return count;
                }
                let first = chunk[0].t;
                let page = if wanted.contains(&(trace.id, first)) {
                    spill.back.push((trace.id, first));
                    Some(chunk)
                } else {
                    trace.chunks[c] = None;
                    None
                };
                out.push(LogicPage { id: trace.id, first, chunk: page });
                trace.spilled += 1;
                count += LOGIC_CHUNK;
            }
        }
        count
    }

    /// The producer's answer to the views' asks: the spilled chunks in `wanted` (by signal and
    /// first time) come back into memory from `spill`, and those back that no view wants any more
    /// leave it again; each change goes to `out` for the next delta. O(wanted + chunks back), a
    /// file read per chunk loaded.
    pub fn load_logic(&mut self, spill: &mut crate::spill::LogicSpill, wanted: &[(SignalId, f64)], out: &mut Vec<LogicPage>) {
        let mut back = std::mem::take(&mut spill.back);
        back.retain(|&(id, first)| {
            if wanted.contains(&(id, first)) {
                return true;
            }
            let page = LogicPage { id, first, chunk: None };
            if self.page_logic(&page) {
                out.push(page);
            }
            false
        });
        for &(id, first) in wanted {
            let Some(trace) = self.logic.iter().find(|trace| trace.id == id) else { continue };
            let away = trace.chunk_at(first).is_some_and(|c| c < trace.spilled && trace.chunks[c].is_none());
            if !away || back.contains(&(id, first)) {
                continue;
            }
            let Some(chunk) = spill.read(id, first) else { continue };
            let page = LogicPage { id, first, chunk: Some(chunk) };
            if self.page_logic(&page) {
                back.push((id, first));
                out.push(page);
            }
        }
        spill.back = back;
    }

    /// Applies a producer's [`LogicPage`]: the chunk of signal `id` starting at `first` is past the
    /// in-memory bound, and in memory as the page says. False when this store has no such chunk.
    /// O(log n).
    pub fn page_logic(&mut self, page: &LogicPage) -> bool {
        let Some(trace) = self.logic.iter_mut().find(|trace| trace.id == page.id) else { return false };
        let Some(c) = trace.chunk_at(page.first) else { return false };
        if page.chunk.as_ref().is_some_and(|chunk| chunk.len() != LOGIC_CHUNK || chunk[0].t != page.first) {
            return false;
        }
        trace.chunks[c] = page.chunk.clone();
        trace.spilled = trace.spilled.max(c + 1);
        true
    }

    /// The logic trace of signal `id`, when it has one.
    pub fn logic(&self, id: SignalId) -> Option<&LogicTrace> {
        self.logic.iter().find(|trace| trace.id == id)
    }

    fn logic_mut(&mut self, id: SignalId) -> &mut LogicTrace {
        let i = match self.logic.iter().position(|trace| trace.id == id) {
            Some(i) => i,
            None => {
                self.logic.push(LogicTrace { id, ..LogicTrace::default() });
                self.logic.len() - 1
            }
        };
        &mut self.logic[i]
    }

    /// Appends a logic entry of signal `id` (the producer's); a chunk it seals goes to `closed`,
    /// for the producer's next delta.
    pub fn push_logic(&mut self, id: SignalId, edge: LogicEdge, closed: &mut Vec<(SignalId, Arc<[LogicEdge]>)>) {
        if let Some(chunk) = self.logic_mut(id).push(edge, |_| None) {
            closed.push((id, chunk));
        }
    }

    /// How many leading closed positions are thinned to the whole signals (`Keep::whole`): a
    /// reader that kept columns over them reduces them again when this moves.
    pub fn thinned(&self) -> usize {
        self.thinned
    }

    /// Bytes of stored time points, signal values and exact copies, pyramids, open tails,
    /// marks and logic entries. The absent chunk counts as zero. Other shared chunks count in
    /// full. Counts populated elements, excluding container and allocator overhead. Uses the
    /// cached total for closed analog positions and walks the signals, logic traces and logic chunks.
    pub fn bytes(&self) -> usize {
        let mut bytes = self.closed_bytes + (self.time.open.len() + self.marks.len()) * size_of::<f64>();
        for signal in &self.signals {
            bytes += signal.open.len() * size_of::<f32>();
            bytes += signal.open_exact.as_ref().map_or(0, |exact| exact.len() * size_of::<f64>());
        }
        for trace in &self.logic {
            let resident = trace.chunks.iter().filter(|chunk| chunk.is_some()).count();
            bytes += (resident * LOGIC_CHUNK + trace.open.len()) * size_of::<LogicEdge>();
        }
        bytes
    }

    /// A consumer's side of a [`Rewind`]: the points, marks and logic entries after `r.t` go (a
    /// resent trace's from its time on), the chunk each cut falls in open again with its kept
    /// part. A logic chunk the cut falls in that this store holds only the summary of goes whole
    /// (the producer resends it when its cut falls there). `base` stays. O(log n + chunks).
    pub fn rewind(&mut self, r: &Rewind) {
        self.cut_points(r.t);
        for trace in &mut self.logic {
            match r.resent.iter().find(|(id, _)| *id == trace.id) {
                Some(&(_, first)) => trace.cut(first, false, |_| None),
                None => trace.cut(r.t, true, |_| None),
            };
        }
    }

    /// The producer's side: cuts at `t` as [`Self::rewind`] does, reading a logic chunk the cut
    /// falls in back from `spill` when it is not in memory, and makes the spill forget the chunks
    /// the cut dropped or opened. Returns the rewind to send: the logic traces opened from the file
    /// are its `resent` (their kept entries are now their open tails, to be sent again).
    /// O(log n + chunks + chunks spilled).
    pub fn rewind_producer(&mut self, t: f64, spill: &mut crate::spill::LogicSpill) -> Rewind {
        self.cut_points(t);
        let mut resent = Vec::new();
        for trace in &mut self.logic {
            let id = trace.id;
            if let Some(first) = trace.cut(t, true, |first| spill.read(id, first)) {
                resent.push((id, first));
            }
        }
        spill.retain(|id, first| self.logic(id).is_some_and(|trace| trace.chunk_at(first).is_some()));
        Rewind { t, resent }
    }

    /// Drops the points and marks after `t`; the closed chunk the cut falls in is open again with
    /// its kept points (a folded one's too: its tail then fills with whole points).
    fn cut_points(&mut self, t: f64) {
        self.rewinds += 1;
        let m = self.marks.partition_point(|&mark| mark <= t);
        self.marks.truncate(m);
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.time.at(mid) <= t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let n = lo;
        if n == self.len() {
            return;
        }
        let (c, k) = (n / CHUNK, n % CHUNK);
        if c < self.time.chunks.len() {
            self.time.open = self.time.chunks[c][..k].to_vec();
            self.time.chunks.truncate(c);
            self.folds.truncate(c);
            self.closed_bytes -= self.position_sizes.drain(c..).sum::<usize>();
            self.thinned = self.thinned.min(c);
            for s in &mut self.signals {
                let chunk = s.chunks[c].clone();
                s.chunks.truncate(c);
                // an absent chunk opens as an empty tail: a gap up to where it is written again
                let kept = if is_absent(&chunk) { 0 } else { k };
                s.open = chunk.y[..kept].to_vec();
                if let Some(exact) = &mut s.open_exact {
                    *exact = match &chunk.exact {
                        Some(x) => x[..kept].to_vec(),
                        None => chunk.y[..kept].iter().map(|&y| f64::from(y)).collect(),
                    };
                }
            }
        } else {
            let k = n - c * CHUNK;
            self.time.open.truncate(k);
            for s in &mut self.signals {
                s.open.truncate(k);
                if let Some(exact) = &mut s.open_exact {
                    exact.truncate(k);
                }
            }
        }
    }

    /// Absolute sample index (`base` plus position) of the newest point at or before `t`,
    /// found by binary search. Returns `None` before the first kept point.
    pub fn index_at(&self, t: f64) -> Option<usize> {
        let (mut lo, mut hi) = (0, self.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.time.at(mid) <= t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo.checked_sub(1).map(|position| self.base + position)
    }

    /// Appends one point (`ys` by signal). Returns the chunks it closed (one per signal, when the
    /// tail reaches `CHUNK`), for the producer's next delta.
    pub fn push(&mut self, t: f64, ys: &[f64], closed: &mut Vec<(SignalId, Arc<Chunk>)>) {
        let at = self.time.open.len();
        self.time.open.push(t);
        let fine = self.fine.as_deref();
        for (s, &y) in self.signals.iter_mut().zip(ys).enumerate().filter(|(k, _)| writes(fine, *k)).map(|(_, e)| e) {
            s.reach(at);
            s.open.push(y as f32);
            if let Some(e) = &mut s.open_exact {
                e.push(y);
            }
        }
        if self.time.open.len() == CHUNK {
            self.close_open(closed);
        }
    }

    /// Appends `t.len()` points, as [`Self::push`] of each in turn: `y(r, s)` is point `r`'s value of
    /// signal `s`. Signal by signal, each open tail filled in one pass up to the chunk's end.
    /// O(points × signals written), plus O(signals) a call.
    pub fn push_rows(&mut self, t: &[f64], y: impl Fn(usize, usize) -> f64, closed: &mut Vec<(SignalId, Arc<Chunk>)>) {
        self.push_rows_into(t, y, closed, &mut [], 0);
    }

    /// [`Self::push_rows`], each written signal's display values also appended to `also[k]`, first
    /// padded with a gap to `also_at` (a delta's pending values, `also_at` its points before these).
    /// Each value is read once (`y`) and converted once: an exact signal's display values are its
    /// exact copy's, and `also[k]` takes the tail's new values as they were written. A signal past
    /// `also`'s end is written to the store alone. The open tails are reserved to a whole chunk when
    /// they start, so they never grow by copying and close without a reallocation.
    pub fn push_rows_into(&mut self, t: &[f64], y: impl Fn(usize, usize) -> f64, closed: &mut Vec<(SignalId, Arc<Chunk>)>, also: &mut [Vec<f32>], also_at: usize) {
        let mut from = 0;
        while from < t.len() {
            let at = self.time.open.len();
            let to = t.len().min(from + CHUNK - at);
            self.time.open.reserve_exact(CHUNK - at);
            self.time.open.extend_from_slice(&t[from..to]);
            let fine = self.fine.as_deref();
            for (k, s) in self.signals.iter_mut().enumerate().filter(|(k, _)| writes(fine, *k)) {
                s.open.reserve_exact(CHUNK.saturating_sub(s.open.len()));
                s.reach(at);
                let start = s.open.len();
                match &mut s.open_exact {
                    Some(e) => {
                        e.reserve_exact(CHUNK.saturating_sub(e.len()));
                        let e0 = e.len();
                        e.extend((from..to).map(|r| y(r, k)));
                        s.open.extend(e[e0..].iter().map(|&v| v as f32));
                    }
                    None => s.open.extend((from..to).map(|r| y(r, k) as f32)),
                }
                if let Some(p) = also.get_mut(k) {
                    fill_gap(p, also_at + from);
                    p.extend_from_slice(&s.open[start..]);
                }
            }
            from = to;
            if self.time.open.len() == CHUNK {
                self.close_open(closed);
            }
        }
    }

    /// The open tails, full, as a closed position: each signal's chunk into `closed`.
    fn close_open(&mut self, closed: &mut Vec<(SignalId, Arc<Chunk>)>) {
        self.time.chunks.push(std::mem::take(&mut self.time.open).into());
        let later = self.deferred.on;
        for s in &mut self.signals {
            let c = s.close(later);
            if later && !is_absent(&c) {
                self.deferred.chunks.push(c.clone());
            }
            s.chunks.push(c.clone());
            closed.push((s.id, c));
        }
        self.close_position();
    }

    /// Applies a producer's delta to this (consumer) store, its rewind first. Mirrored folds
    /// share the producer's chunks, including absent ones. Call [`Self::keep`] with the producer's
    /// policy to thin the other positions. A delta of another generation, or one that does not
    /// continue this store, starts the store over at the delta's first point (`base`).
    pub fn apply(&mut self, d: &TraceDelta) {
        if let Some(r) = d.rewind.as_ref().filter(|_| d.generation == self.generation) {
            self.rewind(r);
        }
        if d.generation != self.generation || d.from != self.base + self.len() || d.values.len() != self.signals.len() {
            *self = TraceStore::new(d.generation, &vec![false; d.values.len()]);
            self.base = d.from;
        }
        // the producer's chunks line up with ours only when we started on a chunk boundary
        let aligned = self.base % CHUNK == 0;
        self.marks.extend_from_slice(&d.marks);
        for (id, entries) in &d.logic {
            let trace = self.logic_mut(*id);
            for &edge in entries.iter() {
                // the producer's sealed chunk with the same entries, shared
                trace.push(edge, |tail| d.logic_closed.iter().find(|(of, chunk)| of == id && chunk[..] == *tail).map(|(_, chunk)| chunk.clone()));
            }
        }
        for page in &d.logic_pages {
            self.page_logic(page);
        }
        let mut closed = d.closed.iter();
        // the points up to the open tails' chunk end, copied signal by signal
        let mut k = 0;
        while k < d.time.len() {
            let at = self.time.open.len();
            let end = (k + CHUNK - at).min(d.time.len());
            self.time.open.extend_from_slice(&d.time[k..end]);
            // a signal the producer wrote no point of here has none: a gap
            for (s, v) in self.signals.iter_mut().zip(&d.values).filter(|(_, v)| !v.is_empty()) {
                s.reach(at);
                s.open.extend_from_slice(&v[k..end]);
            }
            k = end;
            if self.time.open.len() == CHUNK {
                self.time.chunks.push(std::mem::take(&mut self.time.open).into());
                for s in &mut self.signals {
                    let theirs = if aligned { closed.next().filter(|(id, _)| *id == s.id) } else { None };
                    let c = match theirs {
                        Some((_, c)) => {
                            s.open.clear();
                            c.clone()
                        }
                        None => s.close(false),
                    };
                    s.chunks.push(c);
                }
                self.close_position();
            }
        }
        for fold in &d.folds {
            self.mirror_fold(fold);
        }
    }
}

/// The indices of the lowest and highest values in the block of four at `at`, NaN skipped (the
/// block's first and last when all four are NaN).
fn block_extremes(at: usize, y: impl Fn(usize) -> f32) -> (usize, usize) {
    let (mut lo, mut hi) = (None::<usize>, None::<usize>);
    for i in at..at + 4 {
        let v = y(i);
        if v.is_nan() {
            continue;
        }
        if lo.is_none_or(|l| v < y(l)) {
            lo = Some(i);
        }
        if hi.is_none_or(|h| v > y(h)) {
            hi = Some(i);
        }
    }
    lo.zip(hi).unwrap_or((at, at + 3))
}

/// A consumer's store kept on the producer's thread and published whole: each delta is applied
/// there and the store goes out through `Latest`, so a consumer (the UI thread) takes the newest
/// with one `Arc` clone and merges nothing. Stores take turns: the one no reader has held longest
/// catches up by the deltas it missed (O(new points)); only when every one is held is the newest copied
/// (O(record): the closed chunks are shared, the open tails copied). A store a reader keeps (a
/// ghost, a scrub's snapshot) is let go once it is `PUBLISH_TURNS` deltas behind, and its last
/// reader frees it; the producer's own `Arc` of a replaced publication is dropped here.
pub struct TracePublisher {
    out: Arc<Latest<TraceStore>>,
    /// The stores and how many deltas each has applied.
    stores: Vec<(Arc<TraceStore>, u64)>,
    /// The deltas some store has not applied yet and the retention in effect at each one.
    log: VecDeque<(TraceDelta, Keep)>,
    seq: u64,
    copies: u64,
}

/// The stores a publisher turns: the published one, the one its reader still holds, a free one.
pub const PUBLISH_TURNS: usize = 3;

impl TracePublisher {
    pub fn new(out: Arc<Latest<TraceStore>>) -> TracePublisher {
        TracePublisher { out, stores: Vec::new(), log: VecDeque::new(), seq: 0, copies: 0 }
    }

    /// Applies `d` and its retention to a store no reader holds and publishes it. A store catching
    /// up applies each missed delta's retention in order, including any change of whole signals.
    /// A signal made whole after the producer thinned a position but before that position's delta
    /// went out stays in the published store there while the producer's is absent: the reader
    /// keeps more of the run, never other values, and the two hold the same chunks again at the
    /// next fold through that position (it carries the producer's).
    pub fn publish(&mut self, d: TraceDelta, keep: &Keep) {
        self.seq += 1;
        self.log.push_back((d, keep.clone()));
        let seq = self.seq;
        self.stores.retain(|(s, at)| Arc::strong_count(s) == 1 || seq - at <= PUBLISH_TURNS as u64);
        // the free store that has waited longest, so none falls behind for good
        let free = self.stores.iter_mut().enumerate().filter_map(|(i, (s, at))| Arc::get_mut(s).map(|_| (i, *at))).min_by_key(|&(_, at)| at);
        let i = match free {
            Some((i, _)) => i,
            None => {
                let copy = match self.stores.iter().max_by_key(|(_, at)| *at) {
                    Some((s, at)) => {
                        self.copies += 1;
                        (Arc::new((**s).clone()), *at)
                    }
                    None => (Arc::new(TraceStore::default()), seq - 1),
                };
                if self.stores.len() == PUBLISH_TURNS {
                    // every store is held: the oldest goes to its readers
                    let oldest = (0..self.stores.len()).min_by_key(|&i| self.stores[i].1).unwrap_or(0);
                    self.stores.swap_remove(oldest);
                }
                self.stores.push(copy);
                self.stores.len() - 1
            }
        };
        let first = seq + 1 - self.log.len() as u64;
        let (s, at) = &mut self.stores[i];
        let store = Arc::get_mut(s).expect("a store no reader holds");
        for (d, keep) in self.log.iter().skip((*at + 1 - first) as usize) {
            store.apply(d);
            store.keep(keep, &mut Vec::new());
        }
        *at = seq;
        drop(self.out.publish(s.clone()));
        let applied = self.stores.iter().map(|(_, at)| *at).min().unwrap_or(seq);
        let drop_n = (applied + 1).saturating_sub(first).min(self.log.len() as u64) as usize;
        drop(self.log.drain(..drop_n));
    }

    /// Stores copied because every one was held (a reader slower than the publications, a ghost).
    pub fn copies(&self) -> u64 {
        self.copies
    }

    /// Deltas kept for stores that have not applied them: at most `PUBLISH_TURNS`.
    pub fn backlog(&self) -> usize {
        self.log.len()
    }
}
