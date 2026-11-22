use fab_elec_bench::decode::{decode_edges, flags, i2c, spi, text, uart, Ann, Decoder, Input, GAP};
use fab_elec_bench::edges::Threshold;
use fab_elec_bench::uart::{Parity, UartFormat};

/// Lines' entries merged in time order (equal times in line order).
fn merged(lines: &[&[(f64, bool)]]) -> Vec<(u8, f64, bool)> {
    let mut all: Vec<(u8, f64, bool)> = lines.iter().enumerate().flat_map(|(k, line)| line.iter().map(move |&(t, high)| (k as u8, t, high))).collect();
    all.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    all
}

/// Decodes `lines` with the data's end marked after every `chunk` entries (at the last entry's
/// time, once no other line has an entry then), as a live record arrives: the annotations equal one
/// whole decode's.
fn streamed(decoder: &mut dyn Decoder, lines: &[&[(f64, bool)]], end: f64, chunk: usize) -> Vec<Ann> {
    decoder.reset();
    let mut out = Vec::new();
    let all = merged(lines);
    for (i, &(line, t, high)) in all.iter().enumerate() {
        decoder.push(Input::Edge { line, t, high }, &mut |ann| out.push(ann));
        if (i + 1) % chunk == 0 && all.get(i + 1).is_none_or(|next| next.1 > t) {
            decoder.push(Input::End { t }, &mut |ann| out.push(ann));
        }
    }
    decoder.push(Input::End { t: end }, &mut |ann| out.push(ann));
    out
}

/// A line's level at `t`: its last entry at or before it.
fn level_at(line: &[(f64, bool)], t: f64) -> bool {
    line.iter().take_while(|entry| entry.0 <= t).last().expect("a line starts at 0").1
}

fn row(anns: &[Ann], row: u8) -> Vec<Ann> {
    anns.iter().copied().filter(|ann| ann.row == row).collect()
}

fn chunkings(decoder: &mut dyn Decoder, lines: &[&[(f64, bool)]], end: f64, whole: &[Ann], what: &str) {
    for chunk in [1, 2, 3, 7, 64] {
        assert_eq!(streamed(decoder, lines, end, chunk), whole, "{what}: the data's end marked every {chunk} entries decodes the same");
    }
}

/// A UART line idle high, then `frames` (value, kind: an error kind is made so), two idle bits
/// apart; with each frame's annotation as the decoder must find it.
fn uart_line(format: &UartFormat, frames: &[(u16, u16)]) -> (Vec<(f64, bool)>, Vec<Ann>, f64) {
    let at = |bit: f64| bit / format.baud;
    let (mut entries, mut want) = (vec![(0.0, true)], Vec::new());
    let (mut cursor, mut old) = (2.0, true);
    for &(value, kind) in frames {
        let mut bits = vec![false];
        bits.extend((0..format.bits).map(|bit| value & (1 << bit) != 0));
        if format.parity != Parity::None {
            let parity = (value.count_ones() % 2 != 0) != (format.parity == Parity::Odd);
            bits.push(parity != (kind == uart::PARITY));
        }
        bits.push(kind != uart::FRAMING && kind != uart::BREAK);
        let t0 = at(cursor);
        want.push(Ann { t0, t1: t0 + (bits.len() as f64 - 1.0 + 0.5 * format.stop) / format.baud, row: 0, kind, value: u64::from(value),
            flags: if kind == uart::DATA { 0 } else { flags::ERROR } });
        for (i, &high) in bits.iter().enumerate() {
            if high != old {
                entries.push((at(cursor + i as f64), high));
                old = high;
            }
        }
        cursor += (bits.len() - 1) as f64 + format.stop;
        if !old {
            entries.push((at(cursor), true));
            old = true;
        }
        cursor += 2.0;
    }
    (entries, want, at(cursor))
}

