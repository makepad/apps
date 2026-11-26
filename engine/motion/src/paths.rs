//! Named paths into a document's code: what an editor (a person's panel, an
//! AI's) binds to, resolved by name each time, so a bind survives edits.
//!
//! A path follows the code's own names:
//! - `LET`, `LET.field.sub`, `LIST[3].field`: a top-level `let` of the
//!   document, then fields of its record and items of its lists;
//! - `module.LET.field`: the same in a module the document imports by that
//!   name;
//! - `fn_name.local`: a `let` inside a function the file defines;
//! - `name`, `fn_name.name`, `module.name`: a value or expression named with
//!   a doc tag in front of it (`/**wobble*/ sin(t * 3.0) * 0.2`), which is
//!   source-only;
//! - `Motion.post`, `Motion.layers[2].post[0].strength`: the document's own
//!   `Motion{…}` record (the scene), when no `let Motion` is written.
//!
//! [`resolve`] maps a path to the bytes of the value written there and its
//! kind, or says why it cannot, naming the nearest paths that exist.
//! [`list`] gives every path of a document worth binding, with its kind,
//! current value and label (the `///` line of its `let`). Nothing is
//! evaluated: the code is read.

use makepad_edit_values::ValueKind;
use crate::schema::code::{CodeOutline, RegionKind};

pub mod docs;
use std::ops::Range;

/// A file a path can reach: its name as the document imports it (`""` for
/// the document itself), its text and its outline.
pub struct File<'a> {
    pub name: &'a str,
    pub text: &'a str,
    pub code: &'a CodeOutline,
}

/// Where a path's value is written.
#[derive(Clone, Debug, PartialEq)]
pub struct Site {
    /// Index into the files given.
    pub file: usize,
    pub range: Range<usize>,
    pub kind: ValueKind,
    pub text: String,
}

/// A bindable path of a document.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: String,
    pub kind: ValueKind,
    pub value: String,
    pub label: Option<String>,
}

// ------------------------------------------------------------------ reading values

/// A value as written, with its bytes: records and lists are read into; the
/// rest is kept whole (up to where the next field, item or statement starts).
#[derive(Clone, Debug)]
pub(crate) enum V {
    Obj(Vec<(String, V)>, Range<usize>),
    /// A typed record (`Synth{cutoff: 820}`, `Loop{amp: 0.5}`): its fields
    /// are paths (`mono.cutoff`); a value type (`Font{…}`) stays one value.
    Typed(Vec<(String, V)>, Range<usize>),
    Arr(Vec<V>, Range<usize>),
    Atom(Range<usize>),
}

