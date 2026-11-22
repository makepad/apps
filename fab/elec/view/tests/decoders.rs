use fab_elec_bench::decode::{decode_edges, spi, uart, Ann};
use fab_elec_bench::edges::Threshold;
use fab_elec_bench::uart::{Parity, UartFormat};
use fab_elec_sim::trace::{LogicEdge, LogicPage, SignalId, TraceStore, LOGIC_CHUNK};
use fab_elec_view::instruments::decoders::{Decoded, Decoders, Piece, Protocol, Row, Setup};

type Line = Vec<(f64, bool)>;

fn set(line: &mut Line, t: f64, high: bool) {
    if line.last().is_none_or(|&(_, was)| was != high) {
        line.push((t, high));
    }
}

/// A shift chain's bus as drum16 drives it (SPI mode 0, 24 bits MSB first, the latch low while
/// shifting and pulsed high after each word), and a UART line at 115200 sending a short text.
fn bus(transfers: u32) -> ([Line; 3], Line, Vec<u32>) {
    let q = 1e-6;
    let (mut sclk, mut mosi, mut cs) = (vec![(0.0, false)], vec![(0.0, false)], vec![(0.0, false)]);
    let mut words = Vec::new();
    let mut t = 4.0 * q;
    for n in 0..transfers {
        let word = n.wrapping_mul(0x9E37_79B1) & 0xFF_FFFF;
        words.push(word);
        for i in 0..24 {
            set(&mut mosi, t, word >> (23 - i) & 1 == 1);
            set(&mut sclk, t + q, true);
            set(&mut sclk, t + 3.0 * q, false);
            t += 4.0 * q;
        }
        set(&mut cs, t + q, true);
        set(&mut cs, t + 2.0 * q, false);
        t += 4.0 * q;
    }
    let bit = 1.0 / 115200.0;
    let mut rx = vec![(0.0, true)];
    let mut at = 3e-4;
    for &byte in b"Hello, scope\n" {
        let bits = std::iter::once(false).chain((0..8).map(|k| byte >> k & 1 == 1)).chain(std::iter::once(true));
        for (k, high) in bits.enumerate() {
            set(&mut rx, at + k as f64 * bit, high);
        }
        at += 12.0 * bit;
    }
    ([sclk, mosi, cs], rx, words)
}

/// The record up to `to`: each line's entries up to it, then a sample at it (the record's head).
fn grow(store: &mut TraceStore, lines: &[&Line], cursors: &mut [usize], to: f64) {
    for (k, line) in lines.iter().enumerate() {
        while cursors[k] < line.len() && line[cursors[k]].0 <= to {
            let (t, high) = line[cursors[k]];
            store.push_logic(SignalId(k as u32), LogicEdge { t, high }, &mut Vec::new());
            cursors[k] += 1;
        }
    }
    store.push(to, &[0.0; 4], &mut Vec::new());
}

fn anns(row: &Row) -> Vec<Ann> {
    row.sealed.iter().flat_map(|chunk| chunk.iter().copied()).chain(row.tail.iter().copied()).collect()
}

fn rows_of(decoded: &Decoded, id: u64) -> Vec<(u8, Vec<Ann>)> {
    decoded.get(id).expect("published").rows.iter().map(|row| (row.row, anns(row))).collect()
}

fn reference(decoder: &mut dyn fab_elec_bench::decode::Decoder, lines: &[&[(f64, bool)]], end: f64, rows: &[u8]) -> Vec<(u8, Vec<Ann>)> {
    let all = decode_edges(decoder, lines, end);
    rows.iter().map(|&row| (row, all.iter().copied().filter(|ann| ann.row == row).collect())).collect()
}

