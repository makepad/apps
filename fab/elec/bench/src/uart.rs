//! UART frames from the same threshold edges used by the timing measures.

use crate::edges::{EdgeTracker, Threshold};
use crate::measure::Slope;

/// Rate inferred from consecutive edges in one continuous stretch of a line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BaudEstimate {
    /// Observed bits per second, without snapping to a standard rate.
    pub measured: f64,
    /// Nearest standard rate, when it is within three percent of the measurement.
    pub nominal: Option<f64>,
    /// Number of bit times contributing to the refined estimate.
    pub bits: usize,
}

/// Both directions' confirmed threshold crossings, using the decoder's edge definition.
pub fn edge_times(t: &[f64], y: &[f32], threshold: Threshold) -> Vec<f64> {
    if t.len() != y.len() { return Vec::new(); }
    let mut line = EdgeTracker::new(threshold);
    t.iter().zip(y).filter_map(|(&t, &y)| line.push(t, y as f64).map(|edge| edge.t)).collect()
}

/// Estimate from at least eight finite, positive edge intervals. Long idle gaps
/// are excluded from refinement; selection of the short-interval median is O(edges).
pub fn estimate_baud(edges: &[f64]) -> Option<BaudEstimate> {
    if edges.len() < 9 || edges.iter().any(|t| !t.is_finite()) { return None; }
    let intervals: Vec<_> = edges.windows(2).map(|p| p[1] - p[0]).collect();
    if intervals.iter().any(|dt| !dt.is_finite() || *dt <= 0.0) { return None; }
    let shortest = intervals.iter().copied().fold(f64::INFINITY, f64::min);
    let mut short: Vec<_> = intervals.iter().copied().filter(|dt| *dt <= 1.5 * shortest).collect();
    let mid = short.len() / 2;
    let even = short.len() % 2 == 0;
    let (left, median, _) = short.select_nth_unstable_by(mid, f64::total_cmp);
    let b0 = if even { *median * 0.5 + left.iter().copied().fold(0.0, f64::max) * 0.5 } else { *median };
    let mut duration = 0.0;
    let mut bits = 0_usize;
    for dt in intervals {
        if dt > 12.0 * b0 { continue; }
        duration += dt;
        bits = bits.checked_add((dt / b0).round() as usize)?;
    }
    let measured = bits as f64 / duration;
    if !duration.is_finite() || !measured.is_finite() || measured <= 0.0 { return None; }
    let nominal = [300.0_f64, 600.0, 1200.0, 2400.0, 4800.0, 9600.0, 14400.0, 19200.0,
        28800.0, 38400.0, 57600.0, 76800.0, 115200.0, 230400.0, 250000.0, 460800.0,
        500000.0, 921600.0, 1000000.0, 2000000.0, 3000000.0].into_iter()
        .min_by(|a, b| (a - measured).abs().total_cmp(&(b - measured).abs()))
        .filter(|rate| (rate - measured).abs() <= measured * 0.03);
    Some(BaudEstimate { measured, nominal, bits })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Parity { None, Even, Odd }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UartFormat {
    pub baud: f64,
    pub bits: u8,
    pub parity: Parity,
    pub stop: f64,
    pub invert: bool,
    pub threshold: Threshold,
}

impl UartFormat {
    pub(crate) fn valid(self) -> bool {
        self.baud.is_finite() && self.baud > 0.0 && (5..=9).contains(&self.bits)
            && matches!(self.stop, 1.0 | 1.5 | 2.0) && self.threshold.valid()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameError { Framing, Parity, Break }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub t0: f64,
    /// Centre of the stop interval, in seconds.
    pub t1: f64,
    pub value: u16,
    pub error: Option<FrameError>,
}

/// UART frames from samples: the shared threshold edges ([`EdgeTracker`]) fed to the decoder
/// core's UART ([`crate::decode::uart::Uart`]).
#[derive(Clone)]
pub struct UartDecoder {
    format: UartFormat,
    line: EdgeTracker,
    core: crate::decode::uart::Uart,
    previous: Option<f64>,
}

impl UartDecoder {
    pub fn new(format: UartFormat) -> Self {
        Self { format, line: EdgeTracker::new(format.threshold), core: crate::decode::uart::Uart::new(format), previous: None }
    }

    /// Feed samples in time order. A frame completes at the centre of its stop
    /// interval. Centres use the last edge at or before them, even when the next
    /// sample arrives after both the centre and an edge.
    pub fn push(&mut self, t: f64, y: f64) -> Option<Frame> {
        use crate::decode::{Decoder, Input};
        if !self.format.valid() || !t.is_finite() || !y.is_finite()
            || self.previous.is_some_and(|old| t <= old) {
            return None;
        }
        self.previous = Some(t);
        let before = self.line.high();
        let edge = self.line.push(t, y);
        let mut result = None;
        let mut take = |ann: crate::decode::Ann| result = Some(crate::decode::uart::Uart::frame(&ann));
        match (before, edge) {
            // the line's level is known from its first decisive sample
            (None, _) => if let Some(high) = self.line.high() {
                self.core.push(Input::Edge { line: 0, t, high }, &mut take);
            },
            (Some(_), Some(edge)) => self.core.push(Input::Edge { line: 0, t: edge.t, high: edge.dir == Slope::Rising }, &mut take),
            (Some(_), None) => {}
        }
        // A not-yet-confirmed crossing can still be backdated. Wait for its
        // confirmation or cancellation before resolving centres beyond it.
        let until = self.line.pending().map_or(t, |edge| edge.t.next_down());
        self.core.push(Input::End { t: until }, &mut take);
        result
    }

    pub fn reset(&mut self) {
        use crate::decode::Decoder;
        self.line.reset();
        self.core.reset();
        self.previous = None;
    }
}

pub fn decode(t: &[f64], y: &[f32], format: UartFormat) -> Vec<Frame> {
    if t.len() != y.len() { return Vec::new(); }
    let mut decoder = UartDecoder::new(format);
    t.iter().zip(y).filter_map(|(&t, &y)| decoder.push(t, y as f64)).collect()
}

/// Printable ASCII is literal; other byte values are escaped and bad frames are U+FFFD.
/// For a nine-bit format, text uses the low eight bits of each frame.
pub fn text(frames: &[Frame]) -> String {
    let mut text = String::new();
    for &frame in frames { append_text(&mut text, frame); }
    text
}

pub(crate) fn append_text(text: &mut String, frame: Frame) {
    use std::fmt::Write;
    if frame.error.is_some() {
        text.push('\u{FFFD}');
    } else {
        let byte = frame.value as u8;
        if (b' '..=b'~').contains(&byte) {
            text.push(byte as char);
        } else {
            let _ = write!(text, "\\x{byte:02X}");
        }
    }
}
