//! A flat configuration: option key → value, as profile text.
//!
//! Values stay in the profile's own text form (`"0.8"`, `"50%"`, `"Auto
//! Lift"`) so a config round-trips losslessly; typed reads parse on demand.

use crate::options::{options, Kind};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Scalar(String),
    Vector(Vec<String>),
}

impl Value {
    pub fn as_vec(&self) -> Vec<String> {
        match self {
            Value::Scalar(s) => vec![s.clone()],
            Value::Vector(v) => v.clone(),
        }
    }
    pub fn len(&self) -> usize {
        match self {
            Value::Scalar(_) => 1,
            Value::Vector(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Element `i`, falling back to element 0 (and to "" for an empty vector).
    pub fn at(&self, i: usize) -> &str {
        match self {
            Value::Scalar(s) => s,
            Value::Vector(v) => v.get(i).or_else(|| v.first()).map(|s| s.as_str()).unwrap_or(""),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub map: BTreeMap<String, Value>,
}

/// Parse the leading number of a profile value ("0.8", "50%", "1").
pub fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim().trim_end_matches('%');
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn set(&mut self, key: &str, v: Value) {
        self.map.insert(key.to_string(), v);
    }

    pub fn set_str(&mut self, key: &str, v: impl Into<String>) {
        self.map.insert(key.to_string(), Value::Scalar(v.into()));
    }

    pub fn set_vec(&mut self, key: &str, v: Vec<String>) {
        self.map.insert(key.to_string(), Value::Vector(v));
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.map.remove(key)
    }

    /// Raw text of element `i` (element 0 when out of range).
    pub fn raw(&self, key: &str, i: usize) -> &str {
        self.map.get(key).map(|v| v.at(i)).unwrap_or("")
    }

    pub fn str(&self, key: &str) -> &str {
        self.raw(key, 0)
    }

    pub fn f64_at(&self, key: &str, i: usize) -> f64 {
        parse_num(self.raw(key, i)).unwrap_or(0.0)
    }

    pub fn f64(&self, key: &str) -> f64 {
        self.f64_at(key, 0)
    }

    pub fn i64_at(&self, key: &str, i: usize) -> i64 {
        self.f64_at(key, i).round() as i64
    }

    pub fn bool_at(&self, key: &str, i: usize) -> bool {
        matches!(self.raw(key, i).trim(), "1" | "true")
    }

    pub fn bool(&self, key: &str) -> bool {
        self.bool_at(key, 0)
    }

    pub fn is_percent(&self, key: &str, i: usize) -> bool {
        self.raw(key, i).trim().ends_with('%')
    }

    /// A float-or-percent value resolved against `base` (percent of base).
    pub fn abs_or_pct(&self, key: &str, i: usize, base: f64) -> f64 {
        let v = self.f64_at(key, i);
        if self.is_percent(key, i) {
            base * v / 100.0
        } else {
            v
        }
    }

    pub fn vec_len(&self, key: &str) -> usize {
        self.map.get(key).map(|v| v.len()).unwrap_or(0)
    }

    pub fn points(&self, key: &str) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        if let Some(v) = self.map.get(key) {
            for e in v.as_vec() {
                for p in e.split(',') {
                    if let Some((a, b)) = p.split_once('x') {
                        if let (Ok(x), Ok(y)) = (a.trim().parse(), b.trim().parse()) {
                            out.push((x, y));
                        }
                    }
                }
            }
        }
        out
    }

    /// Point groups (`a,b,c#d,e,f` in text, one string per group in JSON).
    pub fn point_groups(&self, key: &str) -> Vec<Vec<(f64, f64)>> {
        let mut out = Vec::new();
        if let Some(v) = self.map.get(key) {
            for e in v.as_vec() {
                for g in e.split('#') {
                    let mut pts = Vec::new();
                    for p in g.split(',') {
                        if let Some((a, b)) = p.split_once('x') {
                            if let (Ok(x), Ok(y)) = (a.trim().parse(), b.trim().parse()) {
                                pts.push((x, y));
                            }
                        }
                    }
                    out.push(pts);
                }
            }
        }
        out
    }

    /// Overlay `o` onto self.
    pub fn apply(&mut self, o: &Config) {
        for (k, v) in &o.map {
            self.map.insert(k.clone(), v.clone());
        }
    }

    /// The `; key = value` lines of a G-code config block, sorted by key.
    pub fn config_block_lines(&self, out: &mut String) {
        for (k, v) in &self.map {
            out.push_str("; ");
            out.push_str(&one_line(k));
            out.push_str(" = ");
            out.push_str(&one_line(&serialize(k, v)));
            out.push('\n');
        }
    }
}

/// `text` on one line of a G-code comment: a line break or other control character a field
/// holds is written as an escape, so no field can end its comment and start a command. Ordinary
/// text comes back as it is.
pub fn one_line(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() && c != '\t' => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.into()
}

/// C-style escaping of a string value.
pub fn escape_cstyle(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

fn needs_quotes(s: &str) -> bool {
    s.chars().any(|c| matches!(c, ' ' | ';' | '"' | '\\' | '\n' | '\r' | '\t'))
}

/// A string vector in profile text: `;`-separated, elements quoted when they
/// carry separators or spaces; a lone empty element is written `""` so it
/// differs from an empty vector.
pub fn serialize_strings(v: &[String]) -> String {
    if v.len() == 1 && v[0].is_empty() {
        return "\"\"".to_string();
    }
    let mut out = String::new();
    for (i, e) in v.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        if needs_quotes(e) {
            out.push('"');
            out.push_str(&escape_cstyle(e));
            out.push('"');
        } else {
            out.push_str(e);
        }
    }
    out
}

/// Parse a string vector from profile text (inverse of `serialize_strings`).
pub fn parse_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    if b.is_empty() {
        return out;
    }
    loop {
        let mut cur = String::new();
        if i < b.len() && b[i] == '"' {
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                    cur.push(match b[i] {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        c => c,
                    });
                } else {
                    cur.push(b[i]);
                }
                i += 1;
            }
            i += 1;
            while i < b.len() && b[i] != ';' {
                i += 1;
            }
        } else {
            while i < b.len() && b[i] != ';' {
                cur.push(b[i]);
                i += 1;
            }
        }
        out.push(cur);
        if i >= b.len() {
            break;
        }
        i += 1; // ';'
        if i >= b.len() {
            out.push(String::new());
            break;
        }
    }
    out
}

