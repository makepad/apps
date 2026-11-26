//! Axis ticks, number formats, text measure estimates and label spreading:
//! the pure layout arithmetic every chart kind shares.

/// A Text's `size` is in points: its em is `size * PX` document units.
/// Charts lay out in the em they see (`Chart::ls` is one) and hand Text
/// layers `em / PX`.
pub(crate) const PX: f64 = 4.0 / 3.0;

/// "Nice" ticks (steps of 1, 2, 2.5 or 5 × 10^k) covering `lo..hi`, about
/// `target` of them.
pub(crate) fn nice_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    let (lo, hi) = if hi - lo < 1e-12 { (lo - 0.5, hi + 0.5) } else { (lo, hi) };
    let raw = (hi - lo) / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw * 0.999).unwrap_or(10.0 * mag);
    let first = (lo / step + 1e-9).floor() * step;
    let mut out = Vec::new();
    let mut k = 0;
    loop {
        let v = first + step * k as f64;
        // Snap away float dust (0.30000000000000004).
        let v = (v / step).round() * step;
        out.push(if v.abs() < step * 1e-9 { 0.0 } else { v });
        if v >= hi - step * 1e-9 || k > 50 {
            break;
        }
        k += 1;
    }
    out
}

/// How numbers read: `@plain` (grouped), `@compact` (1.2M), `@currency`
/// ($1.2M), `@percent` (42%), with `decimals`, `prefix`, `suffix`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NumFmt {
    pub kind: String,
    pub decimals: Option<u32>,
    pub prefix: String,
    pub suffix: String,
    pub currency: String,
    /// `@seconds`' total (`3.5 / 15.0 s`).
    pub total: Option<f64>,
}

impl Default for NumFmt {
    fn default() -> Self {
        NumFmt { kind: "plain".into(), decimals: None, prefix: String::new(), suffix: String::new(), currency: "$".into(), total: None }
    }
}

/// What a count-up Text shows: `number` × `div`, with these fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub div: f64,
    pub decimals: u32,
    pub prefix: String,
    pub suffix: String,
    /// The compact unit at the start of `suffix` ("M"); a zero drops it.
    pub unit: String,
    /// A time format (`@mmss`, `@timecode`, `@hhmmss`, `@seconds`): the
    /// value is seconds, read by Motion's `format_time`.
    pub time: Option<String>,
    /// `@seconds`' total.
    pub total: Option<f64>,
}

/// A time format's id as Motion's [`crate::TimeFormat`].
pub(crate) fn time_format(id: &str, total: Option<f64>) -> Option<crate::TimeFormat> {
    Some(match id {
        "timecode" => crate::TimeFormat::Timecode,
        "mmss" => crate::TimeFormat::MmSs,
        "hhmmss" => crate::TimeFormat::HhMmSs,
        "seconds" => crate::TimeFormat::Seconds { total },
        // Grows through grouped, named, scientific (M1's format_escalate).
        "escalate" => crate::TimeFormat::Escalate,
        _ => return None,
    })
}

impl Shown {
    pub fn text(&self, v: f64) -> String {
        if let Some(format) = self.time.as_deref().and_then(|t| time_format(t, self.total)) {
            return crate::format_time(v, format, 30.0, self.decimals);
        }
        if v == 0.0 {
            return format!("{}0{}", self.prefix, self.suffix.strip_prefix(self.unit.as_str()).unwrap_or(&self.suffix));
        }
        format!("{}{}{}", self.prefix, crate::format_number(v / self.div, self.decimals, Some(",")), self.suffix)
    }
}

