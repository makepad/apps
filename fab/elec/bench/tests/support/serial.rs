use fab_elec_bench::uart::{Parity, UartFormat};

pub fn record(format: UartFormat, values: &[u16], parity_error: Option<usize>, stop_error: Option<usize>) -> (Vec<f64>, Vec<f32>, Vec<f64>) {
    let mut transitions = Vec::new();
    let mut starts = Vec::new();
    let mut cursor = 2.0;
    let mut old = true;
    for (frame, &value) in values.iter().enumerate() {
        starts.push(cursor / format.baud);
        let mut bits = vec![false];
        for bit in 0..format.bits { bits.push(value & (1 << bit) != 0); }
        if format.parity != Parity::None {
            let mut parity = (value.count_ones() % 2 != 0) != (format.parity == Parity::Odd);
            if parity_error == Some(frame) { parity = !parity; }
            bits.push(parity);
        }
        bits.push(stop_error != Some(frame));
        for (i, high) in bits.iter().copied().enumerate() {
            if high != old { transitions.push((cursor + i as f64, high)); old = high; }
        }
        cursor += (bits.len() - 1) as f64 + format.stop;
        if !old { transitions.push((cursor, true)); old = true; }
        cursor += 2.0;
    }
    let mut t = Vec::new();
    let mut y = Vec::new();
    let mut time: f64 = 0.0;
    let mut i = 0;
    let mut edge = 0;
    let mut high = true;
    while time <= cursor {
        while edge < transitions.len() && time > transitions[edge].0 + 0.03 {
            high = transitions[edge].1;
            edge += 1;
        }
        let mut value = if high { 1.0 } else { 0.0 };
        if edge < transitions.len() && time >= transitions[edge].0 - 0.03 {
            let ramp = (time - transitions[edge].0 + 0.03) / 0.06;
            value = if transitions[edge].1 { ramp } else { 1.0 - ramp };
        }
        value += 0.008 * (i as f64 * 1.7).sin();
        if format.invert { value = 1.0 - value; }
        t.push(time / format.baud);
        y.push(value as f32);
        time += 0.025 + (i % 5) as f64 * 0.006;
        i += 1;
    }
    (t, y, starts)
}