/// The Scope's decode worker state over a record that grows: fed in small slices at every
/// growth, each decoder's rows equal one whole decode of the same edges; options left to the
/// record are found (the chain's 24 bits from its latch, the UART's 115200 baud); a spilled chunk
/// decodes as a gap and the words after the next latch are exact again; a new generation starts
/// over; a row's pieces walk only the window and draw a too-dense chunk from its summary.
#[test]
fn scope_decoders_follow_a_growing_record_exactly() {
    let ([sclk, mosi, cs], rx, words) = bus(400);
    let lines: [&Line; 4] = [&sclk, &mosi, &cs, &rx];
    let end = sclk.last().unwrap().0.max(cs.last().unwrap().0) + 1e-6;
    let spi_lines = vec![Some(SignalId(0)), Some(SignalId(1)), None, Some(SignalId(2))];
    let setups = [
        Setup { id: 1, protocol: Protocol::Spi { bits: None, mode: 0, msb_first: true }, lines: spi_lines.clone() },
        Setup { id: 2, protocol: Protocol::Uart { baud: None }, lines: vec![Some(SignalId(3))] },
        Setup { id: 3, protocol: Protocol::Spi { bits: Some(8), mode: 0, msb_first: true }, lines: spi_lines },
    ];
    let mut store = TraceStore::new(7, &[false; 4]);
    let mut cursors = [0; 4];
    let mut decoders = Decoders::default();
    let mut rounds = 0;
    for fraction in [0.004, 0.1, 0.35, 0.36, 0.8, 1.0] {
        grow(&mut store, &lines, &mut cursors, end * fraction);
        loop {
            rounds += 1;
            let (_, more) = decoders.advance(&store, &setups, 37);
            if !more {
                break;
            }
        }
    }
    assert!(rounds > 100, "the slices were small: {rounds} rounds");
    let decoded = decoders.published();
    let found = |id| decoded.get(id).and_then(|one| one.found);
    assert_eq!(found(1).map(|f| f.bits), Some(24), "the chain's word is the clocks of a latch's transfer");
    assert_eq!(found(2).map(|f| f.baud), Some(115200.0), "the UART's baud is measured from its edges");
    let none: &[(f64, bool)] = &[];
    let spi_ref = |bits| {
        let mut decoder = spi::Spi::new(spi::SpiFormat { cpol: false, cpha: false, bits, msb_first: true, cs_active_low: true });
        reference(&mut decoder, &[&sclk, &mosi, none, &cs], end, &[spi::MOSI_ROW, spi::TRANSFERS])
    };
    let chain = rows_of(&decoded, 1);
    assert_eq!(chain, spi_ref(24), "the 24-bit chain decodes as one whole pass does");
    let values: Vec<u32> = chain[0].1.iter().map(|ann| ann.value as u32).collect();
    assert_eq!(values, words, "every word shifted out, in order");
    let format = UartFormat { baud: 115200.0, bits: 8, parity: Parity::None, stop: 1.0, invert: false, threshold: Threshold { level: 0.5, hysteresis: 0.1 } };
    let text = rows_of(&decoded, 2);
    assert_eq!(text, reference(&mut uart::Uart::new(format), &[&rx], end, &[0]), "the UART decodes as one whole pass does");
    assert_eq!(text[0].1.iter().map(|ann| ann.value as u8).collect::<Vec<_>>(), b"Hello, scope\n", "and reads the text");
    let bytes = rows_of(&decoded, 3);
    assert_eq!(bytes, spi_ref(8), "8-bit words of the same bus decode exactly");

    // pieces: the bytes row holds a sealed chunk and a tail
    let one = decoded.get(3).unwrap();
    let row = &one.rows[0];
    assert!(row.sealed.len() == 1 && !row.tail.is_empty(), "{} sealed chunks", row.sealed.len());
    let mut pieces = Vec::new();
    row.pieces(0.0, end, end / 100.0, &mut pieces);
    assert!(matches!(pieces[..], [Piece::Dense(sealed), Piece::Dense(tail)] if sealed.count as usize == 1024 && tail.count as usize == row.tail.len()),
        "a window too narrow for its boxes draws each chunk from its summary: {} pieces", pieces.len());
    let (w0, w1) = (end * 0.4, end * 0.41);
    pieces.clear();
    row.pieces(w0, w1, 0.0, &mut pieces);
    let shown: Vec<Ann> = bytes[0].1.iter().copied().filter(|ann| ann.t1 >= w0 && ann.t0 <= w1).collect();
    assert_eq!(pieces, shown.iter().map(|&ann| Piece::Box(ann)).collect::<Vec<_>>(), "a zoomed window walks only its own boxes");

    // a spilled clock chunk: a gap, then exact words from the next latch on
    let gap_end = sclk[LOGIC_CHUNK - 1].0;
    assert!(store.page_logic(&LogicPage { id: SignalId(0), first: sclk[0].0, chunk: None }), "the clock's first chunk is away");
    let mut spilled = Decoders::default();
    while spilled.advance(&store, &setups[..1], 1 << 20).1 {}
    let decoded = spilled.published();
    let words_row = &rows_of(&decoded, 1)[0].1;
    assert_eq!(words_row[0], Ann::gap(sclk[0].0, gap_end), "the spilled chunk is a gap");
    let latch = cs.iter().find(|&&(t, high)| t > gap_end && !high).unwrap().0;
    let after = |row: &[Ann]| row.iter().copied().filter(|ann| ann.t0 > latch).collect::<Vec<_>>();
    assert_eq!(after(words_row), after(&chain[0].1), "after the next latch every word is exact again");

    // a new generation starts over
    let fresh = TraceStore::new(8, &[false; 4]);
    decoders.advance(&fresh, &setups, 1 << 20);
    let decoded = decoders.published();
    assert!(decoded.decoders.iter().all(|one| one.generation == 8 && one.rows.iter().all(Row::is_empty)), "a new record decodes from its start");
}
