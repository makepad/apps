//! Labelled axes, independent of drawing and widget state. Measurements are along the axis:
//! use text widths for a horizontal axis and line heights for a vertical one.

/// One tick, positioned in pixels from the low end of the axis. An empty label is a log tick
/// whose rule is retained but whose label did not fit.
#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub value: f64,
    pub at: f64,
    pub label: String,
    pub major: bool,
}

/// Linear ticks at the smallest representable 1, 2 or 5 × 10^k step whose centred labels fit.
/// Zero and every fifth step are major. Invalid or empty windows produce no ticks.
/// `measure` may cache text layouts; it measures the label's extent along the axis.
pub fn linear_ticks(lo: f64, hi: f64, length: f64, gap: f64, format: &dyn Fn(f64, f64) -> String, measure: &mut dyn FnMut(&str) -> f64) -> Vec<Tick> {
    let span = hi - lo;
    if !lo.is_finite() || !hi.is_finite() || !span.is_finite() || span <= 0.0
        || !length.is_finite() || length <= 0.0 || !gap.is_finite() || gap < 0.0 { return Vec::new(); }
    // Below this bound, any pair is closer than the gap (unless fewer than two ticks fit).
    // The precision bound also ensures that incrementing an index makes progress.
    let minimum = (span * (gap / length).min(0.5)).max(lo.abs().max(hi.abs()) * f64::EPSILON).max(f64::MIN_POSITIVE);
    let mut decade = 10.0_f64.powf(minimum.log10().floor());
    if decade == 0.0 { decade = f64::MIN_POSITIVE; }
    while decade.is_finite() {
        for multiplier in [1.0, 2.0, 5.0] {
            let step = multiplier * decade;
            if !step.is_finite() { break; }
            let quotient = lo / step;
            let rounded = quotient.round();
            let mut index = if (quotient - rounded).abs() <= 4.0 * f64::EPSILON * quotient.abs() { rounded } else { quotient.ceil() };
            let tolerance = lo.abs().max(hi.abs()).max(step) * 4.0 * f64::EPSILON;
            let mut ticks = Vec::new();
            let mut previous: Option<(f64, f64)> = None;
            let mut fits = true;
            loop {
                let value = index * step;
                if !value.is_finite() || value > hi + tolerance { break; }
                let value = if value == 0.0 { 0.0 } else { value };
                let at = ((value - lo) / span * length).clamp(0.0, length);
                let label = format(value, step);
                let width = measure(&label).max(0.0);
                if let Some((last_at, last_width)) = previous {
                    if at - last_at < (last_width + width) * 0.5 + gap {
                        fits = false;
                        break;
                    }
                }
                previous = Some((at, width));
                ticks.push(Tick { value, at, label, major: index.rem_euclid(5.0) == 0.0 });
                let next = index + 1.0;
                if next <= index { fits = false; break; }
                index = next;
            }
            if fits { return ticks; }
        }
        decade *= 10.0;
    }
    Vec::new()
}

/// Log ticks at 1, 2 and 5 per decade. Every tick is retained; labels compete for space in
/// decade, 5, then 2 order, from low to high within each priority. Decades are major.
pub fn log_ticks(lo: f64, hi: f64, length: f64, gap: f64, format: &dyn Fn(f64, f64) -> String, measure: &mut dyn FnMut(&str) -> f64) -> Vec<Tick> {
    if !lo.is_finite() || !hi.is_finite() || lo <= 0.0 || hi <= lo
        || !length.is_finite() || length <= 0.0 || !gap.is_finite() || gap < 0.0 { return Vec::new(); }
    let log_lo = lo.log10();
    let log_span = hi.log10() - log_lo;
    let mut ticks = Vec::new();
    let mut occupied: Vec<(f64, f64)> = Vec::new();
    for multiplier in [1.0, 5.0, 2.0] {
        for exponent in log_lo.floor() as i32..=hi.log10().floor() as i32 {
            let decade = 10.0_f64.powi(exponent);
            let value = multiplier * decade;
            let tolerance = value * 4.0 * f64::EPSILON;
            if value < lo - tolerance || value > hi + tolerance || value == 0.0 { continue; }
            let at = ((value.log10() - log_lo) / log_span * length).clamp(0.0, length);
            let mut label = format(value, decade);
            let half = measure(&label).max(0.0) * 0.5;
            if occupied.iter().any(|&(other, width)| (at - other).abs() < half + width + gap) {
                label.clear();
            } else {
                occupied.push((at, half));
            }
            ticks.push(Tick { value, at, label, major: multiplier == 1.0 });
        }
    }
    ticks.sort_by(|a, b| a.value.total_cmp(&b.value));
    ticks
}

