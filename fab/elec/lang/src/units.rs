//! Circuit quantities, registered literal suffixes and schematic value text.

use fab_elec_bench::input::LedColor;
use fab_elec_circuit::{QtyUnit, Quantity, Value};
use fab_elec_parts::PartKind;
use makepad_script::units::UnitTable;

const UNITS: &[(&str, QtyUnit, f64, f64)] = &[
    ("m", QtyUnit::Metre, 1.0, 1.0),
    ("cm", QtyUnit::Metre, 1.0, 1e2),
    ("mm", QtyUnit::Metre, 1.0, 1e3),
    ("um", QtyUnit::Metre, 1.0, 1e6),
    ("nm", QtyUnit::Metre, 1.0, 1e9),
    ("mil", QtyUnit::Metre, 254.0, 10_000_000.0),
    ("inch", QtyUnit::Metre, 254.0, 10_000.0),
    ("mohm", QtyUnit::Ohm, 1.0, 1e3),
    ("ohm", QtyUnit::Ohm, 1.0, 1.0),
    ("kohm", QtyUnit::Ohm, 1e3, 1.0),
    ("Mohm", QtyUnit::Ohm, 1e6, 1.0),
    ("pF", QtyUnit::Farad, 1.0, 1e12),
    ("nF", QtyUnit::Farad, 1.0, 1e9),
    ("uF", QtyUnit::Farad, 1.0, 1e6),
    ("mF", QtyUnit::Farad, 1.0, 1e3),
    ("F", QtyUnit::Farad, 1.0, 1.0),
    ("nH", QtyUnit::Henry, 1.0, 1e9),
    ("uH", QtyUnit::Henry, 1.0, 1e6),
    ("mH", QtyUnit::Henry, 1.0, 1e3),
    ("H", QtyUnit::Henry, 1.0, 1.0),
    ("uV", QtyUnit::Volt, 1.0, 1e6),
    ("mV", QtyUnit::Volt, 1.0, 1e3),
    ("V", QtyUnit::Volt, 1.0, 1.0),
    ("kV", QtyUnit::Volt, 1e3, 1.0),
    ("nA", QtyUnit::Ampere, 1.0, 1e9),
    ("uA", QtyUnit::Ampere, 1.0, 1e6),
    ("mA", QtyUnit::Ampere, 1.0, 1e3),
    ("A", QtyUnit::Ampere, 1.0, 1.0),
    ("Hz", QtyUnit::Hertz, 1.0, 1.0),
    ("kHz", QtyUnit::Hertz, 1e3, 1.0),
    ("MHz", QtyUnit::Hertz, 1e6, 1.0),
    ("GHz", QtyUnit::Hertz, 1e9, 1.0),
    ("uW", QtyUnit::Watt, 1.0, 1e6),
    ("mW", QtyUnit::Watt, 1.0, 1e3),
    ("W", QtyUnit::Watt, 1.0, 1.0),
    ("kW", QtyUnit::Watt, 1e3, 1.0),
    ("ps", QtyUnit::Second, 1.0, 1e12),
    ("ns", QtyUnit::Second, 1.0, 1e9),
    ("us", QtyUnit::Second, 1.0, 1e6),
    ("ms", QtyUnit::Second, 1.0, 1e3),
    ("s", QtyUnit::Second, 1.0, 1.0),
];

const PREFIXES: &[(i32, &str)] = &[
    (-12, "p"), (-9, "n"), (-6, "u"), (-3, "m"), (0, ""), (3, "k"), (6, "M"), (9, "G"),
];

/// Standard script units plus circuit suffixes, expressed as ratios to SI units.
pub fn circuit_units() -> UnitTable {
    UNITS.iter().fold(UnitTable::standard(), |table, &(suffix, _, mul, div)| {
        table.with(suffix, mul, div)
    }).with("%", 1.0, 100.0).with("ppm", 1.0, 1e6)
}

/// The quantity unit and SI ratio for a suffix, excluding angles and pixels.
pub fn qty_of(suffix: &str) -> Option<(QtyUnit, f64, f64)> {
    UNITS.iter().find_map(|&(name, unit, mul, div)| (name == suffix).then_some((unit, mul, div)))
}

/// Schematic text in engineering notation with at most four significant digits.
/// Resistance omits its symbol; zero omits its prefix.
pub fn value_text(q: &Quantity) -> String {
    if q.unit == QtyUnit::Count {
        return format!("{}", q.si);
    }
    let symbol = symbol(q.unit);
    if q.si == 0.0 {
        return format!("0{symbol}");
    }
    if !q.si.is_finite() {
        return format!("{}{symbol}", q.si);
    }
    let scientific = format!("{:.3e}", q.si);
    let (mantissa, exponent) = decimal_parts(&scientific);
    let power = engineering_power(exponent);
    let prefix = PREFIXES.iter().find(|&&(p, _)| p == power).expect("engineering prefix").1;
    format!("{}{prefix}{symbol}", decimal_text(mantissa, exponent - power))
}

