//! Preferred-number scrubbing and the literals written back to circuit source.
//! Work per drag is bounded by one series (at most 96 mantissas), independent of the circuit.

use fab_elec_circuit::{CircuitOp, QtyUnit, Quantity, Tol, Value};
use fab_elec_core::CompId;

const MIN_SI: f64 = 1e-12;
const MAX_SI: f64 = 1e12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Series {
    E6,
    E12,
    E24,
    E48,
    E96,
}

/// Resistors default to E24; other quantities default to E12. An off-series value uses
/// the smallest larger series containing it, falling back to E96.
pub fn series_for(q: &Quantity) -> Series {
    if !q.si.is_finite() || q.si <= 0.0 {
        return Series::E96;
    }
    let default = if q.unit == QtyUnit::Ohm { Series::E24 } else { Series::E12 };
    let logarithm = q.si.log10();
    let m = 10.0_f64.powf(logarithm - logarithm.floor());
    for s in [default, Series::E24, Series::E48, Series::E96] {
        if same_si(m, 10.0) || mantissas(s).iter().any(|&v| same_si(m, v)) {
            return s;
        }
    }
    Series::E96
}

/// Standard preferred-number mantissas in [1, 10), in ascending order.
pub fn mantissas(s: Series) -> &'static [f64] {
    match s {
        Series::E6 => &[1.0, 1.5, 2.2, 3.3, 4.7, 6.8],
        Series::E12 => &[1.0, 1.2, 1.5, 1.8, 2.2, 2.7, 3.3, 3.9, 4.7, 5.6, 6.8, 8.2],
        Series::E24 => &[
            1.0, 1.1, 1.2, 1.3, 1.5, 1.6, 1.8, 2.0, 2.2, 2.4, 2.7, 3.0,
            3.3, 3.6, 3.9, 4.3, 4.7, 5.1, 5.6, 6.2, 6.8, 7.5, 8.2, 9.1,
        ],
        Series::E48 => &[
            1.00, 1.05, 1.10, 1.15, 1.21, 1.27, 1.33, 1.40, 1.47, 1.54, 1.62, 1.69,
            1.78, 1.87, 1.96, 2.05, 2.15, 2.26, 2.37, 2.49, 2.61, 2.74, 2.87, 3.01,
            3.16, 3.32, 3.48, 3.65, 3.83, 4.02, 4.22, 4.42, 4.64, 4.87, 5.11, 5.36,
            5.62, 5.90, 6.19, 6.49, 6.81, 7.15, 7.50, 7.87, 8.25, 8.66, 9.09, 9.53,
        ],
        Series::E96 => &[
            1.00, 1.02, 1.05, 1.07, 1.10, 1.13, 1.15, 1.18, 1.21, 1.24, 1.27, 1.30,
            1.33, 1.37, 1.40, 1.43, 1.47, 1.50, 1.54, 1.58, 1.62, 1.65, 1.69, 1.74,
            1.78, 1.82, 1.87, 1.91, 1.96, 2.00, 2.05, 2.10, 2.15, 2.21, 2.26, 2.32,
            2.37, 2.43, 2.49, 2.55, 2.61, 2.67, 2.74, 2.80, 2.87, 2.94, 3.01, 3.09,
            3.16, 3.24, 3.32, 3.40, 3.48, 3.57, 3.65, 3.74, 3.83, 3.92, 4.02, 4.12,
            4.22, 4.32, 4.42, 4.53, 4.64, 4.75, 4.87, 4.99, 5.11, 5.23, 5.36, 5.49,
            5.62, 5.76, 5.90, 6.04, 6.19, 6.34, 6.49, 6.65, 6.81, 6.98, 7.15, 7.32,
            7.50, 7.68, 7.87, 8.06, 8.25, 8.45, 8.66, 8.87, 9.09, 9.31, 9.53, 9.76,
        ],
    }
}