/// One SI prefix for an entire axis. The largest magnitude is in (1, 1000], so a window
/// ending at exactly 1 V uses mV (and 200 mV steps remain whole numbers). Zero has no prefix.
pub fn axis_prefix(lo: f64, hi: f64, unit: &str) -> (f64, String) {
    let largest = lo.abs().max(hi.abs());
    if !largest.is_finite() || largest == 0.0 { return (1.0, unit.into()); }
    let group = (largest.log10() / 3.0).ceil() as i32 - 1;
    let group = group.clamp(-10, 10);
    (10.0_f64.powi(group * 3), format!("{}{unit}", PREFIXES[(group + 10) as usize]))
}

const PREFIXES: [&str; 21] = ["q", "r", "y", "z", "a", "f", "p", "n", "µ", "m", "", "k", "M", "G", "T", "P", "E", "Z", "Y", "R", "Q"];

/// One value with its own prefix, its mantissa in [1, 1000) and at most four significant digits,
/// trailing zeroes removed: a log axis's labels (10 kHz, 50 kHz, 1 MHz), where one prefix cannot fit
/// every decade.
pub fn si_label(value: f64, unit: &str) -> String {
    let mut text = String::new();
    write_si_label(&mut text, value, unit);
    text
}

/// Append the same answer formatting to a retained overlay label, without temporary Strings.
pub(super) fn write_si_label(text: &mut String, value: f64, unit: &str) {
    use std::fmt::Write;
    if value == 0.0 || !value.is_finite() { let _ = write!(text, "0 {unit}"); return; }
    // Percentages and logarithmic units are already scaled quantities: 0.25 % and -0.1 dBV
    // must never become 250 m% or -100 mdBV in an answer.
    let fixed = unit == "%" || unit == "dBV";
    let mut group = if fixed { 0 } else { ((value.abs().log10() / 3.0).floor() as i32).clamp(-10, 10) };
    let mut scaled = value.abs() / 10.0_f64.powi(group * 3);
    if !fixed && scaled >= 999.95 && group < 10 { group += 1; scaled /= 1000.0; }
    let decimals = (3.0 - scaled.log10().floor()).clamp(0.0, 15.0) as usize;
    if value < 0.0 { text.push('−'); }
    let _ = write!(text, "{:.*}", decimals, scaled);
    if decimals > 0 {
        while text.ends_with('0') { text.pop(); }
        if text.ends_with('.') { text.pop(); }
    }
    let _ = write!(text, " {}{unit}", PREFIXES[(group + 10) as usize]);
}

/// Just enough decimal places for the step, with trailing zeroes removed and a typographic
/// minus. `scale` is the multiplier returned by `axis_prefix`; zero never has a sign.
pub fn axis_label(value: f64, step: f64, scale: f64) -> String {
    if value == 0.0 { return "0".into(); }
    let scaled = value / scale;
    let step = (step / scale).abs();
    let mut decimals = if step.is_finite() && step > 0.0 { (-step.log10()).ceil().max(0.0) as usize } else { 0 };
    // Non-menu steps (for example 0.25) are useful to callers too.
    while decimals < 15 && (step * 10.0_f64.powi(decimals as i32) - (step * 10.0_f64.powi(decimals as i32)).round()).abs() > 1e-8 { decimals += 1; }
    let mut text = format!("{:.*}", decimals, scaled.abs());
    if text.contains('.') {
        while text.ends_with('0') { text.pop(); }
        if text.ends_with('.') { text.pop(); }
    }
    if scaled < 0.0 && text != "0" { text.insert(0, '−'); }
    text
}

/// Trigger-relative time labels carry their own unit and an explicit sign away from zero.
pub fn time_label(value: f64, step: f64, scale: f64, unit: &str) -> String {
    let label = axis_label(value, step, scale);
    if label == "0" { label } else { format!("{}{label} {unit}", if value > 0.0 { "+" } else { "" }) }
}