/// A registered quantity literal whose decimal digits preserve the exact SI value.
/// Missing prefixes use the nearest registered prefix below them, or the smallest
/// registered prefix if none is below. Negative values include a unary minus.
///
/// # Panics
/// Panics if the SI value is not finite.
pub fn literal_text(q: &Quantity) -> String {
    assert!(q.si.is_finite(), "quantity literal requires a finite SI value");
    // A count is a plain number.
    if q.unit == QtyUnit::Count {
        return format!("{}", q.si);
    }
    let scientific = format!("{:e}", q.si);
    let (mantissa, exponent) = decimal_parts(&scientific);
    let preferred = engineering_power(exponent);
    let (power, suffix) = PREFIXES.iter().rev()
        .filter(|&&(power, _)| power <= preferred)
        .find_map(|&(power, _)| suffix_at(q.unit, power).map(|suffix| (power, suffix)))
        .or_else(|| PREFIXES.iter()
            .find_map(|&(power, _)| suffix_at(q.unit, power).map(|suffix| (power, suffix))))
        .expect("every quantity has a registered decimal suffix");
    format!("{}{suffix}", decimal_text(mantissa, exponent - power))
}

fn suffix_at(unit: QtyUnit, power: i32) -> Option<&'static str> {
    let (mul, div) = if power < 0 { (1.0, 10f64.powi(-power)) } else { (10f64.powi(power), 1.0) };
    UNITS.iter().find_map(|&(suffix, candidate, m, d)| {
        (candidate == unit && m == mul && d == div).then_some(suffix)
    })
}

fn symbol(unit: QtyUnit) -> &'static str {
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

fn engineering_power(exponent: i32) -> i32 {
    (exponent.div_euclid(3) * 3).clamp(-12, 9)
}

fn decimal_parts(scientific: &str) -> (&str, i32) {
    let (mantissa, exponent) = scientific.split_once('e').expect("finite scientific notation");
    (mantissa, exponent.parse().expect("decimal exponent"))
}

pub(crate) fn decimal_text(mantissa: &str, exponent: i32) -> String {
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(mantissa) => ("-", mantissa),
        None => ("", mantissa),
    };
    let digits = mantissa.replace('.', "");
    let point = exponent + 1;
    // Move the decimal point without rounding through a scaled float.
    let mut text = if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!("{sign}{digits}{}", "0".repeat(point as usize - digits.len()))
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    };
    if text.contains('.') {
        text.truncate(text.trim_end_matches('0').trim_end_matches('.').len());
    }
    text
}

/// A typed label or a decimal quantity in the part's unit family. Prefixes use the language's
/// registered suffixes. Only the submitted text is scanned.
pub fn parse_value(text: &str, current: &Value, kind: &PartKind) -> Result<Value, String> {
    let text = text.trim();
    let Some(current_qty) = current.qty else {
        if text.is_empty() { return Err("enter a value".into()); }
        if *kind == PartKind::Led && LedColor::of_name(&current.text).is_some()
            && (text.bytes().any(|b| b.is_ascii_uppercase()) || LedColor::of_name(text).is_none())
        {
            return Err(format!("unknown LED colour: {text}"));
        }
        return Ok(Value { text: text.into(), qty: None, tol: current.tol });
    };
    let invalid = || format!("not a value: {text}");
    let bytes = text.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
    let mut digits = end - start;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
        digits += end - start;
    }
    if digits == 0 { return Err(invalid()); }
    if matches!(bytes.get(end), Some(b'e' | b'E')) {
        end += 1;
        if matches!(bytes.get(end), Some(b'+' | b'-')) { end += 1; }
        let start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
        if end == start { return Err(invalid()); }
    }
    let number = text[..end].parse::<f64>().map_err(|_| invalid())?;
    let suffix = text[end..].trim();
    let micro;
    let suffix = if let Some(rest) = suffix.strip_prefix('µ') {
        micro = format!("u{rest}");
        micro.as_str()
    } else { suffix };
    let (symbol, needs) = match current_qty.unit {
        QtyUnit::Ohm => ("ohm", "a resistance needs ohms"),
        QtyUnit::Farad => ("F", "a capacitance needs farads"),
        QtyUnit::Henry => ("H", "an inductance needs henries"),
        QtyUnit::Volt => ("V", "a voltage needs volts"),
        QtyUnit::Ampere => ("A", "a current needs amperes"),
        QtyUnit::Hertz => ("Hz", "a frequency needs hertz"),
        QtyUnit::Watt => ("W", "a power needs watts"),
        QtyUnit::Metre => ("m", "a length needs metres"),
        QtyUnit::Second => ("s", "a duration needs seconds"),
        QtyUnit::Count => ("", "a count needs a bare number"),
    };
    let (mul, div) = if suffix.is_empty() {
        (1.0, 1.0)
    } else {
        let explicit = qty_of(suffix);
        let resolved = explicit.filter(|&(unit, _, _)| unit == current_qty.unit).or_else(|| {
            if current_qty.unit != QtyUnit::Count && matches!(suffix, "p" | "n" | "u" | "m" | "c" | "k" | "M" | "G") {
                qty_of(&format!("{suffix}{symbol}"))
            } else { None }
        });
        match resolved {
            Some((unit, mul, div)) if unit == current_qty.unit => (mul, div),
            _ if explicit.is_some() => return Err(format!("{needs}: {text}")),
            _ => return Err(invalid()),
        }
    };
    let q = Quantity { si: number * mul / div, unit: current_qty.unit };
    if !q.si.is_finite() { return Err(invalid()); }
    Ok(Value { text: value_text(&q), qty: Some(q), tol: current.tol })
}