impl V {
    pub(crate) fn range(&self) -> Range<usize> {
        match self {
            V::Obj(_, r) | V::Typed(_, r) | V::Arr(_, r) | V::Atom(r) => r.clone(),
        }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

const OPS: &[u8] = b"+-*/%<>=!&|?";

impl Reader<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                b'/' if self.s.get(self.i + 1) == Some(&b'/') => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'/' if self.s.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    while self.i + 1 < self.s.len() && !(self.s[self.i] == b'*' && self.s[self.i + 1] == b'/') {
                        self.i += 1;
                    }
                    self.i = (self.i + 2).min(self.s.len());
                }
                _ => break,
            }
        }
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
            self.i += 1;
        }
        (self.i > start).then(|| String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn string(&mut self) {
        // On the opening quote; leaves `i` past the closing one. A raw
        // string (`r"…"`, `r#"…"#`) ends at its own closer.
        let hashes = {
            let mut k = self.i;
            let mut n = 0;
            while k > 0 && self.s[k - 1] == b'#' {
                n += 1;
                k -= 1;
            }
            (k > 0 && self.s[k - 1] == b'r').then_some(n)
        };
        self.i += 1;
        while self.i < self.s.len() {
            match (self.s[self.i], hashes) {
                (b'\\', None) => self.i += 1,
                (b'"', Some(n)) if self.s[self.i + 1..].iter().take(n).filter(|c| **c == b'#').count() == n => {
                    self.i += 1 + n;
                    return;
                }
                (b'"', None) => {
                    self.i += 1;
                    return;
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// One token as written: a bracketed group, a string, or a run.
    fn token(&mut self) {
        self.ws();
        let Some(&c) = self.s.get(self.i) else { return };
        match c {
            b'(' | b'[' | b'{' => {
                let mut depth = 0i32;
                while self.i < self.s.len() {
                    match self.s[self.i] {
                        b'(' | b'[' | b'{' => depth += 1,
                        b')' | b']' | b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                self.i += 1;
                                return;
                            }
                        }
                        b'"' => {
                            self.string();
                            continue;
                        }
                        _ => {}
                    }
                    self.i += 1;
                }
            }
            b'"' => self.string(),
            // A raw string: `r"…"`, `r#"…"#`.
            b'r' if matches!(self.s.get(self.i + 1), Some(b'"') | Some(b'#')) && {
                let mut k = self.i + 1;
                while self.s.get(k) == Some(&b'#') {
                    k += 1;
                }
                self.s.get(k) == Some(&b'"')
            } => {
                while self.s.get(self.i) != Some(&b'"') {
                    self.i += 1;
                }
                self.string();
            }
            _ => {
                self.i += 1;
                while self.i < self.s.len() && !matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r' | b',' | b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'"') {
                    self.i += 1;
                }
            }
        }
    }

    /// A `{…}` record's fields, from its opening brace.
    fn fields(&mut self) -> Vec<(String, V)> {
        self.i += 1;
        let mut fields = Vec::new();
        loop {
            self.ws();
            match self.s.get(self.i) {
                None => break,
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                Some(b',') => {
                    self.i += 1;
                    continue;
                }
                _ => {}
            }
            let at = self.i;
            let Some(key) = self.ident() else {
                self.token();
                continue;
            };
            self.ws();
            if self.s.get(self.i) == Some(&b':') && self.s.get(self.i + 1) != Some(&b':') {
                self.i += 1;
                let value = self.value();
                fields.push((key, value));
            } else {
                // Not a field (a child, a spread): skip it.
                self.i = at;
                self.token();
            }
        }
        fields
    }

    fn value(&mut self) -> V {
        self.ws();
        let start = self.i;
        let atom = match self.s.get(self.i) {
            Some(b'{') => {
                let fields = self.fields();
                V::Obj(fields, start..self.i)
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.ws();
                    match self.s.get(self.i) {
                        None => break,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        Some(b',') => {
                            self.i += 1;
                            continue;
                        }
                        _ => items.push(self.value()),
                    }
                }
                V::Arr(items, start..self.i)
            }
            // A raw string (`r"…"`, `r#"…"#`), not a name `r`.
            Some(b'r') if matches!(self.s.get(self.i + 1), Some(b'"') | Some(b'#')) => {
                self.token();
                V::Atom(start..self.i)
            }
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' => {
                // A name, a path, a call, or a typed record (`Font{…}`).
                self.ident();
                while self.s.get(self.i) == Some(&b'.') && self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
                    self.i += 1;
                    self.ident();
                }
                // (`post.Glow{..}`, `mod.post.Glow{..}`: a type named through its vocabulary.)
                let path = &self.s[start..self.i];
                let last = path.iter().rposition(|c| *c == b'.').map_or(0, |dot| dot + 1);
                let qualified = crate::patch::unqualified(std::str::from_utf8(path).unwrap_or("")).len() < path.len() && path[last].is_ascii_uppercase();
                let typed = self.s.get(self.i) == Some(&b'{') && (self.s[start].is_ascii_uppercase() || qualified) && !path.starts_with(b"Font");
                if typed {
                    let fields = self.fields();
                    V::Typed(fields, start..self.i)
                } else {
                    if matches!(self.s.get(self.i), Some(b'(') | Some(b'{')) {
                        self.token();
                    }
                    V::Atom(start..self.i)
                }
            }
            _ => {
                self.token();
                V::Atom(start..self.i)
            }
        };
        // An expression goes on with an operator (`a * 2`): kept whole.
        let mut extended = false;
        loop {
            let save = self.i;
            self.ws();
            match self.s.get(self.i) {
                Some(c) if OPS.contains(c) && !(*c == b'/' && matches!(self.s.get(self.i + 1), Some(b'/') | Some(b'*'))) => {
                    while self.s.get(self.i).is_some_and(|c| OPS.contains(c)) {
                        self.i += 1;
                    }
                    self.token();
                    extended = true;
                }
                _ => {
                    self.i = save;
                    break;
                }
            }
        }
        if extended { V::Atom(start..self.i) } else { atom }
    }
}

