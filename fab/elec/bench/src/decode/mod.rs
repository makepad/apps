//! Protocol decoders over logic edges (the Scope's decoder rows, the AI's `elec.capture.decode`,
//! `expect` text measures). A decoder is a small state machine fed its inputs in time order, one
//! at a time, that never looks back: the edges of its lines (a logic trace's entries, a
//! simulated net's threshold crossings, a real capture's), the annotations of a decoder below it
//! (stacking: UART bytes into text lines), gaps in the data and the data's end so far. What it
//! finds are annotations ([`Ann`]): a span, a row, a kind and a value, formatted to text only when
//! drawn ([`Decoder::label`]). Fed in any chunking, a decoder finds the same annotations.
//!
//! Written from the public bus specifications.

pub mod i2c;
pub mod spi;
pub mod text;
pub mod uart;

/// What a decoder reads and what it writes: its inputs by role, and its rows by name.
#[derive(Debug, PartialEq)]
pub struct DecoderSpec {
    pub name: &'static str,
    pub inputs: &'static [InputSpec],
    pub rows: &'static [&'static str],
}

/// One input of a decoder: a logic line (`scl`), or a row of the decoder below (stacking).
#[derive(Debug, PartialEq)]
pub struct InputSpec {
    pub role: &'static str,
    pub from: InputFrom,
    /// Without it the decoder still works (an SPI bus without MISO or chip select).
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFrom {
    Line,
    Row,
}

/// One input, in time order across all of a decoder's inputs (equal times in input order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input<'a> {
    /// Line `line` is `high` from `t` on: an edge, or, the first time a line is heard of, the level
    /// it starts with.
    Edge { line: u8, t: f64, high: bool },
    /// An annotation of the decoder below, fed as input `input`.
    Ann { input: u8, ann: &'a Ann },
    /// No data from `from` to `to`: the decoder resets and marks the gap, so nothing is decoded
    /// across it. The lines' levels at `to` follow, as their first edges.
    Gap { from: f64, to: f64 },
    /// The data is complete up to `t` inclusive: no later input is at or before it.
    End { t: f64 },
}

/// One annotation: a box from `t0` to `t1` (a mark when equal) in row `row`, of a decoder's `kind`,
/// with its `value` and [`flags`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ann {
    pub t0: f64,
    pub t1: f64,
    pub row: u8,
    pub kind: u16,
    pub value: u64,
    pub flags: u8,
}

/// [`Ann::flags`].
pub mod flags {
    /// The bus broke its rules here (a framing error, a byte cut short).
    pub const ERROR: u8 = 1;
    /// Legal but worth a look (a NACKed address).
    pub const WARN: u8 = 2;
    /// No data: the decoder was reset ([`super::Input::Gap`]).
    pub const GAP: u8 = 4;
}

/// The kind of a gap annotation, in any decoder's row 0.
pub const GAP: u16 = u16::MAX;

impl Ann {
    /// The mark a decoder leaves for a gap in its data.
    pub fn gap(from: f64, to: f64) -> Ann {
        Ann { t0: from, t1: to, row: 0, kind: GAP, value: 0, flags: flags::GAP }
    }

    pub fn is_error(&self) -> bool {
        self.flags & flags::ERROR != 0
    }
}

pub trait Decoder: Send {
    fn spec(&self) -> &'static DecoderSpec;
    /// Takes one input; what it completes goes to `out`, in time order per row.
    fn push(&mut self, input: Input, out: &mut dyn FnMut(Ann));
    /// Back to the state before any input.
    fn reset(&mut self);
    /// The text of `ann`'s box, made only when it is drawn.
    fn label(&self, ann: &Ann) -> String;
}

/// Decodes whole edge lists at once: `lines[k]` is line `k`'s entries (time, level from then on),
/// in time order, merged across lines in time order (equal times in line order), then the data's
/// end at `end`. O(edges × lines): a decoder has a few lines.
pub fn decode_edges(decoder: &mut dyn Decoder, lines: &[&[(f64, bool)]], end: f64) -> Vec<Ann> {
    let mut out = Vec::new();
    let mut at = vec![0usize; lines.len()];
    loop {
        let next = (0..lines.len()).filter(|&k| at[k] < lines[k].len())
            .min_by(|&a, &b| lines[a][at[a]].0.total_cmp(&lines[b][at[b]].0).then(a.cmp(&b)));
        let Some(k) = next else { break };
        let (t, high) = lines[k][at[k]];
        at[k] += 1;
        decoder.push(Input::Edge { line: k as u8, t, high }, &mut |ann| out.push(ann));
    }
    decoder.push(Input::End { t: end }, &mut |ann| out.push(ann));
    out
}

/// A byte as text: printable ASCII as itself, others escaped (`\x0A`).
pub fn byte_text(byte: u8) -> String {
    if (b' '..=b'~').contains(&byte) { (byte as char).to_string() } else { format!("\\x{byte:02X}") }
}