pub fn unescape_cstyle(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A number in its canonical profile form ("1.0" → "1"); other text as is.
fn norm_num(s: &str) -> String {
    match s.trim().parse::<f64>() {
        Ok(v) if v.is_finite() => format!("{v}"),
        _ => s.to_string(),
    }
}

fn norm_pct(s: &str) -> String {
    let t = s.trim();
    if t == "nil" || t.is_empty() {
        return t.to_string();
    }
    let n = norm_num(t.trim_end_matches('%'));
    format!("{n}%")
}

fn norm_float_or_pct(s: &str) -> String {
    let t = s.trim();
    if t.ends_with('%') {
        norm_pct(t)
    } else {
        norm_num(t)
    }
}

/// Profile text of a value, by its option type (numbers in canonical form).
pub fn serialize(key: &str, v: &Value) -> String {
    let kind = options().kind(key);
    let elem = |e: &str| -> String {
        match kind {
            Some(Kind::Float) | Some(Kind::Floats) | Some(Kind::Int) | Some(Kind::Ints) => norm_num(e),
            Some(Kind::Percent) | Some(Kind::Percents) => norm_pct(e),
            Some(Kind::FloatOrPercent) | Some(Kind::FloatsOrPercents) => norm_float_or_pct(e),
            Some(Kind::Point) => e.replace('x', ","),
            _ => e.to_string(),
        }
    };
    match v {
        Value::Scalar(s) => match kind {
            Some(Kind::Str) | None => escape_cstyle(s),
            _ => elem(s),
        },
        Value::Vector(items) => match kind {
            Some(Kind::Strs) => serialize_strings(items),
            Some(Kind::PointsGroups) => items.join("#"),
            Some(Kind::Point) => items.join(",").replace('x', ","),
            _ => items.iter().map(|e| elem(e)).collect::<Vec<_>>().join(","),
        },
    }
}

/// Parse profile text into a value of the key's type.
pub fn parse_text(key: &str, text: &str) -> Value {
    match options().kind(key) {
        Some(Kind::Strs) => Value::Vector(parse_strings(text)),
        Some(Kind::PointsGroups) => Value::Vector(if text.is_empty() { Vec::new() } else { text.split('#').map(|s| s.to_string()).collect() }),
        Some(k) if k.is_vector() => Value::Vector(if text.is_empty() { Vec::new() } else { text.split(',').map(|s| s.to_string()).collect() }),
        Some(Kind::Str) | None => Value::Scalar(unescape_cstyle(text)),
        _ => Value::Scalar(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_lines_stay_one_comment_each_whatever_a_field_holds() {
        let mut c = Config::new();
        c.set_vec("filament_density", vec!["1.24\nM104 S400".into()]);
        c.set_str("layer_height", "0.2\rG28");
        c.set_vec("made_up_list", vec!["a\nG28".into(), "b\x0cG28".into()]);
        c.set_str("made_up\nG28", "1");
        c.set_vec("filament_start_gcode", vec!["M106 S0\nG28".into()]);
        c.set_str("plain_text", "a\tb \\ c");
        let mut out = String::new();
        c.config_block_lines(&mut out);
        assert_eq!(out.lines().count(), c.map.len(), "{out}");
        assert!(out.lines().all(|l| l.starts_with("; ")), "{out}");
        assert!(!out.chars().any(|ch| ch.is_control() && ch != '\n' && ch != '\t'), "{out}");
        assert_eq!(one_line("0.2 \\ a\tb"), "0.2 \\ a\tb", "ordinary text stays as it is");
    }
}