pub(crate) fn read_at(text: &str, at: usize) -> V {
    Reader { s: text.as_bytes(), i: at }.value()
}

/// What a value written as `text` is (`fns`: the functions a name could
/// be).
pub fn kind_of(text: &str, is_fn: impl Fn(&str) -> bool) -> ValueKind {
    let t = text.trim();
    let number_unit = || {
        let digits = t.trim_start_matches('-');
        let end = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
        (end > 0 && digits[..end].parse::<f64>().is_ok()).then(|| &digits[end..])
    };
    match t {
        "true" | "false" => return ValueKind::Bool,
        _ => {}
    }
    if t.starts_with('#') || t.starts_with("color(") {
        return ValueKind::Colour;
    }
    if t.starts_with("Font{") {
        return ValueKind::Font;
    }
    if t.starts_with('"') || t.starts_with("r\"") || t.starts_with("r#") {
        return ValueKind::Text;
    }
    if let Some(name) = t.strip_prefix('@') {
        return if name.starts_with("ease_") || name == "linear" { ValueKind::Ease } else { ValueKind::Choice };
    }
    if t.starts_with('{') {
        return ValueKind::Record;
    }
    if t.starts_with('[') {
        return ValueKind::List;
    }
    match number_unit() {
        Some("") => return ValueKind::Number,
        Some("s") | Some("ms") => return ValueKind::Time,
        Some("deg") | Some("rad") => return ValueKind::Angle,
        Some("px") => return ValueKind::Length,
        _ => {}
    }
    if t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') && is_fn(t) {
        return ValueKind::Function;
    }
    ValueKind::Code
}

// ------------------------------------------------------------------ resolving

/// The top-level `let` of `name` that holds: the last written (a later
/// `let room = Scene3D{…}` shadows `let room = import(…)`).
fn top_let<'a>(code: &'a CodeOutline, name: &str) -> Option<&'a crate::schema::code::Region> {
    code.regions.iter().filter(|r| r.kind == RegionKind::Let && r.depth == 0 && r.name == name).last()
}

/// Where the value of the `let` region starts.
fn let_value(text: &str, region: &crate::schema::code::Region) -> Option<usize> {
    let eq = text[region.range.clone()].find('=')?;
    Some(region.range.start + eq + 1)
}

/// `name[1][2]` → (`name`, [1, 2]).
fn segment(seg: &str) -> Result<(&str, Vec<usize>), String> {
    let (name, rest) = seg.split_once('[').map_or((seg, ""), |(n, r)| (n, r));
    let mut indices = Vec::new();
    if !rest.is_empty() {
        for part in format!("[{rest}").split('[').skip(1) {
            let n = part.strip_suffix(']').and_then(|n| n.trim().parse::<usize>().ok()).ok_or_else(|| format!("`{seg}`: an index is `[n]`"))?;
            indices.push(n);
        }
    }
    Ok((name, indices))
}

/// The tags (`/**name*/`) of a file: name, and where the value after it
/// starts.
fn tags(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("/**").map(|i| from + i) {
        let Some(close) = text[at + 3..].find("*/").map(|i| at + 3 + i) else { break };
        from = close + 2;
        let name = text[at + 3..close].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ' ' || c == '-') {
            continue;
        }
        let start = from + (text[from..].len() - text[from..].trim_start().len());
        // A tag before a `let` labels the let (that is the let's path).
        if text[start..].starts_with("let ") {
            continue;
        }
        out.push((name.replace([' ', '-'], "_"), start));
    }
    out
}

