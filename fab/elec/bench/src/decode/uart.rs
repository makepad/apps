//! UART on the decoder core: one line, frames sampled at bit centres from the start edge.
//! `crate::uart::UartDecoder` feeds it from samples; the Scope feeds it a logic trace's edges.

use super::{byte_text, Ann, Decoder, DecoderSpec, Input, InputFrom, InputSpec, flags};
use crate::uart::{FrameError, Parity, UartFormat};

pub const SPEC: DecoderSpec = DecoderSpec {
    name: "UART",
    inputs: &[InputSpec { role: "rx", from: InputFrom::Line, optional: false }],
    rows: &["frames"],
};

/// The frames row's kinds: a clean frame, then the errors ([`FrameError`]).
pub const DATA: u16 = 0;
pub const FRAMING: u16 = 1;
pub const PARITY: u16 = 2;
pub const BREAK: u16 = 3;

#[derive(Clone)]
struct Receiving {
    t0: f64,
    next: u8,
    value: u16,
    parity_error: bool,
}

/// A frame is an annotation from its start edge to the centre of its stop interval, its value the
/// data bits (LSB first); an error frame carries [`flags::ERROR`] and its error's kind.
#[derive(Clone)]
pub struct Uart {
    format: UartFormat,
    /// The line's level (after `invert`); None until it is known.
    high: Option<bool>,
    receiving: Option<Receiving>,
    /// The line has been idle (high) since the last frame: a falling edge starts one.
    idle: bool,
}

impl Uart {
    pub fn new(format: UartFormat) -> Uart {
        Uart { format, high: None, receiving: None, idle: false }
    }

    /// The frame of a frames-row annotation.
    pub fn frame(ann: &Ann) -> crate::uart::Frame {
        let error = match ann.kind {
            FRAMING => Some(FrameError::Framing),
            PARITY => Some(FrameError::Parity),
            BREAK => Some(FrameError::Break),
            _ => None,
        };
        crate::uart::Frame { t0: ann.t0, t1: ann.t1, value: ann.value as u16, error }
    }

    /// Samples the bit centres before `until` (at it too when `inclusive`) at the line's level
    /// `high`; a frame they complete goes to `out`.
    fn advance(&mut self, until: f64, high: bool, inclusive: bool, out: &mut dyn FnMut(Ann)) {
        let Some(receiving) = self.receiving.as_mut() else { return };
        let parity = u8::from(self.format.parity != Parity::None);
        let stop_index = 1 + self.format.bits + parity;
        loop {
            let index = receiving.next;
            let offset = if index == stop_index { index as f64 + self.format.stop * 0.5 } else { index as f64 + 0.5 };
            let centre = receiving.t0 + offset / self.format.baud;
            if centre > until || (!inclusive && centre == until) {
                return;
            }
            if index == 0 {
                if high {
                    // a glitch shorter than half a start bit is not a frame
                    self.receiving = None;
                    self.idle = true;
                    return;
                }
            } else if index <= self.format.bits {
                if high {
                    receiving.value |= 1 << (index - 1);
                }
            } else if index < stop_index {
                let odd = (receiving.value.count_ones() + u32::from(high)) % 2 != 0;
                receiving.parity_error = odd != (self.format.parity == Parity::Odd);
            } else {
                let kind = if !high {
                    if receiving.value == 0 { BREAK } else { FRAMING }
                } else if receiving.parity_error {
                    PARITY
                } else {
                    DATA
                };
                let flags = if kind == DATA { 0 } else { flags::ERROR };
                out(Ann { t0: receiving.t0, t1: centre, row: 0, kind, value: u64::from(receiving.value), flags });
                self.receiving = None;
                self.idle = high;
                return;
            }
            receiving.next += 1;
        }
    }
}

impl Decoder for Uart {
    fn spec(&self) -> &'static DecoderSpec {
        &SPEC
    }

    /// An edge first finishes the centres before it at the old level; a centre at an edge's time
    /// reads the new one. O(bits completed).
    fn push(&mut self, input: Input, out: &mut dyn FnMut(Ann)) {
        if !self.format.valid() {
            return;
        }
        match input {
            Input::Edge { line: 0, t, high } => {
                let high = high != self.format.invert;
                match self.high {
                    None => self.high = Some(high),
                    Some(was) if was == high => {}
                    Some(was) => {
                        self.advance(t, was, false, out);
                        self.high = Some(high);
                        if high {
                            if self.receiving.is_none() {
                                self.idle = true;
                            }
                        } else if self.receiving.is_none() && self.idle {
                            self.receiving = Some(Receiving { t0: t, next: 0, value: 0, parity_error: false });
                            self.idle = false;
                        }
                    }
                }
                if self.receiving.is_none() && self.high == Some(true) {
                    self.idle = true;
                }
            }
            Input::End { t } => {
                if let Some(high) = self.high {
                    self.advance(t, high, true, out);
                    if self.receiving.is_none() && high {
                        self.idle = true;
                    }
                }
            }
            Input::Gap { from, to } => {
                out(Ann::gap(from, to));
                self.reset();
            }
            Input::Edge { .. } | Input::Ann { .. } => {}
        }
    }

    fn reset(&mut self) {
        (self.high, self.receiving, self.idle) = (None, None, false);
    }

    fn label(&self, ann: &Ann) -> String {
        match ann.kind {
            DATA => byte_text(ann.value as u8),
            FRAMING => "framing".into(),
            PARITY => "parity".into(),
            BREAK => "break".into(),
            _ => "gap".into(),
        }
    }
}
