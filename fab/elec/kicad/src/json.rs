//! Comment-tolerant JSON with located values and an explicitly stacked reader.

use crate::report::Refusal;
use std::collections::HashMap;

pub type Location = (u32, u32);

/// Objects retain the position of the first key and the value of its last occurrence.
#[derive(Debug)]
pub enum Kind {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object { entries: Vec<(String, Value)>, indices: HashMap<String, usize> },
}

#[derive(Debug)]
pub struct Value {
    pub at: Location,
    pub kind: Kind,
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match &self.kind {
            Kind::Object { entries, indices } => indices.get(key).and_then(|&i| entries.get(i)).map(|e| &e.1),
            _ => None,
        }
    }
    pub fn number(&self) -> Option<f64> { if let Kind::Number(n) = self.kind { Some(n) } else { None } }
    pub fn string(&self) -> Option<&str> { if let Kind::String(s) = &self.kind { Some(s) } else { None } }
    pub fn boolean(&self) -> Option<bool> { if let Kind::Bool(b) = self.kind { Some(b) } else { None } }
    pub fn array(&self) -> Option<&[Value]> { if let Kind::Array(a) = &self.kind { Some(a) } else { None } }
    pub fn object(&self) -> Option<&[(String, Value)]> {
        if let Kind::Object { entries, .. } = &self.kind { Some(entries) } else { None }
    }
    pub fn is_empty(&self) -> bool {
        match &self.kind {
            Kind::Null => true, Kind::String(s) => s.is_empty(), Kind::Array(a) => a.is_empty(),
            Kind::Object { entries, .. } => entries.is_empty(), _ => false,
        }
    }
    fn drain(&mut self, pending: &mut Vec<Value>) {
        match std::mem::replace(&mut self.kind, Kind::Null) {
            Kind::Array(mut a) => pending.append(&mut a),
            Kind::Object { entries, .. } => pending.extend(entries.into_iter().map(|(_, v)| v)),
            _ => (),
        }
    }
}

// Destruction, including replacement of a duplicate key or an error's partial tree, is iterative.
impl Drop for Value {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        self.drain(&mut pending);
        while let Some(mut value) = pending.pop() { value.drain(&mut pending); }
    }
}
impl Clone for Value {
    fn clone(&self) -> Self {
        let mut pending = vec![(self, false)];
        let mut built = Vec::new();
        while let Some((v, ready)) = pending.pop() {
            if !ready {
                pending.push((v, true));
                match &v.kind {
                    Kind::Array(a) => pending.extend(a.iter().rev().map(|c| (c, false))),
                    Kind::Object { entries, .. } => pending.extend(entries.iter().rev().map(|(_, c)| (c, false))),
                    _ => (),
                }
                continue;
            }
            let kind = match &v.kind {
                Kind::Null => Kind::Null, Kind::Bool(b) => Kind::Bool(*b),
                Kind::Number(n) => Kind::Number(*n), Kind::String(s) => Kind::String(s.clone()),
                Kind::Array(a) => Kind::Array(built.split_off(built.len() - a.len())),
                Kind::Object { entries, indices } => {
                    let children = built.split_off(built.len() - entries.len());
                    Kind::Object { entries: entries.iter().map(|(k, _)| k.clone()).zip(children).collect(), indices: indices.clone() }
                }
            };
            built.push(Value { at: v.at, kind });
        }
        built.pop().unwrap_or(Value { at: self.at, kind: Kind::Null })
    }
}

struct Reader<'a> { bytes: &'a [u8], pos: usize, line: u32, start: usize }
struct Frame { value: Value, state: State }
enum State { ArrayFirst, ArrayValue, ArrayComma, ObjectFirst, ObjectKey, ObjectColon(String), ObjectValue(String), ObjectComma }