/// A call's arguments as written (`film_post({bloom: 0.7}, 2)` → the
/// record and the number), `None` when `value` is no call.
pub(crate) fn call_args(text: &str, value: &V) -> Option<(String, Vec<V>)> {
    let V::Atom(range) = value else { return None };
    let written = &text[range.clone()];
    let open = written.find('(')?;
    let name = written[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') || !written.trim_end().ends_with(')') {
        return None;
    }
    let mut r = Reader { s: text.as_bytes(), i: range.start + open + 1 };
    let mut args = Vec::new();
    loop {
        r.ws();
        match r.s.get(r.i) {
            None => break,
            Some(b')') => break,
            Some(b',') => r.i += 1,
            _ => {
                let before = r.i;
                args.push(r.value());
                if r.i == before {
                    break;
                }
            }
        }
    }
    Some((name.to_string(), args))
}

fn walk(text: &str, mut value: V, fields: &[&str], path: &str) -> Result<V, String> {
    for field in fields {
        let (name, indices) = segment(field)?;
        // Into a call: `[n]` its n-th argument, a field one of its last
        // record argument's (`film_post({bloom: 0.7})` → `….bloom`).
        let value_call = call_args(text, &value);
        if let Some((_, args)) = value_call {
            if name.is_empty() {
                let mut indices = indices.into_iter();
                let first = indices.next().ok_or_else(|| format!("`{path}`: a call's argument is `[n]`"))?;
                value = args.into_iter().nth(first).ok_or_else(|| format!("`{path}`: the call has no argument {first}"))?;
                value = walk(text, value, &indices.map(|i| format!("[{i}]")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>(), path)?;
                continue;
            }
            let record = args.into_iter().rev().find(|a| matches!(a, V::Obj(..)));
            value = match record {
                Some(V::Obj(items, _)) => items.into_iter().find(|(k, _)| k == name).map(|(_, v)| v).ok_or_else(|| format!("`{path}`: the call's options have no `{name}`"))?,
                _ => return Err(format!("`{path}`: the call has no options record for `{name}`")),
            };
            value = walk(text, value, &indices.iter().map(|i| format!("[{i}]")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>(), path)?;
            continue;
        }
        if !name.is_empty() {
            value = match value {
                V::Obj(items, _) | V::Typed(items, _) => items.into_iter().find(|(k, _)| k == name).map(|(_, v)| v).ok_or_else(|| format!("`{path}`: no field `{name}`"))?,
                other => return Err(format!("`{path}`: `{name}` is not a field of `{}`", text[other.range()].trim().chars().take(40).collect::<String>())),
            };
        }
        for index in indices {
            value = match value {
                V::Arr(items, _) => {
                    let len = items.len();
                    items.into_iter().nth(index).ok_or_else(|| format!("`{path}`: index {index} past the list's {len} items"))?
                }
                _ => return Err(format!("`{path}`: `[{index}]` of something that is not a list")),
            };
        }
    }
    Ok(value)
}

fn is_function(files: &[File], file: usize, name: &str) -> bool {
    match name.split_once('.') {
        Some((module, f)) => files.iter().any(|x| x.name == module && x.code.fns.iter().any(|(n, _)| n == f)),
        None => files[file].code.fns.iter().any(|(n, _)| n == name),
    }
}

fn site(files: &[File], file: usize, value: &V) -> Site {
    let text = files[file].text;
    let range = value.range();
    let written = text[range.clone()].trim_end().to_string();
    let range = range.start..range.start + written.len();
    let kind = kind_of(&written, |n| is_function(files, file, n));
    Site { file, range, kind, text: written }
}

/// The value a path names; when there is none, why, with the nearest
/// paths there are.
pub fn resolve(files: &[File], path: &str) -> Result<Site, String> {
    resolve_exact(files, path).map_err(|why| {
        let near = nearest(files, path);
        if near.is_empty() { why } else { format!("{why}; nearest: {}", near.join(", ")) }
    })
}

/// [`resolve`] without the nearest paths (what listing uses).
pub fn resolve_exact(files: &[File], path: &str) -> Result<Site, String> {
    let path = path.trim();
    let segs: Vec<&str> = path.split('.').collect();
    if segs.is_empty() || segs[0].is_empty() {
        return Err("an empty path".into());
    }
    let doc = files.iter().position(|f| f.name.is_empty()).unwrap_or(0);
    // A module's name first, when the document imports one by it; a
    // document's own `let` of the same name when the module has no such
    // path (`let room = Scene3D{…}` beside `room` the imported module).
    match files.iter().position(|f| !f.name.is_empty() && f.name == segs[0]) {
        Some(k) if segs.len() > 1 => resolve_in(files, k, &segs[1..], path).or_else(|why| resolve_in(files, doc, &segs, path).map_err(|_| why)),
        _ => resolve_in(files, doc, &segs, path),
    }
}

fn resolve_in(files: &[File], file: usize, rest: &[&str], path: &str) -> Result<Site, String> {
    let f = &files[file];
    let (head, head_indices) = segment(rest[0])?;
    let found = (|| {
        if let Some(region) = top_let(f.code, head) {
            let at = let_value(f.text, region).ok_or_else(|| format!("`{head}` has no value"))?;
            let value = read_at(f.text, at);
            // A function's own `let` or tag.
            if rest.len() > 1 && f.text[at..].trim_start().starts_with("fn") {
                let body = &f.text[region.range.clone()];
                let local = rest[1];
                if let Some(off) = body.find(&format!("let {local} =")).or_else(|| body.find(&format!("let {local}="))) {
                    let local_at = region.range.start + off + body[off..].find('=').unwrap_or(0) + 1;
                    return walk(f.text, read_at(f.text, local_at), &rest[2..], path).map(|v| site(files, file, &v));
                }
                if let Some((_, tag_at)) = tags(f.text).into_iter().find(|(n, at)| n == local && region.range.contains(at)) {
                    return walk(f.text, read_at(f.text, tag_at), &rest[2..], path).map(|v| site(files, file, &v));
                }
                return Err(format!("`{path}`: no `let {local}` or tag `{local}` in `{head}`"));
            }
            let mut fields: Vec<String> = Vec::new();
            if !head_indices.is_empty() {
                fields.push(head_indices.iter().map(|i| format!("[{i}]")).collect());
            }
            fields.extend(rest[1..].iter().map(|s| s.to_string()));
            let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
            return walk(f.text, value, &fields, path).map(|v| site(files, file, &v));
        }
        if let Some((_, at)) = tags(f.text).into_iter().find(|(n, _)| n == head) {
            return walk(f.text, read_at(f.text, at), &rest[1..], path).map(|v| site(files, file, &v));
        }
        // The scene itself: the document's top-level `Motion{…}` record.
        if head == "Motion" {
            if let Some(at) = scene_record(f.text) {
                let mut fields: Vec<String> = Vec::new();
                if !head_indices.is_empty() {
                    return Err(format!("`{path}`: `Motion` is a record, not a list"));
                }
                fields.extend(rest[1..].iter().map(|s| s.to_string()));
                let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
                return walk(f.text, read_at(f.text, at), &fields, path).map(|v| site(files, file, &v));
            }
        }
        Err(format!("`{path}`: no `let {head}` or tag `{head}`{}", if f.name.is_empty() { String::new() } else { format!(" in `{}`", f.name) }))
    })();
    found
}

/// Where a document's top-level `Motion{…}` record starts: the last line
/// that begins with it (at the margin, outside any other value).
fn scene_record(text: &str) -> Option<usize> {
    let mut found = None;
    let mut line_start = 0;
    for line in text.split_inclusive('\n') {
        let rest = line.strip_prefix("Motion").map(str::trim_start);
        if rest.is_some_and(|r| r.starts_with('{')) {
            found = Some(line_start);
        }
        line_start += line.len();
    }
    found
}

/// A record's fields as written (`Bloom{strength: 0.8 radius: 0.6}` →
/// `[("strength", "0.8"), ("radius", "0.6")]`), `None` when `text` is no
/// record. Nothing is evaluated.
pub fn record_fields(text: &str) -> Option<Vec<(String, String)>> {
    match read_at(text, 0) {
        V::Obj(fields, _) | V::Typed(fields, _) => Some(fields.into_iter().map(|(k, v)| {
            let written = text[v.range()].trim_end().to_string();
            (k, written)
        }).collect()),
        _ => None,
    }
}

/// The paths of the files that read most like `path` (a few).
pub fn nearest(files: &[File], path: &str) -> Vec<String> {
    let want = path.to_lowercase();
    let last = want.rsplit('.').next().unwrap_or(&want).to_string();
    let mut scored: Vec<(usize, String)> = list(files).into_iter().map(|e| {
        let have = e.path.to_lowercase();
        let tail = have.rsplit('.').next().unwrap_or(&have).to_string();
        (distance(&want, &have).min(distance(&last, &tail) + 2), e.path)
    }).collect();
    scored.sort();
    scored.into_iter().take(4).map(|(_, p)| p).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let next = (row[j] + 1).min(row[j - 1] + 1).min(prev + usize::from(a[i - 1] != b[j - 1]));
            prev = row[j];
            row[j] = next;
        }
    }
    row[b.len()]
}

