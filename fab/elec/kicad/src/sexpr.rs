//! Shared byte lexer. Tokens borrow source ranges; only retained strings need decoding.

use crate::report::Refusal;
use std::collections::VecDeque;

/// The most recent board syntax understood by this reader.
pub const NEWEST_BOARD_VERSION: i64 = 20261031;
/// The grid conversion deliberately leaves ten units below the signed integer limit.
pub const NM_LIMIT: i32 = i32::MAX - 10;

/// Punctuation, unquoted atoms, strings, and end of input.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Kind { Open, Close, Bar, String, Number, Symbol, Eof }

/// A source byte range (strings exclude quotes) and its one-based starting position.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub column: u32,
}
impl Token {
    pub fn location(self) -> (u32, u32) { (self.line, self.column) }
}

/// A forward-only scanner with a single-token lookahead and switchable bar separators.
pub struct Lexer<'a> {
    bytes: &'a [u8],
    offset: usize,
    line: u32,
    line_start: usize,
    line_empty: bool,
    bar_aware: bool,
    peeked: Option<Token>,
    depth: usize,
    replay: VecDeque<Token>,
    // Kept texts that were not valid UTF-8: how many, and where the first was. They are decoded
    // lossily (old boards can carry single-byte encodings in texts); the parser reports them.
    lossy: std::cell::Cell<(u32, (u32, u32))>,
}
impl<'a> Lexer<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: if bytes.starts_with(&[0xef, 0xbb, 0xbf]) { 3 } else { 0 },
            line: 1, line_start: 0, line_empty: true, bar_aware: false, peeked: None, depth: 0, replay: VecDeque::new(),
            lossy: std::cell::Cell::new((0, (0, 0))) }
    }
    /// Change at a token boundary, before peeking the next atom.
    pub fn set_bar_aware(&mut self, enabled: bool) { self.bar_aware = enabled; }
    pub fn bar_aware(&self) -> bool { self.bar_aware }
    pub fn raw(&self, token: Token) -> &'a [u8] { self.bytes.get(token.start..token.end).unwrap_or_default() }
    pub fn peek(&mut self) -> Result<Token, Refusal> {
        if let Some(t) = self.peeked { return Ok(t); }
        let t = if let Some(t) = self.replay.pop_front() { t } else { self.scan()? };
        self.peeked = Some(t);
        Ok(t)
    }
    pub fn next_token(&mut self) -> Result<Token, Refusal> {
        if let Some(t) = self.peeked.take() { return Ok(t); }
        if let Some(t) = self.replay.pop_front() { return Ok(t); }
        self.scan()
    }
    // Conditional items are classified before converting their values. Reuse tokens, never re-lex bytes.
    pub(crate) fn replay_tokens(&mut self, tokens: impl IntoIterator<Item = Token>) {
        if let Some(t) = self.peeked.take() { self.replay.push_front(t); }
        let old = std::mem::take(&mut self.replay);
        self.replay.extend(tokens);
        self.replay.extend(old);
    }
    fn advance(&mut self) {
        if self.bytes.get(self.offset) == Some(&b'\n') {
            self.line = self.line.saturating_add(1);
            self.line_start = self.offset + 1;
            self.line_empty = true;
        }
        self.offset += 1;
    }
    fn scan(&mut self) -> Result<Token, Refusal> {
        loop {
            match self.bytes.get(self.offset).copied() {
                Some(c) if whitespace(c) => self.advance(),
                Some(b'#') if self.line_empty => {
                    while let Some(c) = self.bytes.get(self.offset).copied() {
                        self.advance();
                        if c == b'\n' { break; }
                    }
                }
                _ => break,
            }
        }
        let mut t = Token { kind: Kind::Eof, start: self.offset, end: self.offset, line: self.line,
            column: (self.offset - self.line_start).saturating_add(1).min(u32::MAX as usize) as u32 };
        let Some(c) = self.bytes.get(self.offset).copied() else {
            if self.depth != 0 { return Err(Refusal::new(t.location(), "end of input inside a list")); }
            return Ok(t);
        };
        self.line_empty = false;
        match c {
            b'(' => { t.kind = Kind::Open; self.depth += 1; self.advance(); }
            b')' => {
                if self.depth == 0 { return Err(Refusal::new(t.location(), "unbalanced closing parenthesis")); }
                t.kind = Kind::Close; self.depth -= 1; self.advance();
            }
            b'|' if self.bar_aware => { t.kind = Kind::Bar; self.advance(); }
            b'"' => {
                t.kind = Kind::String;
                self.advance();
                t.start = self.offset;
                loop {
                    match self.bytes.get(self.offset).copied() {
                        None | Some(b'\n') => return Err(Refusal::new(t.location(), "Unterminated delimited string")),
                        Some(b'"') => { t.end = self.offset; self.advance(); return Ok(t); }
                        Some(b'\\') => {
                            self.advance();
                            if matches!(self.bytes.get(self.offset), None | Some(b'\n')) {
                                return Err(Refusal::new(t.location(), "Unterminated delimited string"));
                            }
                            self.advance();
                        }
                        _ => self.advance(),
                    }
                }
            }
            _ => {
                while let Some(c) = self.bytes.get(self.offset).copied() {
                    if whitespace(c) || c == b'(' || c == b')' || (c == b'|' && self.bar_aware) { break; }
                    self.advance();
                }
                t.kind = if is_number(&self.bytes[t.start..self.offset]) { Kind::Number } else { Kind::Symbol };
            }
        }
        t.end = self.offset;
        Ok(t)
    }
    /// Where content other than whitespace and comment lines follows the root's closing token, if it
    /// does. Such a tail is not read (files written by other tools sometimes carry one).
    pub fn trailing_content(&mut self) -> Option<(u32, u32)> {
        if let Some(t) = self.peeked.take() {
            if t.kind != Kind::Eof { return Some(t.location()); }
        }
        if let Some(t) = self.replay.front() { return Some(t.location()); }
        match self.scan() {
            Ok(t) if t.kind == Kind::Eof => None,
            Ok(t) => Some(t.location()),
            Err(e) => Some((e.line, e.column)),
        }
    }
    /// Consume the rest of an already opened list, without allocating or converting values.
    pub fn skip_list(&mut self) -> Result<(), Refusal> {
        let mut depth = 1usize;
        while depth != 0 {
            let t = self.next_token()?;
            match t.kind {
                Kind::Open => depth += 1,
                Kind::Close => depth -= 1,
                Kind::Eof => return Err(Refusal::new(t.location(), "end of input inside a list")),
                _ => {}
            }
        }
        Ok(())
    }
    /// Decode a retained name. Unknown escapes keep their backslash.
    pub fn text(&self, token: Token) -> Result<String, Refusal> {
        let raw = self.raw(token);
        if token.kind != Kind::String || !raw.contains(&b'\\') {
            return Ok(self.decoded(raw, token));
        }
        let mut out = Vec::with_capacity(raw.len());
        let mut i = 0;
        while i < raw.len() {
            let c = raw[i];
            i += 1;
            if c != b'\\' { out.push(c); continue; }
            let Some(&c) = raw.get(i) else { out.push(b'\\'); break; };
            i += 1;
            match c {
                b'"' | b'\\' => out.push(c),
                b'a' => out.push(7), b'b' => out.push(8), b'f' => out.push(12), b'n' => out.push(10),
                b'r' => out.push(13), b't' => out.push(9), b'v' => out.push(11),
                b'x' => {
                    let mut value = 0u8;
                    let mut n = 0;
                    while n < 2 {
                        let Some(d) = raw.get(i).copied().and_then(hex) else { break; };
                        value = value.wrapping_mul(16).wrapping_add(d); i += 1; n += 1;
                    }
                    out.push(if n == 0 { b'x' } else { value });
                }
                b'0'..=b'7' => {
                    let mut value = c - b'0';
                    for _ in 0..2 {
                        let Some(&d @ b'0'..=b'7') = raw.get(i) else { break; };
                        value = value.wrapping_mul(8).wrapping_add(d - b'0'); i += 1;
                    }
                    out.push(value);
                }
                _ => { out.push(b'\\'); out.push(c); }
            }
        }
        Ok(self.decoded(&out, token))
    }
    fn decoded(&self, bytes: &[u8], token: Token) -> String {
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                let (n, first) = self.lossy.get();
                self.lossy.set((n.saturating_add(1), if n == 0 { token.location() } else { first }));
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
    /// Kept texts decoded lossily: their count and the first one's location.
    pub fn lossy_texts(&self) -> (u32, (u32, u32)) { self.lossy.get() }
    pub fn real(&self, t: Token, what: &str) -> Result<f64, Refusal> {
        if t.kind != Kind::Number { return Err(Refusal::new(t.location(), format!("need a number for '{what}'"))); }
        std::str::from_utf8(self.raw(t)).ok().and_then(|s| s.parse().ok())
            .ok_or_else(|| Refusal::new(t.location(), "Invalid floating point number"))
    }
    /// Integers use only the initial decimal digits, even in exponent notation.
    pub fn integer(&self, t: Token, what: &str) -> Result<i64, Refusal> {
        if t.kind != Kind::Number { return Err(Refusal::new(t.location(), format!("need a number for '{what}'"))); }
        let raw = self.raw(t);
        let negative = raw.first() == Some(&b'-');
        let mut i = usize::from(matches!(raw.first(), Some(b'+' | b'-')));
        let mut value = 0i64;
        while let Some(&d @ b'0'..=b'9') = raw.get(i) {
            value = value.saturating_mul(10).saturating_sub((d - b'0') as i64); i += 1;
        }
        Ok(if negative { value } else { value.saturating_neg() })
    }
}

