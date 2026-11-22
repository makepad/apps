//! Number and string forms shared by the Gerber and drill writers and readers.

use fab_elec_core::Nm;

/// Nanometres as a decimal millimetre string with at most six decimals, trailing zeros cut.
pub fn mm_str(nm: i64) -> String {
    let neg = nm < 0;
    let a = nm.unsigned_abs();
    let (i, f) = (a / 1_000_000, a % 1_000_000);
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(&i.to_string());
    if f != 0 {
        let fr = format!("{f:06}");
        s.push('.');
        s.push_str(fr.trim_end_matches('0'));
    }
    s
}

/// Microdegrees as a decimal degree string.
pub fn deg_str(udeg: i32) -> String {
    let neg = udeg < 0;
    let a = (udeg as i64).unsigned_abs();
    let (i, f) = (a / 1_000_000, a % 1_000_000);
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(&i.to_string());
    if f != 0 {
        s.push('.');
        s.push_str(format!("{f:06}").trim_end_matches('0'));
    }
    s
}

/// A decimal number text (`-1.25`, `.5`, `3`) to nanometres when the text is millimetres, or to
/// the value times `scale` rounded (inches: scale 25_400_000). `None` when it is not a number.
pub fn parse_len(s: &str, scale: f64) -> Option<Nm> {
    let v: f64 = s.trim().parse().ok()?;
    Some((v * scale).round() as Nm)
}

/// Gerber attribute string escapes: `\uXXXX` for the characters the syntax reserves
/// (`,` `*` `%` `\`) and everything outside printable ASCII.
pub fn escape_attr(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, ',' | '*' | '%' | '\\') || !(' '..='~').contains(&c) {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                o.push_str(&format!("\\u{:04X}", u));
            }
        } else {
            o.push(c);
        }
    }
    o
}

pub fn unescape_attr(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut units: Vec<u16> = Vec::new();
    let mut out = String::new();
    let flush = |units: &mut Vec<u16>, out: &mut String| {
        if !units.is_empty() {
            out.push_str(&String::from_utf16_lossy(units));
            units.clear();
        }
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' && i + 5 < b.len() + 0 && b[i + 1] == 'u' {
            let h: String = b[i + 2..i + 6].iter().collect();
            if let Ok(v) = u16::from_str_radix(&h, 16) {
                units.push(v);
                i += 6;
                continue;
            }
        }
        flush(&mut units, &mut out);
        out.push(b[i]);
        i += 1;
    }
    flush(&mut units, &mut out);
    out
}

/// CSV cell: quoted when it holds the delimiter, a quote or a line break.
pub fn csv_cell(s: &str, delim: char) -> String {
    if s.contains(delim) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// S46.R91: quote every value, doubling whole string delimiters (which may be empty
/// or multiple characters). The original `csv_cell` keeps its conditional-quoting contract.
pub fn csv_cell_quoted(s: &str, delimiter: &str, strip_breaks: bool, strip_tabs: bool) -> String {
    let value: String = s.chars().filter(|c| {
        (!strip_breaks || !matches!(c, '\r' | '\n')) && (!strip_tabs || *c != '\t')
    }).collect();
    if delimiter.is_empty() { return value; }
    format!("{delimiter}{}{delimiter}", value.replace(delimiter, &delimiter.repeat(2)))
}

/// Splits CSV text into rows of cells (RFC 4180 quoting).
pub fn csv_rows(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        any = true;
        if quoted {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    cell.push('"');
                    it.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            row.push(std::mem::take(&mut cell));
            rows.push(std::mem::take(&mut row));
            any = false;
        } else {
            cell.push(c);
        }
    }
    if any {
        row.push(cell);
        rows.push(row);
    }
    rows
}
