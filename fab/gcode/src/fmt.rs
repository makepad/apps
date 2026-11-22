//! Number formatting in the printer dialect: fixed decimals with trailing
//! zeros and a leading zero dropped (`0.40` → `.4`, `-0.50` → `-.5`).

thread_local! { static NONFINITE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Take and reset this thread's non-finite count; each formatted chunk returns its own.
pub fn take_nonfinite() -> usize { NONFINITE.with(|n| n.replace(0)) }

pub fn trim(v: f64, decimals: usize) -> String {
    let mut s = String::with_capacity(12);
    trim_into(&mut s, v, decimals);
    s
}

const POW10: [f64; 6] = [1.0, 10.0, 100.0, 1000.0, 10000.0, 100000.0];
const IPOW10: [u64; 6] = [1, 10, 100, 1000, 10000, 100000];

/// [`trim`] appended to `out`. Exact: the fast path is taken only where the
/// scaled value is far enough from a rounding tie that the product's
/// rounding error cannot change the digits; anything else is formatted by
/// the standard library and trimmed the same way.
pub fn trim_into(out: &mut String, v: f64, decimals: usize) {
    if !v.is_finite() {
        NONFINITE.with(|n| n.set(n.get() + 1));
        out.push_str(&trim_std(v, decimals));
        return;
    }
    if decimals < POW10.len() {
        let s = v.abs() * POW10[decimals];
        // |s| < 1e9: the product's error is below 1.2e-7 of a unit.
        if s < 1e9 {
            let fl = s.floor();
            let frac = s - fl;
            if (frac - 0.5).abs() > 1e-6 {
                let n = fl as u64 + (frac > 0.5) as u64;
                let (ip, mut fp) = (n / IPOW10[decimals], n % IPOW10[decimals]);
                if v < 0.0 && n != 0 {
                    out.push('-');
                }
                let mut digits = decimals;
                while digits > 0 && fp % 10 == 0 {
                    fp /= 10;
                    digits -= 1;
                }
                if ip != 0 || digits == 0 {
                    push_u64(out, ip);
                }
                if digits > 0 {
                    out.push('.');
                    let mut buf = [b'0'; 8];
                    let mut k = digits;
                    while k > 0 {
                        k -= 1;
                        buf[k] = b'0' + (fp % 10) as u8;
                        fp /= 10;
                    }
                    out.push_str(std::str::from_utf8(&buf[..digits]).unwrap());
                }
                return;
            }
        }
    }
    out.push_str(&trim_std(v, decimals));
}

/// The value [`trim_into`] writes, as [`parse_f64`] reads it back: the same rounding, without
/// the text.
pub fn rounded(v: f64, decimals: usize) -> f64 {
    if decimals < POW10.len() {
        let s = v.abs() * POW10[decimals];
        if s < 1e9 {
            let fl = s.floor();
            let frac = s - fl;
            if (frac - 0.5).abs() > 1e-6 {
                // n / 10^d, correctly rounded: what parsing the trimmed digits gives.
                let n = fl as u64 + (frac > 0.5) as u64;
                let r = n as f64 / POW10[decimals];
                return if v < 0.0 && n != 0 { -r } else { r };
            }
        }
    }
    parse_f64(&trim_std(v, decimals)).unwrap_or(0.0)
}

fn push_u64(out: &mut String, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.push_str(std::str::from_utf8(&buf[i..]).unwrap());
}

fn trim_std(v: f64, decimals: usize) -> String {
    let mut s = format!("{:.*}", decimals, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    if let Some(r) = s.strip_prefix("0.") {
        return format!(".{r}");
    }
    if let Some(r) = s.strip_prefix("-0.") {
        return format!("-.{r}");
    }
    s
}

/// Parse a G-code number as `str::parse::<f64>` does. Plain decimals of at
/// most 15 digits are exact as `digits / 10^k` (both exactly representable,
/// one correctly rounded division); anything else goes to the standard
/// parser.
pub fn parse_f64(t: &str) -> Option<f64> {
    let b = t.as_bytes();
    let (neg, body) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        _ => (false, b),
    };
    let mut m: u64 = 0;
    let mut digits = 0u32;
    let mut frac = 0u32;
    let mut dot = false;
    for &c in body {
        match c {
            b'0'..=b'9' => {
                m = m * 10 + (c - b'0') as u64;
                digits += 1;
                if dot {
                    frac += 1;
                }
            }
            b'.' if !dot => dot = true,
            _ => return t.parse().ok(),
        }
    }
    if digits == 0 || digits > 15 {
        return t.parse().ok();
    }
    const P: [f64; 16] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15];
    let v = m as f64 / P[frac as usize];
    Some(if neg { -v } else { v })
}

/// Coordinates: 3 decimals.
pub fn xyz(v: f64) -> String {
    trim(v, 3)
}

/// Extrusion: 5 decimals.
pub fn e(v: f64) -> String {
    trim(v, 5)
}

/// A fan setting (`M106 S`, 0..255): up to 2 decimals, as the printer's
/// slicer writes a percentage of 255 (`S178.5`, `S160.65`).
pub fn fan(v: f64) -> String {
    let s = trim(v, 2);
    if s.starts_with('.') {
        format!("0{s}")
    } else if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// Feedrate in mm/min: 3 decimals.
pub fn f(v: f64) -> String {
    trim(v, 3)
}

/// A tag value such as a line width (`%g`-like, 5 significant decimals), appended to `out`.
pub fn tag_into(out: &mut String, v: f64) {
    let start = out.len();
    trim_into(out, v, 5);
    if out[start..].starts_with('.') {
        out.insert(start, '0');
    } else if out[start..].starts_with("-.") {
        out.insert(start + 1, '0');
    }
}

/// `1d 2h 3m 4s`, leading zero units left out.
pub fn duration(t: f64) -> String {
    let s = t.round().max(0.0) as u64;
    let (d, h, m, sec) = (s / 86400, (s / 3600) % 24, (s / 60) % 60, s % 60);
    let mut out = String::new();
    if d > 0 {
        out.push_str(&format!("{d}d "));
    }
    if d > 0 || h > 0 {
        out.push_str(&format!("{h}h "));
    }
    if d > 0 || h > 0 || m > 0 {
        out.push_str(&format!("{m}m "));
    }
    out.push_str(&format!("{sec}s"));
    out
}