/// Snap to the nearest series value, then move `steps` places, clamped to [1e-12, 1e12].
/// Midpoint ties snap down. NaN starts at the lower bound; infinities clamp to the bounds.
pub fn step(si: f64, s: Series, steps: i32) -> f64 {
    let si = if si.is_nan() { MIN_SI } else { si.clamp(MIN_SI, MAX_SI) };
    let table = mantissas(s);
    let count = table.len() as i64;
    let decade = si.log10().floor() as i32;
    let m = si / 10.0_f64.powi(decade);
    let mut nearest = 0;
    let mut distance = (m - table[0]).abs();
    for (i, &candidate) in table.iter().enumerate().skip(1) {
        let next_distance = (m - candidate).abs();
        if next_distance < distance {
            nearest = i as i64;
            distance = next_distance;
        }
    }
    // The nearest value may be the first mantissa of the next decade.
    if (m - 10.0).abs() < distance {
        nearest = count;
    }
    let index = ((i64::from(decade) + 12) * count + nearest + i64::from(steps))
        .clamp(0, 24 * count);
    let si = table[(index % count) as usize] * 10.0_f64.powi((index / count) as i32 - 12);
    si.clamp(MIN_SI, MAX_SI)
}

/// Canonical source literal, with at most three significant digits and `u` for micro.
/// Ohms omit a unit suffix. Outside the pico-to-giga range, use scientific notation
/// (including `1e12` at the scrub ceiling); zero is written as `0` plus the unit suffix.
pub fn literal(si: f64, unit: QtyUnit) -> String {
    let suffix = unit_suffix(unit);
    if si == 0.0 {
        return format!("0{suffix}");
    }
    if !si.is_finite() {
        return format!("{si}{suffix}");
    }
    // Round before choosing the prefix, so 999.9 ohms becomes 1k rather than 1000.
    // Decimal formatting also handles subnormal and very large finite quantities.
    let rounded = format!("{si:.2e}");
    let (coefficient, exponent) = rounded.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let prefix = match exponent.div_euclid(3) {
        -4 => "p",
        -3 => "n",
        -2 => "u",
        -1 => "m",
        0 => "",
        1 => "k",
        2 => "M",
        3 => "G",
        _ => {
            let coefficient = coefficient.trim_end_matches('0').trim_end_matches('.');
            return format!("{coefficient}e{exponent}{suffix}");
        }
    };
    let mut number = coefficient.replace('.', "");
    let point = usize::from(si.is_sign_negative()) + 1 + exponent.rem_euclid(3) as usize;
    if point < number.len() {
        number.insert(point, '.');
        while number.ends_with('0') {
            number.pop();
        }
        if number.ends_with('.') {
            number.pop();
        }
    }
    format!("{number}{prefix}{suffix}")
}

/// Parse canonical literals and common forms such as `4k7`, `4.7K`, `100n`, `100 nF`,
/// `1µF`, and `2R2`. An explicit unit must match; nonfinite quantities are rejected.
pub fn parse(text: &str, unit: QtyUnit) -> Option<f64> {
    let text = text.trim();
    let text = if unit == QtyUnit::Ohm {
        text.strip_suffix('Ω')
            .or_else(|| text.strip_suffix("Ohm"))
            .or_else(|| text.strip_suffix("ohm"))
            .unwrap_or(text)
    } else {
        text.strip_suffix(unit_suffix(unit)).unwrap_or(text)
    };
    let text = text.trim_end();
    if let Ok(si) = text.parse::<f64>() {
        return si.is_finite().then_some(si);
    }
    let (index, prefix) = text.char_indices().find(|&(_, ch)| prefix_scale(ch, unit).is_some())?;
    let scale = prefix_scale(prefix, unit)?;
    let head = &text[..index];
    let tail = &text[index + prefix.len_utf8()..];
    let number = if tail.is_empty() {
        head.trim_end().parse::<f64>().ok()?
    } else {
        let digits = head.strip_prefix('+').or_else(|| head.strip_prefix('-')).unwrap_or(head);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit())
            || !tail.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        format!("{head}.{tail}").parse::<f64>().ok()?
    };
    let si = number * scale;
    si.is_finite().then_some(si)
}

fn prefix_scale(prefix: char, unit: QtyUnit) -> Option<f64> {
    Some(match prefix {
        'p' => 1e-12,
        'n' => 1e-9,
        'u' | 'µ' | 'μ' => 1e-6,
        'm' => 1e-3,
        'k' | 'K' => 1e3,
        'M' => 1e6,
        'G' => 1e9,
        'R' | 'r' if unit == QtyUnit::Ohm => 1.0,
        _ => return None,
    })
}