/// Read exactly one JSON value. Neither the reader nor its tree destruction uses call-stack depth.
pub fn parse(bytes: &[u8]) -> Result<Value, Refusal> {
    let mut r = Reader { bytes, pos: usize::from(bytes.starts_with(&[0xef, 0xbb, 0xbf])) * 3, line: 1, start: 0 };
    let mut stack: Vec<Frame> = Vec::new();
    let mut complete: Option<Value> = None;
    loop {
        if let Some(value) = complete.take() {
            if let Some(parent) = stack.last_mut() {
                match (&mut parent.value.kind, std::mem::replace(&mut parent.state, State::ArrayComma)) {
                    (Kind::Array(a), State::ArrayFirst | State::ArrayValue) => a.push(value),
                    (Kind::Object { entries, indices }, State::ObjectValue(key)) => {
                        if let Some(&i) = indices.get(&key) { entries[i].1 = value; }
                        else { indices.insert(key.clone(), entries.len()); entries.push((key, value)); }
                        parent.state = State::ObjectComma;
                    }
                    _ => return Err(r.error("unexpected value")),
                }
            } else {
                r.space()?;
                if r.peek().is_some() { return Err(r.error("trailing content after JSON value")); }
                return Ok(value);
            }
        }
        r.space()?;
        if let Some(frame) = stack.last_mut() {
            match &frame.state {
                State::ArrayFirst if r.peek() == Some(b']') => {
                    r.bump(); complete = stack.pop().map(|f| f.value); continue;
                }
                State::ArrayComma => match r.peek() {
                    Some(b',') => { r.bump(); frame.state = State::ArrayValue; continue; }
                    Some(b']') => { r.bump(); complete = stack.pop().map(|f| f.value); continue; }
                    _ => return Err(r.error("expected ',' or ']'")),
                },
                State::ObjectFirst if r.peek() == Some(b'}') => {
                    r.bump(); complete = stack.pop().map(|f| f.value); continue;
                }
                State::ObjectFirst | State::ObjectKey => {
                    if r.peek() != Some(b'"') { return Err(r.error("expected object key")); }
                    frame.state = State::ObjectColon(r.string()?); continue;
                }
                State::ObjectColon(_) => {
                    if r.peek() != Some(b':') { return Err(r.error("expected ':'")); }
                    r.bump();
                    if let State::ObjectColon(key) = std::mem::replace(&mut frame.state, State::ObjectComma) {
                        frame.state = State::ObjectValue(key);
                    }
                    continue;
                }
                State::ObjectComma => match r.peek() {
                    Some(b',') => { r.bump(); frame.state = State::ObjectKey; continue; }
                    Some(b'}') => { r.bump(); complete = stack.pop().map(|f| f.value); continue; }
                    _ => return Err(r.error("expected ',' or '}'")),
                },
                _ => (),
            }
        }
        let at = r.at();
        let kind = match r.peek() {
            Some(b'[') => {
                r.bump(); stack.push(Frame { value: Value { at, kind: Kind::Array(Vec::new()) }, state: State::ArrayFirst }); continue;
            }
            Some(b'{') => {
                r.bump(); stack.push(Frame { value: Value { at, kind: Kind::Object { entries: Vec::new(), indices: HashMap::new() } }, state: State::ObjectFirst }); continue;
            }
            Some(b'"') => Kind::String(r.string()?),
            Some(b'-' | b'0'..=b'9') => Kind::Number(r.number()?),
            Some(b't') => { r.literal(b"true")?; Kind::Bool(true) }
            Some(b'f') => { r.literal(b"false")?; Kind::Bool(false) }
            Some(b'n') => { r.literal(b"null")?; Kind::Null }
            _ => return Err(r.error("expected JSON value")),
        };
        complete = Some(Value { at, kind });
    }
}
impl Reader<'_> {
    fn peek(&self) -> Option<u8> { self.bytes.get(self.pos).copied() }
    fn at(&self) -> Location { (self.line, (self.pos - self.start).saturating_add(1).min(u32::MAX as usize) as u32) }
    fn error(&self, message: &str) -> Refusal { Refusal::new(self.at(), message) }
    fn bump(&mut self) {
        if self.peek() == Some(b'\n') { self.line = self.line.saturating_add(1); self.start = self.pos + 1; }
        self.pos += 1;
    }
    fn space(&mut self) -> Result<(), Refusal> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.bump(),
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    let start = self.pos; let at = self.at();
                    while self.peek().is_some_and(|b| b != b'\n') { self.bump(); }
                    self.comment_utf8(start, at)?;
                }
                Some(b'/') if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    let start = self.pos; let at = self.at(); self.bump(); self.bump();
                    loop {
                        match (self.peek(), self.bytes.get(self.pos + 1)) {
                            (Some(b'*'), Some(b'/')) => { self.bump(); self.bump(); self.comment_utf8(start, at)?; break; }
                            (None, _) => return Err(Refusal::new(at, "unterminated block comment")),
                            _ => self.bump(),
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }
    fn comment_utf8(&self, start: usize, at: Location) -> Result<(), Refusal> {
        if let Err(error) = std::str::from_utf8(&self.bytes[start..self.pos]) {
            let mut at = at;
            for &b in &self.bytes[start..start + error.valid_up_to()] {
                if b == b'\n' { at.0 = at.0.saturating_add(1); at.1 = 1; }
                else { at.1 = at.1.saturating_add(1); }
            }
            return Err(Refusal::new(at, "invalid UTF-8 in comment"));
        }
        Ok(())
    }
    fn literal(&mut self, text: &[u8]) -> Result<(), Refusal> {
        for &b in text {
            if self.peek() != Some(b) { return Err(self.error("invalid JSON literal")); }
            self.bump();
        }
        Ok(())
    }
    fn digits(&mut self) -> Result<(), Refusal> {
        if !self.peek().is_some_and(|c| c.is_ascii_digit()) { return Err(self.error("expected digit")); }
        while self.peek().is_some_and(|c| c.is_ascii_digit()) { self.bump(); }
        Ok(())
    }
    fn number(&mut self) -> Result<f64, Refusal> {
        let start = self.pos; let at = self.at();
        if self.peek() == Some(b'-') { self.bump(); }
        if self.peek() == Some(b'0') { self.bump(); }
        else if self.peek().is_some_and(|c| (b'1'..=b'9').contains(&c)) { self.digits()?; }
        else { return Err(self.error("expected digit")); }
        if self.peek() == Some(b'.') { self.bump(); self.digits()?; }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.bump(); if matches!(self.peek(), Some(b'+' | b'-')) { self.bump(); } self.digits()?;
        }
        let value = std::str::from_utf8(&self.bytes[start..self.pos]).ok().and_then(|s| s.parse::<f64>().ok());
        value.filter(|n| n.is_finite()).ok_or_else(|| Refusal::new(at, "number exceeds finite float range"))
    }
    fn hex(&mut self) -> Result<u32, Refusal> {
        let mut n = 0;
        for _ in 0..4 {
            let d = self.peek().and_then(|c| (c as char).to_digit(16)).ok_or_else(|| self.error("expected four hexadecimal digits"))?;
            n = n * 16 + d; self.bump();
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, Refusal> {
        self.bump();
        let mut out = String::new();
        loop {
            let start = self.pos;
            while self.peek().is_some_and(|b| b >= 0x20 && b != b'"' && b != b'\\') { self.bump(); }
            let raw = std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| Refusal::new(
                (self.line, (start + e.valid_up_to() - self.start + 1).min(u32::MAX as usize) as u32), "invalid UTF-8 in string"))?;
            out.push_str(raw);
            match self.peek() {
                Some(b'"') => { self.bump(); return Ok(out); }
                Some(b'\\') => {
                    self.bump(); let at = self.at();
                    let c = self.peek().ok_or_else(|| self.error("unterminated escape"))?; self.bump();
                    match c {
                        b'"' | b'\\' | b'/' => out.push(c as char),
                        b'b' => out.push('\u{8}'), b'f' => out.push('\u{c}'), b'n' => out.push('\n'), b'r' => out.push('\r'), b't' => out.push('\t'),
                        b'u' => {
                            let mut n = self.hex()?;
                            if (0xd800..=0xdbff).contains(&n) {
                                self.literal(b"\\u")?; let low = self.hex()?;
                                if !(0xdc00..=0xdfff).contains(&low) { return Err(Refusal::new(at, "expected low surrogate")); }
                                n = 0x10000 + ((n - 0xd800) << 10) + low - 0xdc00;
                            }
                            out.push(char::from_u32(n).ok_or_else(|| Refusal::new(at, "lone surrogate"))?);
                        }
                        _ => return Err(Refusal::new(at, "unknown string escape")),
                    }
                }
                Some(_) => return Err(self.error("raw control character in string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_dialect_and_depth() {
        let value = parse(b"\xef\xbb\xbf/* comment */{\n\"a\":1, // line\n\"b\":\"\\uD83D\\uDE80\\n\\t\\b\\f\\r\\/\\\\\\\"\",\"a\":-2.5e1}").expect("comments and escapes parse");
        assert_eq!(value.get("a").and_then(Value::number), Some(-25.0), "duplicate key keeps last value");
        assert_eq!(value.object().expect("object")[0].0, "a", "duplicate retains first key position");
        assert_eq!(value.get("a").expect("a").at.0, 3, "duplicate keeps last value location");
        assert_eq!(value.get("b").and_then(Value::string), Some("🚀\n\t\u{8}\u{c}\r/\\\""), "surrogate pair and escapes decode");
        let e = parse(b"[\n 1e999]").expect_err("overflow refuses");
        assert_eq!((e.line, e.column), (2, 2), "number refusal has byte location");
        for source in [b"01".as_slice(), b"+1", b".5", b"NaN", b"0x1", b"[1,]", b"{\"a\":1,}", b"1 2", b"\"raw\ncontrol\"", b"\"\\uD800\"", b"\"\\uDC00\"", b"\"\xff\"", b"/*\xff*/0", b"//\xff\n0", b"/*", b""] {
            assert!(parse(source).is_err(), "malformed JSON refuses: {source:?}");
        }
        let depth = 100_000;
        let mut deep = vec![b'['; depth]; deep.push(b'0'); deep.extend(std::iter::repeat_n(b']', depth));
        let value = parse(&deep).expect("deep nesting parses iteratively");
        let mut leaf = &value;
        for _ in 0..depth { leaf = &leaf.array().expect("nested array")[0]; }
        assert_eq!(leaf.number(), Some(0.0), "deep leaf retained");
        drop(value);
    }
}