// ------------------------------------------------------------------ listing

/// The first `///` line above byte `at`'s line.
fn label_above(text: &str, at: usize) -> Option<String> {
    let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let mut end = line_start;
    let mut label = None;
    while end > 0 {
        let prev = text[..end - 1].rfind('\n').map_or(0, |n| n + 1);
        match text[prev..end - 1].trim().strip_prefix("///") {
            Some(doc) => label = Some(doc.trim().to_string()),
            None => break,
        }
        end = prev;
    }
    label
}

/// Every path of the files worth binding: each top-level `let` that is a
/// value (functions are not; a `let` naming one is a slot), the fields of
/// its records and the items of its lists of records, and every tagged
/// value; with its kind, current value and label.
pub fn list(files: &[File]) -> Vec<Entry> {
    let mut out = Vec::new();
    for (k, f) in files.iter().enumerate() {
        let prefix = if f.name.is_empty() { String::new() } else { format!("{}.", f.name) };
        for region in f.code.regions.iter().filter(|r| r.kind == RegionKind::Let && r.depth == 0) {
            let Some(at) = let_value(f.text, region) else { continue };
            if f.text[at..].trim_start().starts_with("fn") || f.text[at..].trim_start().starts_with("import(") {
                continue;
            }
            let label = label_above(f.text, region.range.start);
            let value = read_at(f.text, at);
            push(files, k, &value, format!("{prefix}{}", region.name), &label, &mut out);
        }
        for (name, at) in tags(f.text) {
            let value = read_at(f.text, at);
            let s = site(files, k, &value);
            // Inside a function: named by it.
            let owner = f.code.regions.iter().filter(|r| r.kind == RegionKind::Let && r.depth == 0 && r.range.contains(&at)).map(|r| r.name.clone()).next();
            let path = match owner {
                Some(fun) if f.text[let_value(f.text, f.code.regions.iter().find(|r| r.name == fun).unwrap()).unwrap_or(0)..].trim_start().starts_with("fn") => format!("{prefix}{fun}.{name}"),
                _ => format!("{prefix}{name}"),
            };
            out.push(Entry { path, kind: s.kind, value: s.text, label: Some(format!("tagged `{name}`")) });
        }
    }
    out
}