fn unit_suffix(unit: QtyUnit) -> &'static str {
    match unit {
        QtyUnit::Ohm => "",
        QtyUnit::Farad => "F",
        QtyUnit::Henry => "H",
        QtyUnit::Volt => "V",
        QtyUnit::Ampere => "A",
        QtyUnit::Hertz => "Hz",
        QtyUnit::Watt => "W",
        QtyUnit::Metre => "m",
        QtyUnit::Second => "s",
        QtyUnit::Count => "",
    }
}

fn same_si(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= a.abs().max(b.abs()) * 1e-9
}

/// One drag on a value chip; only `finish` produces an edit operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Scrub {
    pub comp: CompId,
    pub unit: QtyUnit,
    pub start_si: f64,
    pub series: Series,
    pub start_text: String,
    pub start_tol: Option<Tol>,
    pub steps: i32,
}

impl Scrub {
    /// Start on a component's quantity; non-quantity values and plain counts (which have no
    /// preferred-number series) cannot be scrubbed.
    pub fn begin(comp: CompId, value: &Value) -> Option<Scrub> {
        let q = value.qty.filter(|q| q.unit != QtyUnit::Count)?;
        Some(Self {
            comp,
            unit: q.unit,
            start_si: q.si,
            series: series_for(&q),
            start_text: value.text.clone(),
            start_tol: value.tol,
            steps: 0,
        })
    }

    /// Set the displacement from the press: nearest step per 18 logical points, with a
    /// four-point dead zone. Return whether the step count changed.
    pub fn drag(&mut self, dx: f64) -> bool {
        let steps = if dx.abs() <= 4.0 { 0 } else { (dx / 18.0).round() as i32 };
        let changed = steps != self.steps;
        self.steps = steps;
        changed
    }

    /// The current quantity and its canonical source literal. Zero steps preserve the
    /// starting quantity, even when it was not on a preferred-number series.
    pub fn value(&self) -> Value {
        let si = if self.steps == 0 { self.start_si } else { step(self.start_si, self.series, self.steps) };
        Value { text: literal(si, self.unit), qty: Some(Quantity { si, unit: self.unit }), tol: self.start_tol }
    }

