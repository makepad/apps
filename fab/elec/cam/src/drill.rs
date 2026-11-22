//! Excellon / XNC drill files: writer and reader.
//!
//! The writer emits `M48` header, tool table (`T1C0.300`), `%`, then per tool the round holes
//! `X..Y..` and slots as `X..Y..G85X..Y..` or tool-down routed moves. Units are `METRIC` or `INCH`; coordinates are
//! decimal numbers, or fixed `int.frac` digits with leading or trailing zeros omitted as the
//! header (`LZ` keeps leading zeros and omits trailing ones; `TZ` the reverse) says. The file
//! function travels as a `; #@! TF.FileFunction,...` comment.

use crate::error::CamError;
use crate::image::{DrillHole, DrillImage, FileAttr};
use crate::text::{escape_attr, unescape_attr};
use fab_elec_core::{Nm, P};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Zeros {
    /// Explicit decimal points.
    Decimal,
    /// Fixed width, leading zeros kept, trailing omitted (`LZ`).
    KeepLeading,
    /// Fixed width, trailing zeros kept, leading omitted (`TZ`).
    KeepTrailing,
    /// Fixed width with every zero kept (no units suffix).
    Keep,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DrillOpts {
    pub inch: bool,
    pub zeros: Zeros,
    /// Digits before and after the point in the fixed forms (metric 3.3, inch 2.4 are usual).
    pub int_digits: u8,
    pub frac_digits: u8,
}

impl Default for DrillOpts {
    fn default() -> DrillOpts {
        DrillOpts { inch: false, zeros: Zeros::Decimal, int_digits: 3, frac_digits: 3 }
    }
}

const NM_PER_INCH: f64 = 25_400_000.0;

impl DrillOpts {
    /// The reference integer layout. Explicit digit counts remain a Fab extension.
    pub fn for_units(inch: bool) -> Self {
        let (int_digits, frac_digits) = if inch { (2, 4) } else { (3, 3) };
        Self { inch, zeros: Zeros::Decimal, int_digits, frac_digits }
    }
    fn unit_nm(&self) -> f64 {
        if self.inch { NM_PER_INCH } else { 1e6 }
    }
    /// A coordinate or diameter in the file's unit as a number of decimal text.
    fn dec(&self, nm: i64) -> String {
        let v = nm as f64 / self.unit_nm();
        let s = format!("{v:.6}");
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        if s.is_empty() || s == "-0" { "0".into() } else { s }
    }
    fn coord(&self, nm: i64) -> String {
        match self.zeros {
            Zeros::Decimal => self.dec(nm),
            z => {
                let scale = 10f64.powi(self.frac_digits as i32);
                let n = (nm as f64 / self.unit_nm() * scale).round() as i64;
                let width = self.int_digits as usize + self.frac_digits as usize;
                let neg = n < 0;
                let digits = format!("{:0width$}", n.unsigned_abs(), width = width);
                let digits = match z {
                    Zeros::KeepLeading => digits.trim_end_matches('0').to_string(),
                    Zeros::KeepTrailing => digits.trim_start_matches('0').to_string(),
                    _ => digits,
                };
                let digits = if digits.is_empty() { "0".to_string() } else { digits };
                if neg { format!("-{digits}") } else { digits }
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SlotMode {
    Route,
    Alternate,
}

/// Additive writer controls: `DrillOpts` struct literals and output-space callers stay valid.
#[derive(Clone, Debug)]
pub struct ExcellonOptions {
    pub format: DrillOpts,
    pub slots: SlotMode,
    pub minimal_header: bool,
    pub origin: P,
    /// Board exports negate Y unless mirrored; legacy callers already supply output axes.
    pub negate_y: bool,
    /// Reference decimal coordinates/tool sizes use 3 mm or 4 inch places. `None`
    /// preserves the legacy writer's six-place decimal precision.
    pub decimal_places: Option<u8>,
}

impl Default for ExcellonOptions {
    fn default() -> Self {
        Self { format: DrillOpts::default(), slots: SlotMode::Alternate,
            minimal_header: false, origin: P::ZERO, negate_y: false, decimal_places: None }
    }
}

/// Compatibility entry point: holes are already in file coordinates.
pub fn write_excellon(holes: &[DrillHole], file_attrs: &[FileAttr], opts: &DrillOpts) -> Vec<u8> {
    write_excellon_with_options(holes, file_attrs, &ExcellonOptions {
        format: opts.clone(), ..ExcellonOptions::default()
    })
}

pub fn write_excellon_with_options(holes: &[DrillHole], file_attrs: &[FileAttr], options: &ExcellonOptions) -> Vec<u8> {
    let opts = &options.format;
    let decimal = |nm: i64, tool: bool| {
        if let Some(places) = options.decimal_places {
            let mut s = format!("{:.*}", places as usize, nm as f64 / opts.unit_nm());
            if !tool {
                if s.contains('.') {
                    while s.ends_with('0') { s.pop(); }
                    if s.ends_with('.') { s.push('0'); }
                } else { s.push_str(".0"); }
                if s == "-0.0" { s = "0.0".into(); }
            }
            s
        } else {
            let mut s = opts.dec(nm);
            if !tool && !s.contains('.') { s.push_str(".0"); }
            s
        }
    };
    let coord = |nm| if opts.zeros == Zeros::Decimal { decimal(nm, false) } else { opts.coord(nm) };
    let xy = |p: P| {
        let x = p.x as i64 - options.origin.x as i64;
        let y = p.y as i64 - options.origin.y as i64;
        format!("X{}Y{}", coord(x), coord(if options.negate_y { -y } else { y }))
    };
    // One per-export sort groups tools and hits; avoid scanning all holes for every tool.
    let mut ordered: Vec<_> = holes.iter().collect();
    ordered.sort_by_key(|h| (h.dia, h.at.x, h.at.y, h.to.map(|p| (p.x, p.y))));
    let mut tools: Vec<Nm> = ordered.iter().map(|h| h.dia).collect();
    tools.dedup();
    let mut s = String::from("M48\n");
    if !options.minimal_header {
        s.push_str(concat!("; DRILL file generated by Fab ", env!("CARGO_PKG_VERSION"), "\n"));
        let digits = if opts.zeros == Zeros::Decimal { "-:-".into() } else { format!("{}:{}", opts.int_digits, opts.frac_digits) };
        let zeros = match opts.zeros {
            Zeros::Decimal => "decimal", Zeros::KeepLeading => "suppress trailing zeros",
            Zeros::KeepTrailing => "suppress leading zeros", Zeros::Keep => "keep zeros",
        };
        s.push_str(&format!("; FORMAT={{{digits}/ absolute / {} / {zeros}}}\n", if opts.inch { "inch" } else { "metric" }));
        for (k, v) in file_attrs {
            if k == "CreationDate" { continue; }
            s.push_str(&format!("; #@! TF.{}", k));
            for f in v {
                s.push(',');
                s.push_str(&escape_attr(f));
            }
            s.push('\n');
        }
        s.push_str("FMAT,2\n");
    }
    let unit = if opts.inch { "INCH" } else { "METRIC" };
    match opts.zeros {
        Zeros::Decimal | Zeros::Keep => s.push_str(unit),
        Zeros::KeepLeading => s.push_str(&format!("{unit},LZ")),
        Zeros::KeepTrailing => s.push_str(&format!("{unit},TZ")),
    }
    let standard = DrillOpts::for_units(opts.inch);
    // Explicit layouts on the units line keep the free-digit extension readable, even
    // in a minimal header. Reference layouts have no extra units-line field.
    if opts.zeros != Zeros::Decimal && (opts.int_digits, opts.frac_digits) != (standard.int_digits, standard.frac_digits) {
        s.push_str(&format!(",{}.{}", "0".repeat(opts.int_digits as usize), "0".repeat(opts.frac_digits as usize)));
    }
    s.push('\n');
    for (i, d) in tools.iter().enumerate() {
        s.push_str(&format!("T{}C{}\n", i + 1, decimal(*d as i64, true)));
    }
    s.push_str("%\nG90\nG05\n");
    let mut remaining = ordered.as_slice();
    for (i, d) in tools.iter().enumerate() {
        s.push_str(&format!("T{}\n", i + 1));
        let end = remaining.partition_point(|h| h.dia == *d);
        let (mine, rest) = remaining.split_at(end);
        remaining = rest;
        for h in mine {
            match (h.to, options.slots) {
                (Some(t), SlotMode::Route) => s.push_str(&format!("G00{}\nM15\nG01{}\nM16\nG05", xy(h.at), xy(t))),
                (Some(t), SlotMode::Alternate) => s.push_str(&format!("{}G85{}", xy(h.at), xy(t))),
                (None, _) => s.push_str(&xy(h.at)),
            }
            s.push('\n');
        }
    }
    s.push_str("M30\n");
    s.into_bytes()
}

pub fn read_excellon(bytes: &[u8]) -> Result<DrillImage, CamError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CamError::parse(1, "not text"))?;
    let mut img = DrillImage::default();
    let mut header = true;
    let mut zeros = Zeros::Decimal;
    let (mut int_d, mut frac_d) = (3u8, 3u8);
    let mut unit_nm = 1e6f64;
    let mut cur: Option<(u32, Nm)> = None;
    let mut ended = false;
    let mut saw_header = false;
    let mut last = (0 as Nm, 0 as Nm);
    let mut format_digits = None;
    let mut format_zeros = None;
    let mut routing = false;
    for (ln, raw) in text.lines().enumerate() {
        let at = ln + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(c) = line.strip_prefix(';') {
            if let Some(a) = c.trim().strip_prefix("#@! TF.") {
                let mut it = a.split(',').map(unescape_attr);
                let k = it.next().unwrap_or_default();
                img.file_attrs.push((k, it.collect()));
            } else if let Some(f) = c.trim().strip_prefix("FILE_FORMAT=") {
                if let Some((a, b)) = f.split_once(':') {
                    int_d = a.trim().parse().unwrap_or(int_d);
                    frac_d = b.trim().parse().unwrap_or(frac_d);
                    format_digits = Some((int_d, frac_d));
                }
            } else if let Some(f) = c.trim().strip_prefix("FORMAT={") {
                let fields: Vec<_> = f.trim_end_matches('}').split('/').map(str::trim).collect();
                if let Some((a, b)) = fields.first().and_then(|s| s.split_once(':')) {
                    if let (Ok(a), Ok(b)) = (a.parse(), b.parse()) { format_digits = Some((a, b)); }
                }
                format_zeros = match fields.last().copied() {
                    Some("decimal") => Some(Zeros::Decimal),
                    Some("keep zeros") => Some(Zeros::Keep),
                    Some("suppress leading zeros") => Some(Zeros::KeepTrailing),
                    Some("suppress trailing zeros") => Some(Zeros::KeepLeading),
                    _ => None,
                };
            }
            continue;
        }
        if ended {
            return Err(CamError::parse(at, "text after M30"));
        }
        if line == "M48" {
            saw_header = true;
            continue;
        }
        if header {
            if line == "%" || line == "M95" {
                header = false;
                continue;
            }
            if line.starts_with("FMAT") || line == "ICI,OFF" || line.starts_with("VER") {
                continue;
            }
            let head = line.split(',').next().unwrap_or("");
            if head == "METRIC" || head == "INCH" || head == "M71" || head == "M72" {
                let inch = head == "INCH" || head == "M72";
                img.inch = inch;
                unit_nm = if inch { NM_PER_INCH } else { 1e6 };
                (int_d, frac_d) = format_digits.unwrap_or(if inch { (2, 4) } else { (3, 3) });
                let mut seen_zero = false;
                for f in line.split(',').skip(1) {
                    match f.trim() {
                        "LZ" => {
                            zeros = Zeros::KeepLeading;
                            seen_zero = true;
                        }
                        "TZ" => {
                            zeros = Zeros::KeepTrailing;
                            seen_zero = true;
                        }
                        fmt if fmt.contains('.') && fmt.chars().all(|c| c == '0' || c == '.') => {
                            let (a, b) = fmt.split_once('.').unwrap();
                            // the explicit form 000.000 is the decimal-point layout: digit counts only
                            int_d = a.len() as u8;
                            frac_d = b.len() as u8;
                        }
                        _ => {}
                    }
                }
                if !seen_zero {
                    zeros = format_zeros.unwrap_or(Zeros::Keep);
                }
                continue;
            }
            if let Some(t) = line.strip_prefix('T') {
                let nd = t.chars().take_while(|c| c.is_ascii_digit()).count();
                let n: u32 = t[..nd].parse().map_err(|_| CamError::parse(at, "tool number"))?;
                let rest = &t[nd..];
                let c = rest.strip_prefix('C').ok_or_else(|| CamError::unsupported(at, "tool parameters other than C"))?;
                let end = c.find(|ch: char| !(ch.is_ascii_digit() || ch == '.')).unwrap_or(c.len());
                let d: f64 = c[..end].parse().map_err(|_| CamError::parse(at, "tool diameter"))?;
                let dia = (d * unit_nm).round() as Nm;
                img.tools.push((n, dia));
                continue;
            }
            return Err(CamError::unsupported(at, format!("header line {line:?}")));
        }
        match line {
            "G90" | "G05" => continue,
            "M15" => { routing = true; continue; }
            "M16" => { routing = false; continue; }
            "G91" => return Err(CamError::unsupported(at, "incremental coordinates")),
            "M30" | "M00" => {
                ended = true;
                continue;
            }
            _ => {}
        }
        if let Some(pos) = line.strip_prefix("G00") {
            let conv = |s: &str| coord_value(s, zeros, int_d, frac_d, unit_nm)
                .ok_or_else(|| CamError::parse(at, format!("coordinate {s:?}")));
            last = xy(pos, &conv, last)?;
            continue;
        }
        if let Some(pos) = line.strip_prefix("G01") {
            if !routing { return Err(CamError::parse(at, "route without M15")); }
            let (_, dia) = cur.ok_or_else(|| CamError::parse(at, "route before any tool"))?;
            let conv = |s: &str| coord_value(s, zeros, int_d, frac_d, unit_nm)
                .ok_or_else(|| CamError::parse(at, format!("coordinate {s:?}")));
            let next = xy(pos, &conv, last)?;
            img.holes.push(DrillHole { at: P::new(last.0, last.1), dia, to: Some(P::new(next.0, next.1)) });
            last = next;
            continue;
        }
        if let Some(t) = line.strip_prefix('T') {
            if t.chars().all(|c| c.is_ascii_digit()) {
                let n: u32 = t.parse().map_err(|_| CamError::parse(at, "tool number"))?;
                let dia = img.tools.iter().find(|(k, _)| *k == n).map(|(_, d)| *d).ok_or_else(|| CamError::parse(at, format!("tool T{n} is not defined")))?;
                cur = Some((n, dia));
                continue;
            }
        }
        if line.starts_with('X') || line.starts_with('Y') {
            let (_, dia) = cur.ok_or_else(|| CamError::parse(at, "hole before any tool"))?;
            let (a, slot) = match line.split_once("G85") {
                Some((a, b)) => (a, Some(b)),
                None => (line, None),
            };
            let conv = |s: &str| coord_value(s, zeros, int_d, frac_d, unit_nm).ok_or_else(|| CamError::parse(at, format!("coordinate {s:?}")));
            let (x, y) = xy(a, &conv, last)?;
            last = (x, y);
            let to = match slot {
                Some(b) => {
                    let (x2, y2) = xy(b, &conv, last)?;
                    last = (x2, y2);
                    Some(P::new(x2, y2))
                }
                None => None,
            };
            img.holes.push(DrillHole { at: P::new(x, y), dia, to });
            continue;
        }
        return Err(CamError::unsupported(at, format!("command {line:?}")));
    }
    if !saw_header {
        return Err(CamError::parse(1, "no M48 header"));
    }
    if !ended {
        return Err(CamError::parse(text.lines().count(), "no end of file (M30)"));
    }
    Ok(img)
}

fn xy(s: &str, conv: &dyn Fn(&str) -> Result<Nm, CamError>, last: (Nm, Nm)) -> Result<(Nm, Nm), CamError> {
    let (mut x, mut y) = last;
    let mut rest = s;
    while !rest.is_empty() {
        let axis = rest.chars().next().unwrap();
        let body = &rest[1..];
        let end = body.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(body.len());
        let v = conv(&body[..end])?;
        match axis {
            'X' => x = v,
            'Y' => y = v,
            _ => return Err(CamError::parse(0, format!("axis {axis}"))),
        }
        rest = &body[end..];
    }
    Ok((x, y))
}

fn coord_value(s: &str, zeros: Zeros, int_d: u8, frac_d: u8, unit_nm: f64) -> Option<Nm> {
    if s.contains('.') || zeros == Zeros::Decimal {
        let v: f64 = s.parse().ok()?;
        return Some((v * unit_nm).round() as Nm);
    }
    let (neg, d) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let width = int_d as usize + frac_d as usize;
    let padded = match zeros {
        Zeros::KeepLeading => format!("{d:0<width$}"),
        _ => format!("{d:0>width$}"),
    };
    let n: i64 = padded.parse().ok()?;
    let v = n as f64 / 10f64.powi(frac_d as i32) * unit_nm;
    Some(if neg { -v } else { v }.round() as Nm)
}
