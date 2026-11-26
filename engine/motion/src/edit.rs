//! The one way a value is written into a scene's text: a path (what the
//! editor panel binds, what an inspector or a picker names) resolved in the
//! text as it is now, the value written in the literal's own form (a number
//! keeps its unit, a string its quoting, raw or not, a colour its form),
//! spliced over exactly the literal's bytes, and checked before it is kept:
//! the text still lexes with its brackets matched, the path resolves again
//! to a literal of the same kind, and reads back the value written. A write
//! that fails any of these is refused with the reason, and nothing changes.
//!
//! Undo is the splice reversed: the old literal over the new one's bytes
//! gives the text back byte for byte.

use makepad_edit_values::ValueKind;
use crate::paths::{self, File, Site};
use makepad_script::tokenizer::{lex, LexedToken, ScriptToken, ScriptTokenTag};
use std::ops::Range;

/// A value to write.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// A colour's channels (0..1, light over 1 written as `color(…)`).
    Colour([f64; 4]),
    /// Code as written (a name for a function slot, `@choice`, an
    /// expression).
    Code(String),
}

/// A write that was kept: in file `file`, the bytes `range` (in the text
/// before) became `literal`; `text` is the whole file after.
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    pub file: usize,
    pub range: Range<usize>,
    pub literal: String,
    pub before: String,
    pub text: String,
}

impl Written {
    /// The bytes the literal holds now (in `text`).
    pub fn new_range(&self) -> Range<usize> {
        self.range.start..self.range.start + self.literal.len()
    }

    /// The splice that undoes it: (bytes in `text`, what they become).
    pub fn undo(&self) -> (Range<usize>, String) {
        (self.new_range(), self.before.clone())
    }
}

/// Write `value` at `path` of `files` (doc 0's name `""`, modules by the
/// name the document imports them as). Refused, with why, when the path
/// does not resolve, the value does not fit the literal there, or the text
/// would not read back as written.
pub fn write(files: &[File], path: &str, value: &Value) -> Result<Written, String> {
    let (site, path, literal, lexed) = place_at(files, path, value, 0)?;
    check(files, &site, &path, &literal, value, lexed.as_deref())
}

/// Where [`write`] would write `value` and what (the site's file and bytes,
/// the literal), without the read-back check: a held gesture's steps, which
/// are not written into the text (its release is, through [`write`]).
pub fn place(files: &[File], path: &str, value: &Value) -> Result<(usize, Range<usize>, String), String> {
    let (site, _, literal, _) = place_at(files, path, value, 0)?;
    Ok((site.file, site.range, literal))
}

/// The site a value goes to (a name followed to the literal it holds), the
/// path that names it, the literal, and for a value or a text (one literal
/// token) the tokens of the site's file.
fn place_at(files: &[File], path: &str, value: &Value, depth: usize) -> Result<(Site, String, String, Option<Vec<LexedToken>>), String> {
    let mut site = paths::resolve_exact(files, path)?;
    // A value written as a name of another (`hot: SIGNAL`, `fill:
    // look.BONE`): the literal the name holds is written (a name or a slot
    // chooser writes code, a `Value::Code`, here instead).
    if !matches!(value, Value::Code(_)) && depth < 4 {
        let name = site.text.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && !name.chars().next().is_some_and(|c| c.is_ascii_digit()) && !matches!(name, "true" | "false") {
            let module = files[site.file].name;
            let target = if module.is_empty() || name.contains('.') { name.to_string() } else { format!("{module}.{name}") };
            if paths::resolve_exact(files, &target).is_ok() {
                return place_at(files, &target, value, depth + 1);
            }
        }
    }
    // A value or a text is one literal token of the text as it is now (a
    // number with its sign, a colour, a string, raw or not): that token is
    // replaced by one of the same kind, which cannot move a bracket. A
    // `color(…)` keeps its call and has its numbers written.
    let mut lexed = None;
    if !matches!(value, Value::Code(_)) {
        let tokens = lex(files[site.file].text);
        site.range = literal_in(&tokens, &site.range).ok_or_else(|| format!("`{path}` is not one literal (`{}`)", site.text.trim()))?;
        site.text = files[site.file].text[site.range.clone()].to_string();
        lexed = Some(tokens);
    }
    let literal = form(&site, value)?;
    Ok((site, path.to_string(), literal, lexed))
}

/// The bytes of the one literal written in `range` of `text`: a number
/// (with a `-` before it), a colour, a bool, a string or a raw string; or a
/// `color(…)` call of numbers. `None` when `range` holds anything else.
pub fn literal_token(text: &str, range: &Range<usize>) -> Option<Range<usize>> {
    literal_in(&lex(text), range)
}