/// UART on edges: every format's clean and broken frames exactly, an inverted line, a gap that
/// loses the frame across it, and the same frames whatever the chunking.
#[test]
fn uart_decodes_frames_and_errors_from_edges_in_any_chunking() {
    for (bits, parity, stop, invert) in [(8, Parity::None, 1.0, false), (7, Parity::Even, 1.5, true), (9, Parity::Odd, 2.0, false)] {
        let format = UartFormat { baud: 115200.0, bits, parity, stop, invert, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
        let mut frames = vec![(0x55, uart::DATA), (0x41, uart::FRAMING), (0, uart::BREAK), (0x3F, uart::DATA)];
        if parity != Parity::None {
            frames.push((0x2A, uart::PARITY));
        }
        let (mut line, want, end) = uart_line(&format, &frames);
        if invert {
            line.iter_mut().for_each(|(_, high)| *high = !*high);
        }
        let mut decoder = uart::Uart::new(format);
        let whole = decode_edges(&mut decoder, &[&line], end);
        assert_eq!(whole, want, "{bits}{parity:?}: each frame from its start edge to its stop centre, errors flagged");
        chunkings(&mut decoder, &[&line], end, &whole, &format!("UART {bits}{parity:?}"));
    }

    // a gap inside the second frame: it is lost, marked, and the third decodes
    let format = UartFormat { baud: 9600.0, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
    let (line, want, end) = uart_line(&format, &[(0x31, uart::DATA), (0x32, uart::DATA), (0x33, uart::DATA)]);
    let (from, to) = (want[1].t0 + 3.0 / format.baud, want[1].t1 + 1.0 / format.baud);
    let mut decoder = uart::Uart::new(format);
    let mut out = Vec::new();
    for &(t, high) in &line {
        if t > from && t < to {
            continue;
        }
        if t >= to && out.iter().all(|ann: &Ann| ann.kind != GAP) {
            decoder.push(Input::Gap { from, to }, &mut |ann| out.push(ann));
            decoder.push(Input::Edge { line: 0, t: to, high: level_at(&line, to) }, &mut |ann| out.push(ann));
        }
        decoder.push(Input::Edge { line: 0, t, high }, &mut |ann| out.push(ann));
    }
    decoder.push(Input::End { t: end }, &mut |ann| out.push(ann));
    assert_eq!(out, [want[0], Ann::gap(from, to), want[2]], "the frame across the gap is not decoded; the next one is");
    assert_eq!((decoder.label(&want[0]), decoder.label(&Ann { value: 10, ..want[0] })), ("1".into(), "\\x0A".into()), "a frame reads as its byte");
}

/// An I2C bus written one quarter bit at a time: SDA moves while SCL is low, SCL rises to sample.
struct Bus {
    t: u32,
    scl: Vec<(f64, bool)>,
    sda: Vec<(f64, bool)>,
}

impl Bus {
    fn new() -> Bus {
        Bus { t: 0, scl: vec![(0.0, true)], sda: vec![(0.0, true)] }
    }
    fn at(t: u32) -> f64 {
        f64::from(t) * 2.5e-6
    }
    fn set(line: &mut Vec<(f64, bool)>, t: u32, high: bool) {
        if line.last().is_some_and(|&(_, was)| was != high) {
            line.push((Bus::at(t), high));
        }
    }
    /// A start (or a repeated start, from a byte's end): its time.
    fn start(&mut self) -> f64 {
        Bus::set(&mut self.sda, self.t, true);
        Bus::set(&mut self.scl, self.t + 1, true);
        Bus::set(&mut self.sda, self.t + 2, false);
        Bus::set(&mut self.scl, self.t + 3, false);
        self.t += 4;
        Bus::at(self.t - 2)
    }
    /// `bits` of `value` MSB first, then the acknowledge unless cut: the first and last SCL rises.
    fn byte(&mut self, value: u8, bits: u32, ack: Option<bool>) -> (f64, f64) {
        let levels = (0..bits).map(|k| value >> (7 - k) & 1 == 1).chain(ack.map(|ack| !ack));
        let (mut first, mut last) = (None, 0.0);
        for high in levels {
            Bus::set(&mut self.sda, self.t, high);
            Bus::set(&mut self.scl, self.t + 1, true);
            Bus::set(&mut self.scl, self.t + 3, false);
            first.get_or_insert(Bus::at(self.t + 1));
            last = Bus::at(self.t + 1);
            self.t += 4;
        }
        (first.unwrap_or(last), last)
    }
    /// A stop: its time.
    fn stop(&mut self) -> f64 {
        Bus::set(&mut self.sda, self.t, false);
        Bus::set(&mut self.scl, self.t + 1, true);
        Bus::set(&mut self.sda, self.t + 2, true);
        self.t += 4;
        Bus::at(self.t - 2)
    }
}

fn mark(t: f64, kind: u16) -> Ann {
    Ann { t0: t, t1: t, row: i2c::CONDITIONS, kind, value: 0, flags: 0 }
}

/// I2C: a register write, a repeated start into a read whose last byte the master does not
/// acknowledge, an address nobody acknowledges, and a byte cut short by a stop, each condition,
/// byte and transaction exactly; a gap drops the transaction across it; any chunking decodes the
/// same.
#[test]
fn i2c_decodes_transactions_conditions_and_cut_bytes() {
    let mut bus = Bus::new();
    let (mut conditions, mut bytes) = (Vec::new(), Vec::new());
    let mut byte = |bus: &mut Bus, conditions: &mut Vec<Ann>, kind: u16, value: u8, ack: bool| {
        let (t0, t1) = bus.byte(value, 8, Some(ack));
        bytes.push(Ann { t0, t1, row: i2c::BYTES, kind, value: u64::from(value) | if ack { i2c::ACKED } else { 0 }, flags: 0 });
        conditions.push(mark(t1, if ack { i2c::ACK } else { i2c::NACK }));
    };
    let start = bus.start();
    conditions.push(mark(start, i2c::START));
    byte(&mut bus, &mut conditions, i2c::ADDRESS, 0x50 << 1, true);
    for value in [0x00, 0x12, 0x34] {
        byte(&mut bus, &mut conditions, i2c::DATA, value, true);
    }
    let restart = bus.start();
    conditions.push(mark(restart, i2c::RESTART));
    byte(&mut bus, &mut conditions, i2c::ADDRESS, 0x50 << 1 | 1, true);
    byte(&mut bus, &mut conditions, i2c::DATA, 0x56, true);
    byte(&mut bus, &mut conditions, i2c::DATA, 0x78, false);
    let stop = bus.stop();
    conditions.push(mark(stop, i2c::STOP));
    let lonely = bus.start();
    conditions.push(mark(lonely, i2c::START));
    byte(&mut bus, &mut conditions, i2c::ADDRESS, 0x21 << 1, false);
    let cut = bus.byte(0b1011_0000, 3, None);
    let cut_stop = bus.stop();
    conditions.push(mark(cut_stop, i2c::STOP));
    let mut bytes = bytes;
    bytes.push(Ann { t0: cut.0, t1: cut_stop, row: i2c::BYTES, kind: i2c::DATA, value: 0b101, flags: flags::ERROR });
    let transactions = [
        Ann { t0: start, t1: restart, row: i2c::TRANSACTIONS, kind: i2c::WRITE, value: 0x50 | 3 << 8, flags: 0 },
        Ann { t0: restart, t1: stop, row: i2c::TRANSACTIONS, kind: i2c::READ, value: 0x50 | 2 << 8, flags: 0 },
        Ann { t0: lonely, t1: cut_stop, row: i2c::TRANSACTIONS, kind: i2c::WRITE, value: 0x21, flags: flags::WARN },
    ];
    let end = Bus::at(bus.t);
    let lines: [&[(f64, bool)]; 2] = [&bus.scl, &bus.sda];
    let mut decoder = i2c::I2c::new();
    let whole = decode_edges(&mut decoder, &lines, end);
    assert_eq!(row(&whole, i2c::CONDITIONS), conditions, "every start, repeated start, acknowledge and stop at its time");
    assert_eq!(row(&whole, i2c::BYTES), bytes, "each address and data byte from its first to its acknowledge rise; the cut one flagged with its three bits");
    assert_eq!(row(&whole, i2c::TRANSACTIONS), transactions, "a write of three, a read of two, and an address nobody acknowledged");
    chunkings(&mut decoder, &lines, end, &whole, "I2C");
    assert_eq!([&bytes[0], &bytes[5], &transactions[1]].map(|ann| decoder.label(ann)), ["50 W", "56", "R 50 (2)"], "boxes read as address, byte and transaction");

    // a gap inside the read: the transaction across it is lost, the third one decodes
    let (from, to) = (bytes[5].t0, bytes[6].t1);
    decoder.reset();
    let mut out = Vec::new();
    let mut gapped = false;
    for (line, t, high) in merged(&lines) {
        if t >= from && t <= to {
            continue;
        }
        if t > to && !gapped {
            decoder.push(Input::Gap { from, to }, &mut |ann| out.push(ann));
            for (k, entries) in lines.iter().enumerate() {
                decoder.push(Input::Edge { line: k as u8, t: to, high: level_at(entries, to) }, &mut |ann| out.push(ann));
            }
            gapped = true;
        }
        decoder.push(Input::Edge { line, t, high }, &mut |ann| out.push(ann));
    }
    assert_eq!(row(&out, i2c::TRANSACTIONS), [transactions[0], transactions[2]], "no transaction across a gap");
    assert!(out.contains(&Ann::gap(from, to)), "the gap is marked");
}

/// SPI in all four modes and both bit orders: words on MOSI and MISO from their first to their
/// last sampling edge, a transfer per chip select, a word cut short by the chip select flagged
/// with its bits; a bus without chip select decodes its words too; any chunking decodes the same.
#[test]
fn spi_decodes_words_in_every_mode_and_order() {
    let (mosi_words, miso_words) = ([0xA5u64, 0x3C, 0xF0], [0x5Au64, 0xC3, 0x0F]);
    for cpol in [false, true] {
        for cpha in [false, true] {
            for msb_first in [true, false] {
                let format = spi::SpiFormat { cpol, cpha, bits: 8, msb_first, cs_active_low: true };
                let at = |t: u32| f64::from(t) * 1e-7;
                let mut lines: [Vec<(f64, bool)>; 4] = [vec![(0.0, cpol)], vec![(0.0, false)], vec![(0.0, false)], vec![(0.0, true)]];
                let set = |lines: &mut [Vec<(f64, bool)>; 4], line: u8, t: u32, high: bool| {
                    let line = &mut lines[line as usize];
                    if line.last().is_some_and(|&(_, was)| was != high) {
                        line.push((at(t), high));
                    }
                };
                let mut t = 2;
                set(&mut lines, spi::CS, t, false);
                let selected = at(t);
                t += 2;
                let (mut want_mosi, mut want_miso) = (Vec::new(), Vec::new());
                // two whole words, then five bits of the third
                for (w, (&mosi, &miso)) in mosi_words.iter().zip(&miso_words).enumerate() {
                    let bits = if w == 2 { 5 } else { 8 };
                    let mut samples = Vec::new();
                    for k in 0..bits {
                        let shift = if msb_first { 7 - k } else { k };
                        set(&mut lines, spi::MOSI, t, mosi >> shift & 1 == 1);
                        set(&mut lines, spi::MISO, t, miso >> shift & 1 == 1);
                        set(&mut lines, spi::SCLK, t + 1, !cpol);
                        set(&mut lines, spi::SCLK, t + 3, cpol);
                        samples.push(at(if cpha { t + 3 } else { t + 1 }));
                        t += 4;
                    }
                    let value = |word: u64| if bits == 8 { word } else if msb_first { word >> 3 } else { word & 0x1F };
                    let (t0, t1) = (samples[0], *samples.last().unwrap());
                    let (t1, flags) = if bits == 8 { (t1, 0) } else { (at(t + 1), flags::ERROR) };
                    want_mosi.push(Ann { t0, t1, row: spi::MOSI_ROW, kind: spi::WORD, value: value(mosi), flags });
                    want_miso.push(Ann { t0, t1, row: spi::MISO_ROW, kind: spi::WORD, value: value(miso), flags });
                }
                set(&mut lines, spi::CS, t + 1, true);
                let end = at(t + 4);
                let lines: Vec<&[(f64, bool)]> = lines.iter().map(Vec::as_slice).collect();
                let what = format!("SPI cpol {cpol} cpha {cpha} msb {msb_first}");
                let mut decoder = spi::Spi::new(format);
                let whole = decode_edges(&mut decoder, &lines, end);
                assert_eq!(row(&whole, spi::MOSI_ROW), want_mosi, "{what}: MOSI words");
                assert_eq!(row(&whole, spi::MISO_ROW), want_miso, "{what}: MISO words");
                assert_eq!(row(&whole, spi::TRANSFERS), [Ann { t0: selected, t1: at(t + 1), row: spi::TRANSFERS, kind: spi::TRANSFER, value: 2, flags: 0 }],
                    "{what}: one transfer of two whole words");
                chunkings(&mut decoder, &lines, end, &whole, &what);
                // the same bus without its chip select: the words, no transfers
                let bare = spi::Spi::new(format);
                let mut bare: Box<dyn Decoder> = Box::new(bare);
                let words = decode_edges(bare.as_mut(), &lines[..3], end);
                assert_eq!(row(&words, spi::MOSI_ROW)[..2], want_mosi[..2], "{what}: without a chip select the words still decode");
                assert!(row(&words, spi::TRANSFERS).is_empty(), "{what}: and there are no transfers");
            }
        }
    }
    let decoder = spi::Spi::new(spi::SpiFormat::MODE0);
    let word = Ann { t0: 0.0, t1: 1.0, row: 0, kind: spi::WORD, value: 0xA5, flags: 0 };
    assert_eq!((decoder.label(&word), decoder.label(&Ann { flags: flags::ERROR, value: 0x14, ..word })), ("A5".into(), "14?".into()), "a word reads in hex");
}

/// Text lines stacked on a UART's frames: a line from its first byte to its newline, its bytes
/// counted (a carriage return before the newline not), a bad frame flagging its line, and a gap
/// below cutting the open line.
#[test]
fn text_lines_stack_on_uart_frames() {
    let format = UartFormat { baud: 9600.0, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
    let mut frames: Vec<(u16, u16)> = b"hi\r\nok\n".iter().map(|&b| (u16::from(b), uart::DATA)).collect();
    frames.insert(4, (0x6F, uart::FRAMING));
    frames.extend(b"tail".iter().map(|&b| (u16::from(b), uart::DATA)));
    let (line, want, end) = uart_line(&format, &frames);
    let bytes = decode_edges(&mut uart::Uart::new(format), &[&line], end);
    assert_eq!(bytes, want, "the frames below");
    let mut lines = text::TextLines::new();
    let mut out = Vec::new();
    for ann in &bytes {
        lines.push(Input::Ann { input: 0, ann }, &mut |ann| out.push(ann));
    }
    let gap = Ann::gap(end, end + 1e-3);
    lines.push(Input::Ann { input: 0, ann: &gap }, &mut |ann| out.push(ann));
    assert_eq!(out, [
        Ann { t0: want[0].t0, t1: want[3].t1, row: 0, kind: text::LINE, value: 2, flags: 0 },
        Ann { t0: want[4].t0, t1: want[7].t1, row: 0, kind: text::LINE, value: 3, flags: flags::ERROR },
        Ann { t0: want[8].t0, t1: gap.t0, row: 0, kind: text::CUT, value: 4, flags: flags::WARN },
        gap,
    ], "\"hi\\r\\n\", a line with a bad frame, and the tail a gap cut");
    assert_eq!(lines.label(&out[0]), "2 bytes", "a line's box counts its bytes");
}