    /// The single release edit, or no edit when back at the start (including clamped drags).
    pub fn finish(&self) -> Option<CircuitOp> {
        if self.steps == 0 {
            return None;
        }
        let value = self.value();
        if same_si(value.qty?.si, self.start_si) {
            return None;
        }
        Some(CircuitOp::SetValue { comp: self.comp, value })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_round_trip_and_series_step() {
        use QtyUnit::*;
        let cases = [
            (4700.0, Ohm, "4.7k"), (270.0, Ohm, "270"), (1e6, Ohm, "1M"),
            (100e-9, Farad, "100nF"), (4.7e-6, Farad, "4.7uF"), (2.2e-6, Henry, "2.2uH"),
            (1e-12, Farad, "1pF"), (33e-12, Farad, "33pF"), (1e-6, Farad, "1uF"),
            (2.2, Ohm, "2.2"), (0.0, Ohm, "0"), (1e-3, Volt, "1mV"),
            (2.2, Ampere, "2.2A"), (1e9, Hertz, "1GHz"), (3.3, Watt, "3.3W"),
            (1e-3, Metre, "1mm"), (1.0, Metre, "1m"), (1e-6, Second, "1us"),
            (-2.2e-3, Volt, "-2.2mV"), (1e12, Ohm, "1e12"),
        ];
        for (si, unit, expected) in cases {
            let text = literal(si, unit);
            assert_eq!(text, expected, "canonical literal for {si} {unit:?}");
            let parsed = parse(&text, unit).expect("canonical literal must parse");
            assert!((parsed - si).abs() <= si.abs() * 1e-9, "literal round trip: {text}");
        }
        for (text, unit, si) in [
            ("4k7", Ohm, 4700.0), ("4.7K", Ohm, 4700.0), ("100n", Farad, 100e-9),
            ("100 nF", Farad, 100e-9), ("1µF", Farad, 1e-6), ("2R2", Ohm, 2.2),
        ] {
            let parsed = parse(text, unit).expect("common quantity variant must parse");
            assert!((parsed - si).abs() <= si.abs() * 1e-9, "variant round trip: {text}");
        }
        assert_eq!(parse("4k7", Ohm), Some(4700.0), "embedded resistor prefix");
        for (text, unit) in [("1H", Farad), ("green", Ohm), ("4.7k7", Ohm), ("NaN", Volt)] {
            assert_eq!(parse(text, unit), None, "reject invalid or mismatched quantity: {text}");
        }
        assert_eq!(literal(999.9, Ohm), "1k", "rounding carries into the next prefix");
        for (si, series, steps, expected) in [
            (10_000.0, Series::E24, 1, 11_000.0), (10_000.0, Series::E24, -1, 9100.0),
            (100e-9, Series::E12, 1, 120e-9), (9.1e3, Series::E24, 1, 10e3),
            (10_400.0, Series::E24, 0, 10_000.0), (9800.0, Series::E24, 0, 10_000.0),
        ] {
            let actual = step(si, series, steps);
            assert!((actual - expected).abs() <= expected.abs() * 1e-9,
                "nearest snap and decade step: {si} {series:?} {steps}");
        }
        for series in [Series::E6, Series::E12, Series::E24, Series::E48, Series::E96] {
            assert_eq!(step(MIN_SI, series, -1), MIN_SI, "lower bound: {series:?}");
            assert_eq!(step(MAX_SI, series, 1), MAX_SI, "upper bound: {series:?}");
            assert_eq!(step(1.0, series, i32::MIN), MIN_SI, "large negative step: {series:?}");
            assert_eq!(step(1.0, series, i32::MAX), MAX_SI, "large positive step: {series:?}");
        }
        for (si, unit, expected) in [
            (10_000.0, Ohm, Series::E24), (100e-9, Farad, Series::E12),
            (2.2e-6, Henry, Series::E12), (1.5, Volt, Series::E12),
            (2.4e-6, Farad, Series::E24), (1.05e3, Ohm, Series::E48),
            (1.02e3, Ohm, Series::E96), (1234.0, Ohm, Series::E96),
        ] {
            assert_eq!(series_for(&Quantity { si, unit }), expected,
                "default or smallest containing series: {si} {unit:?}");
        }
    }

    #[test]
    fn a_scrub_emits_one_op_only_when_changed() {
        let comp = CompId(0);
        let start = Value { text: "10k".into(), qty: Some(Quantity { si: 10_000.0, unit: QtyUnit::Ohm }), tol: None };
        let mut scrub = Scrub::begin(comp, &start).expect("quantity starts a scrub");
        assert_eq!(scrub.start_text, start.text, "retain original source literal");
        assert_eq!(scrub.series, Series::E24, "resistor default series");
        assert!(!scrub.drag(3.0), "click stays inside the dead zone");
        assert_eq!(scrub.value(), start, "click preserves the value");
        assert_eq!(scrub.finish(), None, "click produces no undo entry");
        assert!(scrub.drag(40.0), "right drag changes the step count");
        assert_eq!(scrub.steps, 2, "40 points is two steps");
        assert_eq!(scrub.value().text, "12k", "two upward resistor steps");
        assert!(!scrub.drag(40.0), "repeated displacement does not change steps");
        assert!(scrub.drag(0.0), "returning to the press changes steps back");
        assert_eq!(scrub.value(), start, "return to the exact starting value");
        assert_eq!(scrub.finish(), None, "return to start produces no undo entry");
        assert!(scrub.drag(-20.0), "left drag changes the step count");
        let expected = Value { text: "9.1k".into(), qty: Some(Quantity { si: 9100.0, unit: QtyUnit::Ohm }), tol: None };
        assert_eq!(scrub.value(), expected, "one downward resistor step");
        assert_eq!(scrub.finish(), Some(CircuitOp::SetValue { comp, value: expected }),
            "release contains one SetValue for the component and canonical value");
        assert_eq!(Scrub::begin(comp, &Value { text: "green".into(), qty: None, tol: None }), None,
            "non-quantity chip cannot be scrubbed");
    }
}
