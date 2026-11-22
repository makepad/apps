// A 1 kHz, 30% PWM with nonuniform sample times and 20 us linear transitions.
pub fn record(end: f64, delay: f64) -> (Vec<f64>, Vec<f32>) {
    let mut t = Vec::new();
    let mut y = Vec::new();
    let mut time: f64 = 0.0;
    let mut i = 0;
    loop {
        let phase = (time - delay - 0.0002).rem_euclid(0.001);
        let value = if phase < 0.00002 { phase / 0.00002 }
            else if phase < 0.0003 { 1.0 }
            else if phase < 0.00032 { 1.0 - (phase - 0.0003) / 0.00002 }
            else { 0.0 };
        t.push(time);
        y.push(value as f32);
        if time == end { break; }
        time = (time + 0.000003 + (i % 7) as f64 * 0.0000007).min(end);
        i += 1;
    }
    (t, y)
}