fn whitespace(c: u8) -> bool { matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0) }
fn hex(c: u8) -> Option<u8> {
    match c { b'0'..=b'9' => Some(c - b'0'), b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10), _ => None }
}
/// Full-atom decimal syntax, independent of conversion or locale.
pub fn is_number(bytes: &[u8]) -> bool {
    let mut i = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while bytes.get(i).is_some_and(u8::is_ascii_digit) { i += 1; digits += 1; }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) { i += 1; digits += 1; }
    }
    if digits == 0 { return false; }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) { i += 1; }
        let start = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) { i += 1; }
        if i == start { return false; }
    }
    i == bytes.len()
}
/// Board millimetres to nanometres, clamped before rounding, ties away from zero.
pub fn mm_to_nm(value: f64) -> i32 {
    (value * 1_000_000.0).clamp(-(NM_LIMIT as f64), NM_LIMIT as f64).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_positions_escapes_and_conversion() {
        let mut lex = Lexer::new(b"\xef\xbb\xbf\t# header\n(foo 1.5e2 \"a\\n\\x41\\101\\q\")\n# tail\nA|B ab\"c #tag");
        assert_eq!(lex.next_token().expect("opening token").location(), (2, 1), "BOM and whole-line comment skipped");
        let name = lex.next_token().expect("keyword");
        assert_eq!(lex.raw(name), b"foo", "keyword source range");
        let number = lex.next_token().expect("number");
        assert_eq!(number.location(), (2, 6), "number byte column");
        assert_eq!(lex.real(number, "real").expect("real conversion"), 150.0, "exponent conversion");
        assert_eq!(lex.integer(number, "integer").expect("integer conversion"), 1, "integer ignores decimal and exponent");
        let string = lex.next_token().expect("string");
        assert_eq!(string.location(), (2, 12), "string location names opening quote");
        assert_eq!(lex.text(string).expect("decoded text"), "a\nAA\\q", "hex, octal, newline and unknown escapes");
        assert_eq!(lex.next_token().expect("closing token").kind, Kind::Close, "quoted text did not consume close");
        let bar = lex.next_token().expect("old bar atom");
        assert_eq!(lex.raw(bar), b"A|B", "old mode keeps bar in names");
        let quote = lex.next_token().expect("embedded quote");
        assert_eq!(lex.raw(quote), b"ab\"c", "mid-atom quote is ordinary");
        let hash = lex.next_token().expect("inline hash");
        assert_eq!(lex.raw(hash), b"#tag", "inline hash is not a comment");
        let mut lex = Lexer::new(b"A|B\0\x0c\x0b");
        lex.set_bar_aware(true);
        assert_eq!(lex.next_token().expect("first bar atom").end, 1, "bar splits a new-mode name");
        assert_eq!(lex.next_token().expect("bar").kind, Kind::Bar, "bar token in new mode");
        lex.next_token().expect("second bar atom");
        let control = lex.next_token().expect("control bytes");
        assert_eq!(lex.raw(control), b"\x0c\x0b", "only the specified whitespace separates atoms");
        for value in ["1", "-0.5", ".5", "1.", "+3", "2.5e-3"] {
            assert!(is_number(value.as_bytes()), "decimal number {value}");
        }
        for value in ["1e", "-", ".", "0x1F", "1.2.3", "inf", "nan", "1mm"] {
            assert!(!is_number(value.as_bytes()), "symbol resembling a number {value}");
        }
        assert_eq!(mm_to_nm(0.0000005), 1, "positive half nm rounds away");
        assert_eq!(mm_to_nm(-0.0000005), -1, "negative half nm rounds away");
        assert_eq!(mm_to_nm(1e300), NM_LIMIT, "positive board clamp");
        assert_eq!(mm_to_nm(-1e300), -NM_LIMIT, "negative board clamp");
        let mut invalid = Lexer::new(b"\xff \"\\xff\"");
        let raw = invalid.next_token().expect("a byte outside UTF-8 is an atom byte");
        assert_eq!(invalid.text(raw).expect("lossy decode"), "\u{fffd}", "invalid source bytes decode lossily");
        let escaped = invalid.next_token().expect("escape source");
        assert_eq!(invalid.text(escaped).expect("lossy decode"), "\u{fffd}", "escapes that make invalid UTF-8 decode lossily");
        assert_eq!(invalid.lossy_texts(), (2, (1, 1)), "lossy decodes are counted from the first location");
        let mut tail = Lexer::new(b"(a)\n# trailing comment\n");
        for _ in 0..3 { tail.next_token().expect("list tokens"); }
        assert_eq!(tail.trailing_content(), None, "comment lines may follow the board");
        let mut tail = Lexer::new(b"(a) b");
        for _ in 0..3 { tail.next_token().expect("list tokens"); }
        assert_eq!(tail.trailing_content(), Some((1, 5)), "an atom after the board is found at its location");
        let mut tail = Lexer::new(b"(a)\n))");
        for _ in 0..3 { tail.next_token().expect("list tokens"); }
        assert_eq!(tail.trailing_content(), Some((2, 1)), "an unbalanced tail is found, not refused");
    }

    #[test]
    fn skips_deep_data_and_locates_lexical_failures() {
        let mut source = "(".repeat(2048);
        source.push_str("junk 1e999999 \"(escaped\\\")\"");
        source.push_str(&")".repeat(2048));
        let mut lex = Lexer::new(source.as_bytes());
        lex.next_token().expect("outer open");
        lex.skip_list().expect("deep skipped content needs no number conversion or recursion");
        assert_eq!(lex.next_token().expect("end").kind, Kind::Eof, "skip stops after matching parenthesis");
        let mut lex = Lexer::new(b"\n  \"open\n");
        let error = lex.next_token().expect_err("raw newline must refuse a string");
        assert_eq!((error.line, error.column), (2, 3), "unterminated string points to opening quote");
        let mut lex = Lexer::new(b"(x\n");
        lex.next_token().expect("open");
        let error = lex.skip_list().expect_err("missing close must refuse");
        assert_eq!((error.line, error.column), (2, 1), "missing close points to EOF");
    }
}