/// [`literal_token`] of a text lexed as `tokens`.
fn literal_in(tokens: &[LexedToken], range: &Range<usize>) -> Option<Range<usize>> {
    let toks: Vec<_> = tokens.iter().filter(|t| t.span().start >= range.start && t.span().end <= range.end && !t.span().is_empty()).collect();
    let is_literal = |t: &ScriptToken| matches!(t.kind(), ScriptTokenTag::String | ScriptTokenTag::F32 | ScriptTokenTag::F64 | ScriptTokenTag::F16 | ScriptTokenTag::U32 | ScriptTokenTag::I32 | ScriptTokenTag::U40 | ScriptTokenTag::Color) || t.as_identifier().is_some_and(|id| id == makepad_script::LiveId::from_str("true") || id == makepad_script::LiveId::from_str("false"));
    let minus = |t: &ScriptToken| t.is_operator() && t.operator() == makepad_script::LiveId::from_str("-");
    match toks.as_slice() {
        [one] if is_literal(one.token()) => Some(one.span().clone()),
        [sign, one] if minus(sign.token()) && is_literal(one.token()) => Some(sign.span().start..one.span().end),
        // `color(r, g, b[, a])`: the call, its numbers rewritten.
        // `color(r, g, b[, a])` of number literals: the call, its numbers
        // rewritten (one computed from code is not a literal).
        [first, rest @ ..] if first.token().as_identifier() == Some(makepad_script::LiveId::from_str("color"))
            && rest.first().is_some_and(|t| t.token().is_open_round())
            && rest.last().is_some_and(|t| t.token().is_close_round())
            && rest[1..rest.len() - 1].iter().all(|t| is_literal(t.token()) || minus(t.token()) || t.token().operator_or_separator().is_some_and(|op| op == makepad_script::LiveId::from_str(",") || op == makepad_script::LiveId::from_str("-"))) => Some(first.span().start..rest.last()?.span().end),
        _ => None,
    }
}

/// The value as code in the form of the literal at `site`.
pub fn form(site: &Site, value: &Value) -> Result<String, String> {
    let was = site.text.trim();
    Ok(match (site.kind, value) {
        (ValueKind::Number | ValueKind::Time | ValueKind::Angle | ValueKind::Length, Value::Number(n)) => {
            if !n.is_finite() {
                return Err("not a finite number".into());
            }
            let digits = was.trim_start_matches('-');
            let number_end = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
            let unit = &digits[number_end..];
            // A whole number stays whole (an index, a count).
            let n = if !digits[..number_end].contains('.') && unit.is_empty() { n.round() } else { *n };
            format!("{}{unit}", crate::patch::fmt_num(n))
        }
        (ValueKind::Text, Value::Text(t)) => string_like(was, t),
        (ValueKind::Bool, Value::Bool(b)) => b.to_string(),
        (ValueKind::Colour, Value::Colour(c)) => {
            // A `color(…)` stays a call (its numbers with a decimal point,
            // as the palette reads them); light over 1 cannot be a hex, so
            // a hex becomes a call for it.
            if was.starts_with("color(") || (was.starts_with('#') && c[..3].iter().any(|v| *v > 1.0)) {
                let n = |v: f64| {
                    let t = crate::patch::fmt_num(v);
                    if t.contains('.') { t } else { format!("{t}.0") }
                };
                let alpha = c[3] < 1.0 || was.matches(',').count() >= 3;
                return Ok(if alpha { format!("color({}, {}, {}, {})", n(c[0]), n(c[1]), n(c[2]), n(c[3])) } else { format!("color({}, {}, {})", n(c[0]), n(c[1]), n(c[2])) });
            }
            let old = crate::palette::of(was).first().map(|s| s.rgba).ok_or("not a colour literal")?;
            crate::palette::set(was, &[(old, *c)])
        }
        (ValueKind::Function | ValueKind::Choice | ValueKind::Ease | ValueKind::Code | ValueKind::Font, Value::Code(code)) => code.trim().to_string(),
        (_, Value::Code(code)) if !matches!(site.kind, ValueKind::Text) => code.trim().to_string(),
        (kind, v) => return Err(format!("a {} cannot take {v:?}", kind.name())),
    })
}

