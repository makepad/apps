//! Text lines, stacked on a byte row (a UART's frames): a line from its first byte to its newline.
//! Its text is the bytes below it, read from that row when it is drawn; its value is their count.

use super::{uart, Ann, Decoder, DecoderSpec, Input, InputFrom, InputSpec, flags};

pub const SPEC: DecoderSpec = DecoderSpec {
    name: "Text lines",
    inputs: &[InputSpec { role: "bytes", from: InputFrom::Row, optional: false }],
    rows: &["lines"],
};

/// A line ended by its newline: value is its bytes before the newline (a carriage return before it
/// not counted); one with a bad frame carries [`flags::ERROR`].
pub const LINE: u16 = 0;
/// The bytes a gap cut off before their newline, with [`flags::WARN`].
pub const CUT: u16 = 1;

pub struct TextLines {
    /// The open line's first byte's start, its byte count, a carriage return last, and an error.
    open: Option<(f64, u64, bool, bool)>,
}

impl TextLines {
    pub fn new() -> TextLines {
        TextLines { open: None }
    }
}

impl Default for TextLines {
    fn default() -> TextLines {
        TextLines::new()
    }
}

impl Decoder for TextLines {
    fn spec(&self) -> &'static DecoderSpec {
        &SPEC
    }

    /// O(1) per byte.
    fn push(&mut self, input: Input, out: &mut dyn FnMut(Ann)) {
        match input {
            Input::Ann { input: 0, ann } if ann.kind != super::GAP => {
                let (t0, count, cr, error) = self.open.get_or_insert((ann.t0, 0, false, false));
                let t0 = *t0;
                if ann.kind == uart::DATA && ann.value == u64::from(b'\n') {
                    let count = *count - u64::from(*cr);
                    let flags = if *error { flags::ERROR } else { 0 };
                    self.open = None;
                    out(Ann { t0, t1: ann.t1, row: 0, kind: LINE, value: count, flags });
                    return;
                }
                *error |= ann.is_error();
                *cr = ann.kind == uart::DATA && ann.value == u64::from(b'\r');
                *count += 1;
            }
            // the row below lost data: so did the open line
            Input::Ann { input: 0, ann } => self.push(Input::Gap { from: ann.t0, to: ann.t1 }, out),
            Input::Gap { from, to } => {
                if let Some((t0, count, ..)) = self.open.take() {
                    out(Ann { t0, t1: from, row: 0, kind: CUT, value: count, flags: flags::WARN });
                }
                out(Ann::gap(from, to));
            }
            Input::Ann { .. } | Input::Edge { .. } | Input::End { .. } => {}
        }
    }

    fn reset(&mut self) {
        self.open = None;
    }

    fn label(&self, ann: &Ann) -> String {
        match ann.kind {
            LINE | CUT => format!("{} bytes", ann.value),
            _ => "gap".into(),
        }
    }
}