fn push(files: &[File], file: usize, value: &V, path: String, label: &Option<String>, out: &mut Vec<Entry>) {
    match value {
        V::Obj(fields, _) | V::Typed(fields, _) => {
            for (name, v) in fields {
                push(files, file, v, format!("{path}.{name}"), label, out);
            }
        }
        V::Arr(items, range) => {
            // A list of records or lists: each item; of plain values: whole.
            if items.iter().any(|i| !matches!(i, V::Atom(_))) {
                for (n, v) in items.iter().enumerate() {
                    push(files, file, v, format!("{path}[{n}]"), label, out);
                }
            } else {
                let text = files[file].text[range.clone()].to_string();
                out.push(Entry { path, kind: ValueKind::List, value: text, label: label.clone() });
            }
        }
        V::Atom(_) => {
            let mut s = site(files, file, value);
            // A name of another value (`hot: SIGNAL`): that value's kind.
            if s.kind == ValueKind::Code && s.text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                let name = if files[file].name.is_empty() || s.text.contains('.') { s.text.clone() } else { format!("{}.{}", files[file].name, s.text) };
                if let Ok(target) = resolve_exact(files, &name) {
                    s.kind = target.kind;
                    s.text = format!("{} ({})", s.text, target.text);
                }
            }
            out.push(Entry { path, kind: s.kind, value: s.text, label: label.clone() });
        }
    }
}
