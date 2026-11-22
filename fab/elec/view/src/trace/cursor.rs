use super::columns::{Column, Columns};

/// Formats significant figures with an SI prefix from pico through giga.
/// For example, `eng(50.0123e-6, "s", 5)` is `"50.012 µs"`.
/// Values beyond those prefixes use scientific notation; non-finite values use `—`.
pub fn eng(value: f64, unit: &str, sig: usize) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    let sig = sig.max(1);
    if value == 0.0 {
        return format!("{:.precision$} {unit}", 0.0, precision = sig - 1);
    }
    let mut exponent = ((value.abs().log10().floor() / 3.0).floor() as i32 * 3).clamp(-12, 9);
    let mut scaled = value / 10.0_f64.powi(exponent);
    let digits = scaled.abs().log10().floor() as i32 + 1;
    let step = 10.0_f64.powf(f64::from(digits) - sig as f64);
    if step > 0.0 && (scaled / step).is_finite() {
        scaled = (scaled / step).round() * step;
    }
    if scaled.abs() >= 1000.0 && exponent < 9 {
        exponent += 3;
        scaled /= 1000.0;
    }
    let prefix = match exponent {
        -12 => "p",
        -9 => "n",
        -6 => "µ",
        -3 => "m",
        0 => "",
        3 => "k",
        6 => "M",
        _ => "G",
    };
    if scaled.abs() < 1.0 || scaled.abs() >= 1000.0 {
        return format!("{scaled:.precision$e} {prefix}{unit}", precision = sig - 1);
    }
    let digits = scaled.abs().log10().floor() as usize + 1;
    let decimals = sig.saturating_sub(digits);
    format!("{scaled:.decimals$} {prefix}{unit}")
}

/// What a cursor snaps to near the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Snap {
    Sample,
    Crossing,
    Peak,
    Trough,
    Free,
}

/// Finds a nearby sample, then a level crossing, then a local extreme.
/// Crossings use retained endpoints. Between eligible extremes, the nearer one wins.
pub fn snap(cols: &Columns, x: f32, radius: f32, level: f32) -> (f32, Snap) {
    if !x.is_finite() || !radius.is_finite() || radius < 0.0 {
        return (x, Snap::Free);
    }
    let data = &cols.cols_data;
    let near = |position: f32| position.is_finite() && (position - x).abs() <= radius;
    let closer = |candidate: f32, current: Option<f32>| current.is_none_or(|p| (candidate - x).abs() < (p - x).abs());
    let mut sample = None;
    let mut sparse = true;
    for column in data {
        if column.x1 >= x - radius && column.x0 <= x + radius {
            sparse &= column.count == 1;
            if column.count == 1 && column.y0.is_finite() && near(column.x0) && closer(column.x0, sample) {
                sample = Some(column.x0);
            }
        }
    }
    if let Some(position) = sample.filter(|_| sparse) {
        return (position, Snap::Sample);
    }

    let mut crossing = None;
    let mut consider = |a: (f32, f32), b: (f32, f32)| {
        if !a.0.is_finite() || !a.1.is_finite() || !b.0.is_finite() || !b.1.is_finite() || b.0 <= a.0 || a.1 == b.1 {
            return;
        }
        if level >= a.1.min(b.1) && level <= a.1.max(b.1) {
            let fraction = (f64::from(level) - f64::from(a.1)) / (f64::from(b.1) - f64::from(a.1));
            let position = (f64::from(a.0) + (f64::from(b.0) - f64::from(a.0)) * fraction) as f32;
            if near(position) && closer(position, crossing) {
                crossing = Some(position);
            }
        }
    };
    let mut previous = cols.before;
    for column in data {
        let first = (column.x0, column.y0);
        let last = (column.x1, column.y1);
        if let Some(prev) = previous {
            consider(prev, first);
        }
        // Reduction retains only the first and last samples of a dense column.
        consider(first, last);
        previous = Some(last);
    }
    if let (Some(prev), Some(next)) = (previous, cols.after) {
        consider(prev, next);
    }
    if let Some(position) = crossing {
        return (position, Snap::Crossing);
    }

    let mut peak = None;
    let mut trough = None;
    for (i, column) in data.iter().enumerate() {
        let position = column.x0 + (column.x1 - column.x0) * 0.5;
        if !near(position) {
            continue;
        }
        if column.hi.is_finite()
            && peak.is_none_or(|j: usize| column.hi > data[j].hi || (column.hi == data[j].hi && closer(position, Some((data[j].x0 + data[j].x1) * 0.5))))
        {
            peak = Some(i);
        }
        if column.lo.is_finite()
            && trough.is_none_or(|j: usize| column.lo < data[j].lo || (column.lo == data[j].lo && closer(position, Some((data[j].x0 + data[j].x1) * 0.5))))
        {
            trough = Some(i);
        }
    }
    let mut extreme = (x, Snap::Free);
    for (index, kind) in [(peak, Snap::Peak), (trough, Snap::Trough)] {
        let Some(i) = index else { continue };
        let column = data[i];
        let value = |c: &Column| if kind == Snap::Peak { c.hi } else { -c.lo };
        let left = i.checked_sub(1).map(|j| value(&data[j])).or_else(|| cols.before.map(|p| if kind == Snap::Peak { p.1 } else { -p.1 }));
        let right = data.get(i + 1).map(value).or_else(|| cols.after.map(|p| if kind == Snap::Peak { p.1 } else { -p.1 }));
        if let (Some(left), Some(right)) = (left, right) {
            let v = value(&column);
            let position = column.x0 + (column.x1 - column.x0) * 0.5;
            if left.is_finite()
                && right.is_finite()
                && v >= left
                && v >= right
                && (v > left || v > right)
                && (extreme.1 == Snap::Free || (position - x).abs() < (extreme.0 - x).abs())
            {
                extreme = (position, kind);
            }
        }
    }
    extreme
}

/// The two cursor times in seconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursors {
    pub a: Option<f64>,
    pub b: Option<f64>,
}

impl Cursors {
    /// Lists set times, their signed difference and its reciprocal magnitude.
    pub fn time_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(3);
        if let Some(a) = self.a {
            lines.push(format!("A  {}", eng(a, "s", 5)));
        }
        if let Some(b) = self.b {
            lines.push(format!("B  {}", eng(b, "s", 5)));
        }
        if let (Some(a), Some(b)) = (self.a, self.b) {
            let delta = b - a;
            lines.push(format!("Δt {}   1/Δt {}", eng(delta, "s", 5), eng(1.0 / delta.abs(), "Hz", 5)));
        }
        lines
    }
}