/// `words` as a string literal in the form of `old` (`"…"`, `r"…"`,
/// `r#"…"#`): raw stays raw when the words fit (with as many `#` as they
/// need), a plain string is escaped.
fn string_like(old: &str, words: &str) -> String {
    if old.starts_with('r') {
        let mut hashes = old[1..].chars().take_while(|c| *c == '#').count();
        while words.contains(&format!("\"{}", "#".repeat(hashes))) {
            hashes += 1;
        }
        let h = "#".repeat(hashes);
        return format!("r{h}\"{words}\"{h}");
    }
    crate::patch::fmt_text(words)
}

/// The literal spliced in, kept only when the text still lexes with its
/// brackets matched and the path reads back what was written; `lexed`: the
/// old text's tokens when one literal token is replaced (a value or a text).
fn check(files: &[File], site: &Site, path: &str, literal: &str, value: &Value, lexed: Option<&[LexedToken]>) -> Result<Written, String> {
    let before_text = files[site.file].text;
    let range = site.range.clone();
    let mut text = String::with_capacity(before_text.len() + literal.len());
    text.push_str(&before_text[..range.start]);
    text.push_str(literal);
    text.push_str(&before_text[range.end..]);
    let tokens = lex(&text);
    balanced_lexed(&text, &tokens)?;
    // A value or a text: the token stream is the old one but for that one
    // literal (the same tokens before and after it, the same text between
    // them): nothing next to it was eaten or joined to it.
    if let Some(old) = lexed {
        same_stream_lexed(before_text, old, &range, &text, &tokens, &(range.start..range.start + literal.len()))?;
    }
    // Read back: the path again, in the new text.
    let code = crate::schema::code::outline_lexed(&text, &tokens);
    let again: Vec<File> = files.iter().enumerate().map(|(k, f)| if k == site.file { File { name: f.name, text: &text, code: &code } } else { File { name: f.name, text: f.text, code: f.code } }).collect();
    let back = paths::resolve_exact(&again, path).map_err(|why| format!("`{path}` does not resolve after the write: {why}"))?;
    if back.file != site.file || back.range.start != range.start || back.text.trim() != literal.trim() {
        return Err(format!("`{path}` reads back `{}` where `{literal}` was written", back.text.trim()));
    }
    // The value it reads back: what was asked (a number to what its
    // literal can hold: a whole number where one was whole).
    let read = read_value(&back.text, value);
    let whole = matches!(value, Value::Number(_)) && !site.text.contains('.') && literal.trim_start_matches('-').chars().all(|c| c.is_ascii_digit());
    let want = match value {
        Value::Number(n) if whole => Value::Number(n.round()),
        v => v.clone(),
    };
    if read.as_ref() != Some(&want) && !close(read.as_ref(), &want) {
        return Err(format!("`{path}` reads back {read:?}, not {value:?}"));
    }
    Ok(Written { file: site.file, range: range.clone(), literal: literal.to_string(), before: before_text[range].to_string(), text })
}

/// Whether two values are the same as written (numbers to the written
/// precision, colours to a byte).
fn close(read: Option<&Value>, want: &Value) -> bool {
    match (read, want) {
        (Some(Value::Number(a)), Value::Number(b)) => (a - b).abs() <= 0.5e-4 + b.abs() * 1e-9,
        (Some(Value::Colour(a)), Value::Colour(b)) => a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1.0 / 255.0 + 1e-9 || (*y > 1.0 && (x - y).abs() < 1e-3)),
        _ => false,
    }
}