impl NumFmt {
    /// The unit, decimals and affixes for values up to `max_abs` (every
    /// label of one scale shares them, so they read alike). `step` is the
    /// smallest difference the labels must show (a tick step), or 0.
    pub fn shown(&self, max_abs: f64, step: f64, sample: &[f64]) -> Shown {
        if time_format(&self.kind, None).is_some() {
            let decimals = self.decimals.unwrap_or(if self.kind == "seconds" { 1 } else { 0 });
            return Shown { div: 1.0, decimals, prefix: String::new(), suffix: String::new(), unit: String::new(), time: Some(self.kind.clone()), total: self.total };
        }
        let compact = self.kind == "compact" || (self.kind == "currency" && max_abs >= 1e4);
        let (div, unit) = if compact {
            if max_abs >= 1e12 { (1e12, "T") } else if max_abs >= 1e9 { (1e9, "B") } else if max_abs >= 1e6 { (1e6, "M") } else if max_abs >= 1e3 { (1e3, "K") } else { (1.0, "") }
        } else {
            (1.0, "")
        };
        // The digits a value needs after the point (up to 3).
        let digits = |x: f64| (0..=3u32).find(|d| {
            let scaled = x * 10f64.powi(*d as i32);
            (scaled - scaled.round()).abs() < 1e-6 * scaled.abs().max(1.0)
        }).unwrap_or(3);
        let decimals = self.decimals.unwrap_or_else(|| {
            let sample_d = sample.iter().map(|v| digits(v / div)).max().unwrap_or(0);
            let step_d = if step > 0.0 { digits(step / div) } else { 0 };
            if compact && max_abs / div >= 100.0 {
                step_d.min(2)
            } else if compact {
                step_d.max(sample_d.min(1)).min(2)
            } else {
                step_d.max(sample_d).min(2)
            }
        });
        let prefix = if self.kind == "currency" { format!("{}{}", self.prefix, self.currency) } else { self.prefix.clone() };
        let suffix = if self.kind == "percent" { format!("{unit}%{}", self.suffix) } else { format!("{unit}{}", self.suffix) };
        Shown { div, decimals, prefix, suffix, unit: unit.to_string(), time: None, total: None }
    }
}

/// An estimate of a line's width in the bundled sans (Inter's advances,
/// by character class; heavier weights run a little wider).
pub(crate) fn text_width(text: &str, size: f64, weight: f64) -> f64 {
    let mut em = 0.0;
    for c in text.chars() {
        em += match c {
            ' ' => 0.26,
            '.' | ',' | ':' | ';' | '\'' | '|' | '!' | 'i' | 'l' | 'j' | 'I' => 0.27,
            'f' | 't' | 'r' | '(' | ')' | '[' | ']' => 0.37,
            'm' | 'w' => 0.82,
            'M' | 'W' => 0.9,
            '0'..='9' => 0.62,
            '%' => 0.84,
            '$' | '€' | '£' => 0.62,
            '-' | '–' | '+' => 0.5,
            '▲' | '▼' => 0.7,
            c if c.is_uppercase() => 0.72,
            c if c.is_lowercase() => 0.58,
            _ => 0.62,
        };
    }
    em * size * (1.0 + (weight - 400.0).max(0.0) / 500.0 * 0.08)
}

/// A text block's box estimate `[w, h]`: `wrap` breaks it into lines,
/// `fit` shrinks it.
pub(crate) fn text_box(text: &str, size: f64, weight: f64, wrap: Option<f64>, fit: Option<f64>, line_height: f64) -> [f64; 2] {
    let mut lines: Vec<f64> = Vec::new();
    for line in text.lines() {
        match wrap {
            Some(wrap) if wrap > 0.0 => {
                let mut current = 0.0;
                let space = text_width(" ", size, weight);
                for word in line.split(' ') {
                    let ww = text_width(word, size, weight);
                    if current > 0.0 && current + space + ww > wrap {
                        lines.push(current);
                        current = ww;
                    } else {
                        current += if current > 0.0 { space + ww } else { ww };
                    }
                }
                lines.push(current);
            }
            _ => lines.push(text_width(line, size, weight)),
        }
    }
    if lines.is_empty() {
        lines.push(0.0);
    }
    let width = lines.iter().copied().fold(0.0, f64::max);
    let shrink = match fit {
        Some(fit) if fit > 0.0 && width > fit => fit / width,
        _ => 1.0,
    };
    let n = lines.len() as f64;
    [width * shrink, size * shrink * (1.21 + (n - 1.0) * line_height.max(1.0) * 1.21)]
}

