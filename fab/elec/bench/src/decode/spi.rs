//! SPI on the decoder core, from the bus's usual description: the clock idles at `cpol`; data is
//! sampled on its leading edge (`cpha` false) or its trailing edge (true), MOSI and MISO alike,
//! `bits` to a word in either bit order. A chip select, when the bus has one, frames transfers and
//! restarts the word count; without one the words run on from the first sampled bit.

use super::{Ann, Decoder, DecoderSpec, Input, InputFrom, InputSpec, flags};

pub const SPEC: DecoderSpec = DecoderSpec {
    name: "SPI",
    inputs: &[
        InputSpec { role: "sclk", from: InputFrom::Line, optional: false },
        InputSpec { role: "mosi", from: InputFrom::Line, optional: false },
        InputSpec { role: "miso", from: InputFrom::Line, optional: true },
        InputSpec { role: "cs", from: InputFrom::Line, optional: true },
    ],
    rows: &["mosi", "miso", "transfers"],
};

pub const SCLK: u8 = 0;
pub const MOSI: u8 = 1;
pub const MISO: u8 = 2;
pub const CS: u8 = 3;

/// Rows.
pub const MOSI_ROW: u8 = 0;
pub const MISO_ROW: u8 = 1;
pub const TRANSFERS: u8 = 2;

/// A word, from its first sampling edge to its last; one cut short by the chip select carries
/// [`flags::ERROR`] and the bits it had.
pub const WORD: u16 = 0;
/// A transfer, chip select active to inactive: value is its whole words' count.
pub const TRANSFER: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpiFormat {
    /// The clock's idle level.
    pub cpol: bool,
    /// Sample on the trailing clock edge (else the leading one).
    pub cpha: bool,
    /// Bits a word, 1 to 64.
    pub bits: u8,
    pub msb_first: bool,
    /// The chip select selects when low.
    pub cs_active_low: bool,
}

impl SpiFormat {
    /// Mode 0, eight bits MSB first, chip select active low: the common setting.
    pub const MODE0: SpiFormat = SpiFormat { cpol: false, cpha: false, bits: 8, msb_first: true, cs_active_low: true };

    fn valid(&self) -> bool {
        (1..=64).contains(&self.bits)
    }
}

struct Word {
    t0: f64,
    t1: f64,
    bits: u8,
    mosi: u64,
    miso: u64,
}

pub struct Spi {
    format: SpiFormat,
    levels: [Option<bool>; 4],
    word: Option<Word>,
    /// The open transfer's start and whole words.
    transfer: Option<(f64, u64)>,
}

impl Spi {
    pub fn new(format: SpiFormat) -> Spi {
        Spi { format, levels: [None; 4], word: None, transfer: None }
    }

    /// Selected: the chip select is active, or the bus has none (its level never heard of).
    fn selected(&self) -> bool {
        self.levels[CS as usize].is_none_or(|cs| cs != self.format.cs_active_low)
    }

    fn emit(&self, word: &Word, flags: u8, out: &mut dyn FnMut(Ann)) {
        out(Ann { t0: word.t0, t1: word.t1, row: MOSI_ROW, kind: WORD, value: word.mosi, flags });
        // a bus without MISO has no MISO row
        if self.levels[MISO as usize].is_some() {
            out(Ann { t0: word.t0, t1: word.t1, row: MISO_ROW, kind: WORD, value: word.miso, flags });
        }
    }

    /// A sampling edge at `t`.
    fn sample(&mut self, t: f64, out: &mut dyn FnMut(Ann)) {
        let bit = |line: u8| u64::from(self.levels[line as usize] == Some(true));
        let (mosi, miso) = (bit(MOSI), bit(MISO));
        let format = self.format;
        let word = self.word.get_or_insert(Word { t0: t, t1: t, bits: 0, mosi: 0, miso: 0 });
        if format.msb_first {
            (word.mosi, word.miso) = (word.mosi << 1 | mosi, word.miso << 1 | miso);
        } else {
            (word.mosi, word.miso) = (word.mosi | mosi << word.bits, word.miso | miso << word.bits);
        }
        (word.bits, word.t1) = (word.bits + 1, t);
        if word.bits == format.bits {
            let word = self.word.take().expect("the word just filled");
            self.emit(&word, 0, out);
            if let Some((_, words)) = self.transfer.as_mut() {
                *words += 1;
            }
        }
    }
}

impl Decoder for Spi {
    fn spec(&self) -> &'static DecoderSpec {
        &SPEC
    }

    /// O(1) per input.
    fn push(&mut self, input: Input, out: &mut dyn FnMut(Ann)) {
        if !self.format.valid() {
            return;
        }
        match input {
            Input::Edge { line, t, high } if (line as usize) < self.levels.len() => {
                let was = self.levels[line as usize].replace(high);
                let Some(was) = was.filter(|&was| was != high) else {
                    // a line's first level: a chip select found active opens a transfer
                    if line == CS && was.is_none() && self.selected() {
                        self.transfer = Some((t, 0));
                    }
                    return;
                };
                match line {
                    SCLK => {
                        // leading: away from the idle level
                        let leading = was == self.format.cpol;
                        if leading != self.format.cpha && self.selected() {
                            self.sample(t, out);
                        }
                    }
                    CS if self.selected() => {
                        self.word = None;
                        self.transfer = Some((t, 0));
                    }
                    CS => {
                        if let Some(word) = self.word.take() {
                            let word = Word { t1: t, ..word };
                            self.emit(&word, flags::ERROR, out);
                        }
                        if let Some((t0, words)) = self.transfer.take() {
                            out(Ann { t0, t1: t, row: TRANSFERS, kind: TRANSFER, value: words, flags: 0 });
                        }
                    }
                    _ => {}
                }
            }
            Input::Gap { from, to } => {
                out(Ann::gap(from, to));
                self.reset();
            }
            Input::Edge { .. } | Input::Ann { .. } | Input::End { .. } => {}
        }
    }

    fn reset(&mut self) {
        *self = Spi::new(self.format);
    }

    fn label(&self, ann: &Ann) -> String {
        match ann.kind {
            WORD if ann.is_error() => format!("{:0w$X}?", ann.value, w = usize::from(self.format.bits.div_ceil(4))),
            WORD => format!("{:0w$X}", ann.value, w = usize::from(self.format.bits.div_ceil(4))),
            TRANSFER => format!("{} words", ann.value),
            _ => "gap".into(),
        }
    }
}