/// A literal's value, read the way `want` is a value.
pub fn read_value(literal: &str, want: &Value) -> Option<Value> {
    let t = literal.trim();
    match want {
        Value::Number(_) => {
            let digits_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(t.len());
            t[..digits_end].parse::<f64>().ok().map(Value::Number)
        }
        Value::Text(_) => lex(t).first().and_then(|tok| tok.string().map(str::to_owned)).map(Value::Text),
        Value::Bool(_) => match t {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        Value::Colour(_) => crate::palette::of(t).first().map(|s| Value::Colour(s.rgba)),
        Value::Code(_) => Some(Value::Code(t.to_string())),
    }
}

/// The tokens of `old` outside `old_range` and of `new` outside
/// `new_range` are the same, with the same text between them, and the
/// literal is one token (or a sign and one, or a `color(…)` call) in both.
pub fn same_stream(old: &str, old_range: &Range<usize>, new: &str, new_range: &Range<usize>) -> Result<(), String> {
    same_stream_lexed(old, &lex(old), old_range, new, &lex(new), new_range)
}

/// [`same_stream`] of `old` lexed as `a` and `new` as `b`.
fn same_stream_lexed(old: &str, a: &[LexedToken], old_range: &Range<usize>, new: &str, b: &[LexedToken], new_range: &Range<usize>) -> Result<(), String> {
    let split = |toks: &[makepad_script::tokenizer::LexedToken], r: &Range<usize>| -> (usize, usize) {
        let before = toks.iter().take_while(|t| t.span().end <= r.start).count();
        let after = toks.iter().filter(|t| t.span().start >= r.end).count();
        (before, after)
    };
    let (ab, aa) = split(a, old_range);
    let (bb, ba) = split(b, new_range);
    // (The literal itself may become a call: a hex taking light over 1
    // becomes `color(…)`.)
    if ab != bb || aa != ba || a.len() - ab - aa == 0 || b.len() - bb - ba == 0 {
        return Err("the write would change the code around the value".into());
    }
    let shift = new_range.end as isize - old_range.end as isize;
    for (x, y) in a[..ab].iter().zip(&b[..bb]) {
        if x.token().kind() != y.token().kind() || x.span() != y.span() || old.get(x.span().clone()) != new.get(y.span().clone()) {
            return Err("the write would change the code before the value".into());
        }
    }
    for (x, y) in a[a.len() - aa..].iter().zip(&b[b.len() - ba..]) {
        if x.token().kind() != y.token().kind() || x.span().start as isize + shift != y.span().start as isize || x.preceded_by_newline() != y.preceded_by_newline() || old.get(x.span().clone()) != new.get(y.span().clone()) {
            return Err("the write would change the code after the value".into());
        }
    }
    // The token after the literal starts after the same gap.
    if let (Some(x), Some(y)) = (a.get(a.len() - aa), b.get(b.len() - ba)) {
        if old.get(old_range.end..x.span().start) != new.get(new_range.end..y.span().start) {
            return Err("the write would change what follows the value".into());
        }
    }
    Ok(())
}

/// The text lexes, and its brackets open and close in order.
pub fn balanced(text: &str) -> Result<(), String> {
    balanced_lexed(text, &lex(text))
}

/// [`balanced`] of `text` lexed as `tokens`.
fn balanced_lexed(text: &str, tokens: &[LexedToken]) -> Result<(), String> {
    let mut stack: Vec<char> = Vec::new();
    for tok in tokens {
        let (open, close) = match tok.token().kind() {
            ScriptTokenTag::OpenCurly => (Some('{'), None),
            ScriptTokenTag::OpenRound => (Some('('), None),
            ScriptTokenTag::OpenSquare => (Some('['), None),
            ScriptTokenTag::CloseCurly => (None, Some('{')),
            ScriptTokenTag::CloseRound => (None, Some('(')),
            ScriptTokenTag::CloseSquare => (None, Some('[')),
            ScriptTokenTag::StringUnfinished => return Err("a string is left open".into()),
            _ => (None, None),
        };
        if let Some(o) = open {
            stack.push(o);
        }
        if let Some(c) = close {
            if stack.pop() != Some(c) {
                let line = text[..tok.span().start.min(text.len())].matches('\n').count() + 1;
                return Err(format!("brackets no longer match at line {line}"));
            }
        }
    }
    if !stack.is_empty() {
        return Err("a bracket is left open".into());
    }
    Ok(())
}

/// A held gesture's text: the text as the gesture began (what the running
/// document was loaded from, and what paths resolve in: their outline is
/// that text's) and each literal moved since, by its bytes then and its
/// text now. Nothing is written until the release (`text`): every step
/// lands in the running document, and each binding finds its literal by its
/// path in the base text, `now_of` saying where those bytes are now.
#[derive(Clone, Debug)]
pub struct Held {
    base: String,
    /// (bytes in the base text, text now), in no order.
    spots: Vec<(Range<usize>, String)>,
}

impl Held {
    pub fn new(base: String) -> Self {
        Self { base, spots: Vec::new() }
    }

    /// The text as the gesture began.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The bytes `then` of the base text, in the text now: a moved literal
    /// is where its new text is; anything else moves by what grew before it.
    pub fn now_of(&self, then: Range<usize>) -> Range<usize> {
        if let Some((r, text)) = self.spots.iter().find(|(r, _)| r.start == then.start || (r.start < then.end && then.start < r.end)) {
            let at = self.shift(r.start);
            return at..at + text.len();
        }
        self.shift(then.start)..self.shift(then.end)
    }

    /// The bytes `now` of the text now, in the base text: a moved literal
    /// by where it starts or any overlap (whatever end the writer read),
    /// anything else by what grew before it.
    pub fn then_of(&self, now: Range<usize>) -> Range<usize> {
        for (r, text) in &self.spots {
            let at = self.shift(r.start);
            if at == now.start || (at < now.end && now.start < at + text.len()) {
                return r.clone();
            }
        }
        let grown: isize = self.spots.iter().filter(|(r, text)| self.shift(r.start) + text.len() <= now.start).map(|(r, text)| text.len() as isize - r.len() as isize).sum();
        let start = (now.start as isize - grown).max(0) as usize;
        start..start + now.len()
    }

    /// The literal at `then` (base bytes) is `text` now.
    pub fn set(&mut self, then: Range<usize>, text: &str) {
        match self.spots.iter_mut().find(|(r, _)| r.start == then.start || (r.start < then.end && then.start < r.end)) {
            Some(spot) => spot.1 = text.to_string(),
            None => self.spots.push((then, text.to_string())),
        }
    }

    /// The text now: the base with every moved literal's text.
    pub fn text(&self) -> String {
        let mut spots: Vec<&(Range<usize>, String)> = self.spots.iter().collect();
        spots.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        let mut text = self.base.clone();
        for (r, now) in spots {
            if r.end <= text.len() {
                text.replace_range(r.clone(), now);
            }
        }
        text
    }

    /// Where base byte `at` is now (moved literals before it grown).
    fn shift(&self, at: usize) -> usize {
        let grown: isize = self.spots.iter().filter(|(r, _)| r.end <= at).map(|(r, text)| text.len() as isize - r.len() as isize).sum();
        (at as isize + grown) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_gestures_literals_are_found_by_their_bytes_then_and_now() {
        let base = "let A = 1\nlet B = 22\nlet C = 333\n".to_string();
        let mut held = Held::new(base.clone());
        let b = base.find("22").unwrap()..base.find("22").unwrap() + 2;
        let c = base.find("333").unwrap()..base.find("333").unwrap() + 3;
        held.set(b.clone(), "22.125");
        assert_eq!(held.text(), "let A = 1\nlet B = 22.125\nlet C = 333\n");
        // C moved by B's growth; B is its new text.
        assert_eq!(&held.text()[held.now_of(c.clone())], "333");
        assert_eq!(&held.text()[held.now_of(b.clone())], "22.125");
        // A writer that read B before its last step (the old length) finds
        // the same literal, not a second spot over it.
        assert_eq!(held.then_of(held.now_of(b.clone()).start..held.now_of(b.clone()).start + 2), b);
        held.set(c.clone(), "3");
        held.set(b.clone(), "2");
        assert_eq!(held.text(), "let A = 1\nlet B = 2\nlet C = 3\n");
        assert_eq!(held.then_of(held.now_of(c.clone())), c);
    }

    fn files(text: &str) -> (String, crate::schema::code::CodeOutline) {
        (text.to_string(), crate::schema::code::outline(text))
    }

    #[test]
    fn a_light_over_one_colour_in_a_hex_becomes_a_colour_call() {
        let (t, c) = files("let SIGNAL = /**signal*/ #ff4d12\nlet LOOK = {hot: SIGNAL}\n");
        let f = [File { name: "", text: &t, code: &c }];
        let w = write(&f, "SIGNAL", &Value::Colour([3.0, 1.0, 0.5, 1.0])).unwrap();
        assert_eq!(w.text, "let SIGNAL = /**signal*/ color(3.0, 1.0, 0.5)\nlet LOOK = {hot: SIGNAL}\n");
        // A name's value: the literal it names.
        let w = write(&f, "LOOK.hot", &Value::Colour([0.0, 0.0, 1.0, 1.0])).unwrap();
        assert!(w.text.starts_with("let SIGNAL = /**signal*/ #0000ff"), "{}", w.text);
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn a_unit_number_at_a_lines_end_keeps_the_newline() {
        let t = "let SHEET = 64px\nlet sheet = fn(x, y) { x * SHEET }\nlet D = {a: 0.5s // half\n b: 90deg}\n";
        let c = crate::schema::code::outline(t);
        let f = [File { name: "", text: t, code: &c }];
        let w = write(&f, "SHEET", &Value::Number(49.0)).unwrap();
        assert_eq!(w.text, t.replacen("64px", "49px", 1));
        let w = write(&f, "D.a", &Value::Number(0.25)).unwrap();
        assert_eq!(w.text, t.replacen("0.5s", "0.25s", 1));
        let w = write(&f, "D.b", &Value::Number(45.0)).unwrap();
        assert_eq!(w.text, t.replacen("90deg", "45deg", 1));
    }
}