/// `text` broken into lines no wider than `width` (for centred blocks:
/// explicit lines centre exactly).
pub(crate) fn wrap_lines(text: &str, size: f64, weight: f64, width: f64) -> String {
    let space = text_width(" ", size, weight);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        let mut w = 0.0;
        for word in line.split(' ').filter(|w| !w.is_empty()) {
            let ww = text_width(word, size, weight);
            if !current.is_empty() && w + space + ww > width {
                out.push(std::mem::take(&mut current));
                w = 0.0;
            }
            if !current.is_empty() {
                current.push(' ');
                w += space;
            }
            current.push_str(word);
            w += ww;
        }
        out.push(current);
    }
    out.join("\n")
}

/// Moves `want` positions apart to at least `gap` each, inside `lo..hi`,
/// as little as possible; returns them in the input's order.
pub(crate) fn spread(want: &[f64], gap: f64, lo: f64, hi: f64) -> Vec<f64> {
    let n = want.len();
    if n == 0 {
        return Vec::new();
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| want[*a].total_cmp(&want[*b]).then(a.cmp(b)));
    let mut pos: Vec<f64> = order.iter().map(|&i| want[i]).collect();
    // A few passes of push forward, pull back, then centre each cluster.
    for _ in 0..8 {
        for k in 1..n {
            if pos[k] < pos[k - 1] + gap {
                pos[k] = pos[k - 1] + gap;
            }
        }
        if pos[n - 1] > hi {
            pos[n - 1] = hi;
        }
        for k in (0..n - 1).rev() {
            if pos[k] > pos[k + 1] - gap {
                pos[k] = pos[k + 1] - gap;
            }
        }
        if pos[0] < lo {
            pos[0] = lo;
        }
    }
    for k in 1..n {
        if pos[k] < pos[k - 1] + gap {
            pos[k] = pos[k - 1] + gap;
        }
    }
    let mut out = vec![0.0; n];
    for (k, &i) in order.iter().enumerate() {
        out[i] = pos[k];
    }
    out
}

/// Inverse of `ease_in_out_sine`: the time fraction the eased value
/// reaches `f`.
pub(crate) fn inverse_in_out_sine(f: f64) -> f64 {
    (1.0 - 2.0 * f.clamp(0.0, 1.0)).acos() / std::f64::consts::PI
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_round_and_cover_the_range() {
        assert_eq!(nice_ticks(0.0, 91.0, 5), vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]);
        assert_eq!(nice_ticks(0.0, 1.0, 4), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        let t = nice_ticks(-3.2, 12.5, 5);
        assert!(t[0] <= -3.2 && *t.last().unwrap() >= 12.5 && t.contains(&0.0), "{t:?}");
    }

    #[test]
    fn numbers_format_compactly_and_consistently() {
        let money = NumFmt { kind: "currency".into(), ..Default::default() };
        let shown = money.shown(12_400_000.0, 0.0, &[12_400_000.0, 800_000.0]);
        assert_eq!(shown.text(12_400_000.0), "$12.4M");
        assert_eq!(shown.text(800_000.0), "$0.8M");
        let plain = NumFmt::default().shown(45_210.0, 0.0, &[45_210.0]);
        assert_eq!(plain.text(45_210.0), "45,210");
        let pct = NumFmt { kind: "percent".into(), ..Default::default() }.shown(42.0, 0.0, &[42.0, 18.0]);
        assert_eq!(pct.text(42.0), "42%");
        assert_eq!(NumFmt::default().shown(1.0, 0.25, &[0.25, 0.5]).text(0.25), "0.25");
        assert_eq!(NumFmt { kind: "mmss".into(), ..Default::default() }.shown(2710.0, 0.0, &[]).text(2710.0), "45:10");
    }

    #[test]
    fn spreading_keeps_order_gap_and_bounds() {
        let out = spread(&[10.0, 11.0, 12.0, 50.0], 5.0, 0.0, 100.0);
        assert!(out[1] - out[0] >= 5.0 - 1e-9 && out[2] - out[1] >= 5.0 - 1e-9 && out[3] == 50.0, "{out:?}");
        let out = spread(&[98.0, 99.0, 100.0], 5.0, 0.0, 100.0);
        assert!(out.iter().all(|p| *p <= 100.0 + 1e-9) && out[2] - out[1] >= 5.0 - 1e-9, "{out:?}");
        assert!((inverse_in_out_sine(0.5) - 0.5).abs() < 1e-9);
    }
}
