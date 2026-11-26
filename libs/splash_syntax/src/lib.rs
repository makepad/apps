//! Read Splash data with byte ranges for source-preserving edits. The
//! patchers keep the comments, order and spacing outside the edited value.
//!
//! A document contains `use` lines, `let name = value` statements and
//! top-level values. Values are numbers, strings,
//! `@tags`, `#colours`, `true`/`false`/`nil`, arrays, objects (typed or
//! not), calls such as `asset("…", "…")`, and bare names that refer to an
//! earlier `let`. Entries are separated by spaces, new lines or commas;
//! `//` and `/* */` comments go anywhere.

mod size;
pub use size::expanded_size;

/// A value and the bytes it was written in.
///
/// Dropping a tree and walking it recursively also use stack. Walk and drop
/// it on a thread with at least as much available stack as the parsing thread.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Num(f64),
    Str(String),
    /// `@name`.
    Tag(String),
    /// `#rrggbb[aa]` as RGBA packed big-endian.
    Color(u32),
    Bool(bool),
    Nil,
    /// The complete original text of a value whose nesting exceeded the reader's limit.
    TooDeep(String),
    /// A bare name: a reference to a `let`.
    Name(String),
    Arr(Vec<Node>),
    /// `Type{…}` or `{…}`: `open` and `close` are the braces.
    Obj { ty: Option<String>, open: usize, close: usize, fields: Vec<Field> },
    Call { name: String, args: Vec<Node> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    /// Where the entry starts (its name) and ends (its value).
    pub start: usize,
    pub end: usize,
    pub value: Node,
}

/// A top-level statement: `let name = value`, or a bare value.
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub name: Option<String>,
    /// From `let` (or the value) to the value's end.
    pub start: usize,
    pub end: usize,
    pub value: Node,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub stmts: Vec<Stmt>,
    /// `(byte offset, message)`.
    pub errors: Vec<(usize, String)>,
}

impl Node {
    pub fn fields(&self) -> &[Field] {
        match &self.kind {
            Kind::Obj { fields, .. } => fields,
            _ => &[],
        }
    }

    pub fn field(&self, name: &str) -> Option<&Node> {
        self.fields().iter().find(|field| field.name == name).map(|field| &field.value)
    }

    pub fn ty(&self) -> Option<&str> {
        match &self.kind {
            Kind::Obj { ty, .. } => ty.as_deref(),
            _ => None,
        }
    }

    pub fn braces(&self) -> Option<(usize, usize)> {
        match &self.kind {
            Kind::Obj { open, close, .. } => Some((*open, *close)),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self.kind {
            Kind::Num(n) if n.is_finite() => Some(n),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match &self.kind {
            Kind::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn tag(&self) -> Option<&str> {
        match &self.kind {
            Kind::Tag(s) => Some(s),
            _ => None,
        }
    }

    /// A reference: a bare name, or (tolerated) a tag or a string.
    pub fn name(&self) -> Option<&str> {
        match &self.kind {
            Kind::Name(s) | Kind::Tag(s) | Kind::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn arr(&self) -> Option<&[Node]> {
        match &self.kind {
            Kind::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// What the value is, for an error message.
    pub fn what(&self) -> String {
        match &self.kind {
            Kind::Num(n) => format!("the number {n}"),
            Kind::Str(s) => format!("the string {s:?}"),
            Kind::Tag(s) => format!("@{s}"),
            Kind::Color(_) => "a colour".into(),
            Kind::Bool(b) => format!("{b}"),
            Kind::Nil => "nil".into(),
            Kind::TooDeep(_) => "a value nested too deeply".into(),
            Kind::Name(s) => format!("the name `{s}`"),
            Kind::Arr(items) => format!("an array of {}", items.len()),
            Kind::Obj { ty: Some(ty), .. } => format!("a {ty}{{…}}"),
            Kind::Obj { ty: None, .. } => "an object".into(),
            Kind::Call { name, .. } => format!("{name}(…)"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Optional additional limit, with the root at depth zero. By default
    /// the available stack alone determines how deeply the reader descends.
    pub max_depth: Option<usize>,
}

struct Parser<'a> {
    src: &'a str,
    bytes: &'a [u8],
    at: usize,
    limits: Limits,
    stack_floor: usize,
    single_value: bool,
    errors: Vec<(usize, String)>,
}

pub fn ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

pub fn ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Read a Splash document, keeping its statements and listing any problems.
pub fn parse(src: &str) -> Parsed {
    parse_with(src, Limits::default())
}

/// Read a document with a caller-selected nesting limit.
pub fn parse_with(src: &str, limits: Limits) -> Parsed {
    let marker = 0u8;
    let origin = std::hint::black_box(&marker) as *const u8 as usize;
    parse_with_budget(src, limits, origin, makepad_stack_budget::native_stack_budget(origin))
}

fn parse_with_budget(src: &str, limits: Limits, origin: usize, budget: usize) -> Parsed {
    let mut parser = Parser::new(src, limits, false, origin, budget);
    let mut stmts = Vec::new();
    loop {
        parser.skip_space();
        if parser.at >= parser.bytes.len() {
            break;
        }
        let start = parser.at;
        let word = parser.peek_word();
        if word == "use" {
            // Skip the import up to the end of the line.
            parser.at = src[start..].find('\n').map_or(src.len(), |n| start + n);
            continue;
        }
        let name = if word == "let" {
            parser.at += 3;
            parser.skip_space();
            let name = parser.peek_word().to_string();
            if name.is_empty() {
                parser.error("`let` needs a name");
                parser.recover();
                continue;
            }
            parser.at += name.len();
            parser.skip_space();
            if parser.bytes.get(parser.at) != Some(&b'=') {
                parser.error(&format!("`let {name}` needs `=` and a value"));
                parser.recover();
                continue;
            }
            parser.at += 1;
            Some(name)
        } else {
            None
        };
        parser.skip_space();
        match parser.value(0) {
            Some(value) => {
                let end = value.end;
                stmts.push(Stmt { name, start, end, value });
            }
            None => parser.recover(),
        }
        parser.skip_space();
        if parser.bytes.get(parser.at) == Some(&b';') {
            parser.at += 1;
        }
    }
    if stmts.iter().any(|stmt| stmt.name.is_some()) {
        size::expansion(&stmts, |at, name| {
            parser.errors.push((at, format!("cycle in let reference to `{name}`")));
        });
    }
    Parsed { stmts, errors: parser.errors }
}

/// Read exactly one value, allowing only comments and whitespace around it.
pub fn read_value(src: &str) -> Result<Node, Vec<(usize, String)>> {
    let marker = 0u8;
    let origin = std::hint::black_box(&marker) as *const u8 as usize;
    let mut parser = Parser::new(src, Limits::default(), true, origin, makepad_stack_budget::native_stack_budget(origin));
    parser.skip(false);
    let value = parser.value(0);
    parser.skip(false);
    if parser.at != src.len() {
        parser.error("unexpected text after the value");
    }
    match value {
        Some(node) if parser.errors.is_empty() => Ok(node),
        _ => Err(parser.errors),
    }
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, limits: Limits, single_value: bool, origin: usize, budget: usize) -> Self {
        let stack_floor = origin.saturating_sub(budget);
        Self { src, bytes: src.as_bytes(), at: 0, limits, stack_floor, single_value, errors: Vec::new() }
    }

    fn error(&mut self, message: &str) {
        self.errors.push((self.at, message.to_string()));
    }

    /// Skip to the next line after an error.
    fn recover(&mut self) {
        let from = self.at.min(self.src.len());
        self.at = self.src[from..].find('\n').map_or(self.src.len(), |n| from + n + 1);
    }

    fn skip_space(&mut self) {
        self.skip(true);
    }

    fn skip(&mut self, commas: bool) {
        loop {
            while self.at < self.bytes.len() && (self.bytes[self.at].is_ascii_whitespace() || (commas && self.bytes[self.at] == b',')) {
                self.at += 1;
            }
            if self.bytes[self.at..].starts_with(b"//") {
                while self.at < self.bytes.len() && self.bytes[self.at] != b'\n' {
                    self.at += 1;
                }
                continue;
            }
            if self.bytes[self.at..].starts_with(b"/*") {
                if let Some(n) = self.src[self.at + 2..].find("*/") {
                    self.at += 2 + n + 2;
                } else {
                    if self.single_value {
                        self.error("a block comment is not closed with `*/`");
                    }
                    self.at = self.src.len();
                }
                continue;
            }
            break;
        }
    }

    fn peek_word(&self) -> &str {
        let mut end = self.at;
        if end < self.bytes.len() && ident_start(self.bytes[end]) {
            while end < self.bytes.len() && ident_byte(self.bytes[end]) {
                end += 1;
            }
        }
        &self.src[self.at..end]
    }

    fn value(&mut self, depth: usize) -> Option<Node> {
        let start = self.at;
        let Some(&byte) = self.bytes.get(start) else {
            self.error("a value is missing at the end");
            return None;
        };
        let here = std::hint::black_box(&start) as *const usize as usize;
        let too_deep = here <= self.stack_floor || self.limits.max_depth.is_some_and(|max| depth > max);
        if too_deep {
            self.error("values nest too deeply");
        }
        let kind = match byte {
            b'[' | b'{' if too_deep => return self.skip_group(start),
            b'"' => Kind::Str(self.string()?),
            b'@' => {
                self.at += 1;
                let word = self.peek_word().to_string();
                if word.is_empty() {
                    self.error("`@` needs a name after it");
                    return None;
                }
                self.at += word.len();
                Kind::Tag(word)
            }
            b'#' => {
                self.at += 1;
                let from = self.at;
                while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_hexdigit() {
                    self.at += 1;
                }
                match parse_color(&self.src[from..self.at]) {
                    Some(color) => Kind::Color(color),
                    None => {
                        self.error("a colour is `#rgb`, `#rrggbb` or `#rrggbbaa`");
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.bytes.get(self.at) {
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        None => {
                            self.errors.push((start, "an array is not closed with `]`".into()));
                            return None;
                        }
                        _ => items.push(self.value(depth + 1)?),
                    }
                }
                Kind::Arr(items)
            }
            b'{' => self.object(None, depth)?,
            b'-' | b'0'..=b'9' | b'.' => Kind::Num(self.number()?),
            byte if ident_start(byte) => {
                let word = self.peek_word().to_string();
                self.at += word.len();
                let after = self.at;
                // A comma separates values, even when an object follows a bare name.
                let errors_before_lookahead = self.errors.len();
                self.skip(false);
                self.errors.truncate(errors_before_lookahead);
                match (word.as_str(), self.bytes.get(self.at)) {
                    ("true", _) => {
                        self.at = after;
                        Kind::Bool(true)
                    }
                    ("false", _) => {
                        self.at = after;
                        Kind::Bool(false)
                    }
                    ("nil", _) => {
                        self.at = after;
                        Kind::Nil
                    }
                    (_, Some(b'{' | b'(')) if too_deep => return self.skip_group(start),
                    (_, Some(b'{')) => self.object(Some(word), depth)?,
                    (_, Some(b'(')) => {
                        self.at += 1;
                        let mut args = Vec::new();
                        loop {
                            self.skip_space();
                            match self.bytes.get(self.at) {
                                Some(b')') => {
                                    self.at += 1;
                                    break;
                                }
                                None => {
                                    self.errors.push((start, format!("`{word}(` is not closed with `)`")));
                                    return None;
                                }
                                _ => args.push(self.value(depth + 1)?),
                            }
                        }
                        Kind::Call { name: word, args }
                    }
                    _ => {
                        // A dotted path (`mod.x`) reads as one name.
                        self.at = after;
                        let mut name = word;
                        while self.bytes.get(self.at) == Some(&b'.') && self.bytes.get(self.at + 1).is_some_and(|b| ident_start(*b)) {
                            self.at += 1;
                            let part = self.peek_word().to_string();
                            self.at += part.len();
                            name.push('.');
                            name.push_str(&part);
                        }
                        Kind::Name(name)
                    }
                }
            }
            other => {
                self.error(&format!("unexpected `{}`", other as char));
                return None;
            }
        };
        if too_deep {
            Some(self.placeholder(start))
        } else {
            Some(Node { start, end: self.at, kind })
        }
    }

    fn placeholder(&self, start: usize) -> Node {
        Node { start, end: self.at, kind: Kind::TooDeep(self.src[start..self.at].into()) }
    }

    /// Consume the balanced group at the cursor without building its nodes.
    fn skip_group(&mut self, start: usize) -> Option<Node> {
        let open = self.at;
        let mut closers = Vec::new();
        while let Some(&byte) = self.bytes.get(self.at) {
            match byte {
                b'[' => closers.push(b']'),
                b'{' => closers.push(b'}'),
                b'(' => closers.push(b')'),
                b']' | b'}' | b')' => {
                    if closers.pop() != Some(byte) {
                        self.error("mismatched closing delimiter");
                        return None;
                    }
                    if closers.is_empty() {
                        self.at += 1;
                        return Some(self.placeholder(start));
                    }
                }
                b'"' => {
                    self.string()?;
                    continue;
                }
                b'/' if matches!(self.bytes.get(self.at + 1), Some(b'/' | b'*')) => {
                    self.skip_space();
                    continue;
                }
                _ => {}
            }
            self.at += 1;
        }
        self.errors.push((open, "a nested value is not closed".into()));
        None
    }

    fn object(&mut self, ty: Option<String>, depth: usize) -> Option<Kind> {
        let open = self.at;
        self.at += 1;
        let mut fields = Vec::new();
        loop {
            self.skip_space();
            match self.bytes.get(self.at) {
                Some(b'}') => break,
                None => {
                    self.errors.push((open, format!("{} is not closed with `}}`", ty.as_deref().map_or("an object".to_string(), |ty| format!("`{ty}{{`")))));
                    return None;
                }
                _ => {}
            }
            let start = self.at;
            let name = if self.bytes[self.at] == b'"' { self.string()? } else { self.peek_word().to_string() };
            if name.is_empty() {
                self.error("a field needs a name");
                return None;
            }
            if self.bytes[start] != b'"' {
                self.at += name.len();
            }
            self.skip_space();
            if self.bytes.get(self.at) != Some(&b':') {
                self.error(&format!("`{name}` needs `:` and a value"));
                return None;
            }
            self.at += 1;
            self.skip_space();
            let value = self.value(depth + 1)?;
            fields.push(Field { name, start, end: value.end, value });
        }
        let close = self.at;
        self.at += 1;
        Some(Kind::Obj { ty, open, close, fields })
    }

    fn string(&mut self) -> Option<String> {
        let start = self.at;
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.src[self.at..].chars().next() else {
                self.errors.push((start, "a string is not closed with `\"`".into()));
                return None;
            };
            self.at += ch.len_utf8();
            match ch {
                '"' => return Some(out),
                '\\' => {
                    let Some(next) = self.src[self.at..].chars().next() else { continue };
                    self.at += next.len_utf8();
                    out.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        other => other,
                    });
                }
                ch => out.push(ch),
            }
        }
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        if self.bytes[self.at] == b'-' {
            self.at += 1;
        }
        while self.at < self.bytes.len() && (self.bytes[self.at].is_ascii_digit() || self.bytes[self.at] == b'.' || self.bytes[self.at] == b'_') {
            self.at += 1;
        }
        if self.at < self.bytes.len() && (self.bytes[self.at] == b'e' || self.bytes[self.at] == b'E') {
            self.at += 1;
            if self.at < self.bytes.len() && (self.bytes[self.at] == b'-' || self.bytes[self.at] == b'+') {
                self.at += 1;
            }
            while self.at < self.bytes.len() && self.bytes[self.at].is_ascii_digit() {
                self.at += 1;
            }
        }
        let text: String = self.src[start..self.at].chars().filter(|c| *c != '_').collect();
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Some(n),
            _ => {
                self.errors.push((start, format!("`{}` is not a number", &self.src[start..self.at])));
                None
            }
        }
    }
}

pub fn parse_color(hex: &str) -> Option<u32> {
    let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let pair = |i: usize| digits[i] * 16 + digits[i + 1];
    let (r, g, b, a) = match digits.len() {
        3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, 255),
        4 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, digits[3] * 17),
        6 => (pair(0), pair(2), pair(4), 255),
        8 => (pair(0), pair(2), pair(4), pair(6)),
        _ => return None,
    };
    Some((r << 24) | (g << 16) | (b << 8) | a)
}

/// 1-based `(line, column)` of a byte offset.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(offset, |n| offset - n - 1) + 1;
    (line, col)
}

// ------------------------------------------------------------- literals

/// Write a parsed value as canonical Splash. A depth placeholder keeps its
/// original spelling, including any comments and spacing inside the subtree.
pub fn write_value(node: &Node) -> String {
    fn list(out: &mut String, open: char, items: &[Node], close: char) {
        out.push(open);
        for (i, item) in items.iter().enumerate() {
            if i != 0 { out.push_str(", "); }
            write(out, item);
        }
        out.push(close);
    }

    fn write(out: &mut String, node: &Node) {
        match &node.kind {
            Kind::Num(n) => out.push_str(&n.to_string()),
            Kind::Str(s) => out.push_str(&string_literal(s)),
            Kind::Tag(tag) => { out.push('@'); out.push_str(tag); }
            Kind::Color(rgba) => out.push_str(&color_literal(*rgba)),
            Kind::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Kind::Nil => out.push_str("nil"),
            Kind::TooDeep(text) | Kind::Name(text) => out.push_str(text),
            Kind::Arr(items) => list(out, '[', items, ']'),
            Kind::Call { name, args } => {
                out.push_str(name);
                list(out, '(', args, ')');
            }
            Kind::Obj { ty, fields, .. } => {
                if let Some(ty) = ty { out.push_str(ty); }
                out.push('{');
                for (i, field) in fields.iter().enumerate() {
                    if i != 0 { out.push(' '); }
                    let name = field.name.as_bytes();
                    if name.first().is_some_and(|b| ident_start(*b)) && name.iter().all(|b| ident_byte(*b)) {
                        out.push_str(&field.name);
                    } else {
                        out.push_str(&string_literal(&field.name));
                    }
                    out.push_str(": ");
                    write(out, &field.value);
                }
                out.push('}');
            }
        }
    }

    let mut out = String::new();
    write(&mut out, node);
    out
}

pub fn string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Write a number rounded to thousandths, keeping `.0` on whole numbers.
pub fn number(value: f64) -> String {
    let value = (value * 1000.0).round() / 1000.0;
    let value = if value == 0.0 { 0.0 } else { value };
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

pub fn color_literal(rgba: u32) -> String {
    if rgba & 0xff == 0xff {
        format!("#{:06x}", rgba >> 8)
    } else {
        format!("#{rgba:08x}")
    }
}

/// Words Splash reserves: never a `let` name.
pub const RESERVED: &[&str] = &["let", "fn", "if", "else", "for", "in", "while", "loop", "return", "break", "continue", "use", "mod", "true", "false", "nil", "match", "me", "scope", "self", "var"];

/// A name that can follow `let`.
pub fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty() && ident_start(bytes[0]) && bytes.iter().all(|b| ident_byte(*b)) && !RESERVED.contains(&name) && name.len() <= 64
}

// ------------------------------------------------------------- patching

/// Replace the value of `field` in the object `(open, close)` with `value`,
/// or add `field: value` before the object closes, matching the object's
/// layout (one entry per line, or all on one line). Only those bytes change.
pub fn set_field(src: &str, object: &Node, field: &str, value: &str) -> String {
    let Some((open, close)) = object.braces() else { return src.to_string() };
    let fields = object.fields();
    let mut out = src.to_string();
    if let Some(entry) = fields.iter().find(|entry| entry.name == field) {
        out.replace_range(entry.value.start..entry.value.end, value);
        return out;
    }
    let line_start = src[..close].rfind('\n').map_or(0, |n| n + 1);
    let multiline = src[open..close].contains('\n');
    if multiline && line_start > open && src[line_start..close].bytes().all(|b| b == b' ' || b == b'\t') {
        let indent = fields.last().map(|entry| {
            let at = src[..entry.start].rfind('\n').map_or(0, |n| n + 1);
            src[at..entry.start].to_string()
        }).filter(|indent| indent.bytes().all(|b| b == b' ' || b == b'\t')).unwrap_or_else(|| format!("{}    ", &src[line_start..close]));
        out.insert_str(line_start, &format!("{indent}{field}: {value}\n"));
    } else {
        let at = src[..close].trim_end().len();
        let gap = if src[open + 1..at].trim().is_empty() { "" } else { " " };
        out.replace_range(at..close, &format!("{gap}{field}: {value}"));
    }
    out
}

/// Remove `field` (and the space before it) from an object.
pub fn remove_field(src: &str, object: &Node, field: &str) -> String {
    let Some(entry) = object.fields().iter().find(|entry| entry.name == field) else { return src.to_string() };
    let mut start = entry.start;
    let mut end = entry.end;
    let bytes = src.as_bytes();
    // A trailing comma goes with it.
    let mut after = end;
    while after < bytes.len() && (bytes[after] == b' ' || bytes[after] == b'\t') {
        after += 1;
    }
    if bytes.get(after) == Some(&b',') {
        end = after + 1;
    }
    let line_start = src[..start].rfind('\n').map_or(0, |n| n + 1);
    let line_end = src[end..].find('\n').map_or(src.len(), |n| end + n);
    if src[line_start..start].trim().is_empty() && src[end..line_end].trim().is_empty() {
        // Alone on its line: the whole line.
        start = line_start;
        end = (line_end + 1).min(src.len());
    } else {
        while start > 0 && (bytes[start - 1] == b' ' || bytes[start - 1] == b'\t') {
            start -= 1;
        }
        // The first entry after `{`: the space after it goes instead.
        if start > 0 && bytes[start - 1] == b'{' {
            while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
                end += 1;
            }
        }
    }
    let mut out = src.to_string();
    out.replace_range(start..end, "");
    out
}

/// Remove a whole statement, with the line it is on when nothing else is,
/// and a comment block directly above it.
pub fn remove_stmt(src: &str, stmt: &Stmt) -> String {
    let bytes = src.as_bytes();
    let mut start = src[..stmt.start].rfind('\n').map_or(0, |n| n + 1);
    if !src[start..stmt.start].trim().is_empty() {
        start = stmt.start;
    }
    let line_end = src[stmt.end..].find('\n').map_or(src.len(), |n| stmt.end + n);
    let end = if src[stmt.end..line_end].trim().trim_start_matches(';').trim().is_empty() { (line_end + 1).min(src.len()) } else { stmt.end };
    // Comment lines right above go too.
    while start > 0 {
        let prev_start = src[..start - 1].rfind('\n').map_or(0, |n| n + 1);
        if src[prev_start..start].trim_start().starts_with("//") {
            start = prev_start;
        } else {
            break;
        }
    }
    let _ = bytes;
    let mut out = src.to_string();
    out.replace_range(start..end, "");
    out
}

/// Write an array of names: one line when it fits, else wrapped with the
/// indentation of the line it is on.
pub fn names_array(names: &[String], indent: &str) -> String {
    let one = format!("[{}]", names.join(", "));
    if one.len() + indent.len() < 96 {
        return one;
    }
    let mut out = String::from("[\n");
    let mut line = String::new();
    for name in names {
        if !line.is_empty() && line.len() + name.len() + indent.len() > 88 {
            out.push_str(&format!("{indent}    {}\n", line.trim_end()));
            line.clear();
        }
        line.push_str(name);
        line.push_str(", ");
    }
    if !line.is_empty() {
        out.push_str(&format!("{indent}    {}\n", line.trim_end().trim_end_matches(',')));
    }
    out.push_str(indent);
    out.push(']');
    out
}

/// The indentation of the line `at` is on.
pub fn indent_of(src: &str, at: usize) -> String {
    let line_start = src[..at.min(src.len())].rfind('\n').map_or(0, |n| n + 1);
    src[line_start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// FNV-1a 64: stable content keys.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overdeep_subtree_keeps_its_range_and_the_reader_continues() {
        let quoted = r#""\"})✓""#;
        let cut = format!("Box /* ( */ {{x: f({} {quoted} // )]\n {} )}}", "[".repeat(4096), "]".repeat(4096));
        let src = format!("let doc = [Wrap{{cut: {cut}}}, 7]\nNext{{ok: true}}");
        let parsed = parse_with(&src, Limits { max_depth: Some(1) });
        let items = parsed.stmts[0].value.arr().unwrap();
        let node = items[0].field("cut").unwrap();
        assert!(matches!(&node.kind, Kind::TooDeep(text) if text == &cut));
        assert_eq!(&src[node.start..node.end], cut);
        assert_eq!(parsed.errors, [(node.start, "values nest too deeply".into())]);
        assert_eq!(items[1].num(), Some(7.0));
        assert_eq!(parsed.stmts[1].value.ty(), Some("Next"));
        let expected = src.clone();
        drop(src);
        assert_eq!(format!("let doc = {}\n{}", write_value(&parsed.stmts[0].value),
                           write_value(&parsed.stmts[1].value)), expected);
    }

    #[test]
    fn nesting_follows_the_descent_budget() {
        fn leaf(parsed: &Parsed) -> &Node {
            let mut node = &parsed.stmts[0].value;
            while let Kind::Arr(items) = &node.kind { node = &items[0]; }
            node
        }

        let src = format!("{}nil{}", "[".repeat(256), "]".repeat(256));
        let marker = 0u8;
        let origin = std::hint::black_box(&marker) as *const u8 as usize;
        let small = parse_with_budget(&src, Limits::default(), origin, 32 * 1024);
        let cut = leaf(&small);
        assert!(matches!(cut.kind, Kind::TooDeep(_)));
        assert_eq!(small.errors, [(cut.start, "values nest too deeply".into())]);

        let large = parse_with_budget(&src, Limits::default(), origin, 4 * 1024 * 1024);
        assert!(large.errors.is_empty());
        assert!(matches!(leaf(&large).kind, Kind::Nil));

        // On platforms without OS stack bounds the default uses the small fallback budget.
        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple", windows))]
        {
            let default = parse(&src);
            assert!(default.errors.is_empty());
            assert!(matches!(leaf(&default).kind, Kind::Nil));
        }
    }

    #[test]
    fn expanded_size_memoizes_fanout_and_reports_cycles() {
        let parsed = parse("let leaf = Box{x: 1}\nlet pair = [leaf leaf]\nf(pair pair @pair \"pair\" unknown)");
        assert!(parsed.errors.is_empty());
        assert_eq!(expanded_size(&parsed), 14);

        let src = "let a = b\nlet b = [a]\na";
        let parsed = parse(src);
        assert_eq!(expanded_size(&parsed), 2);
        assert_eq!(parsed.errors.len(), 1);
        assert_eq!(parsed.errors[0].0, src.find("[a]").unwrap() + 1);
        assert!(parsed.errors[0].1.contains("cycle"));

        let mut src = String::from("let n0 = 1\n");
        for i in 0..64 {
            src.push_str(&format!("let n{} = [n{i} n{i}]\n", i + 1));
        }
        src.push_str("n64");
        let parsed = parse(&src);
        assert!(parsed.errors.is_empty());
        assert_eq!(expanded_size(&parsed), u64::MAX);
    }

    #[test]
    fn read_value_requires_one_complete_value() {
        let src = "/* before */ Box{x: [1, \"}\"]} // after";
        let node = read_value(src).unwrap();
        assert_eq!(&src[node.start..node.end], "Box{x: [1, \"}\"]}");
        for invalid in ["1 2", "Box{x: [1}", "f(1", "1 trailing", "1;", "let a = 1",
                        "/* only */", "name /* open", ",1", "1,", "name, {}"] {
            assert!(!read_value(invalid).unwrap_err().is_empty(), "{invalid}");
        }
    }

    #[test]
    fn reads_values_with_their_bytes() {
        let src = "use mod.storyboard.*\n// a note\nlet a = Note{row: story span: [0.0, 2.5] text: \"hi \\\"there\\\"\" color: #ff5c39, tag: @x}\nStoryboard{items: [a] music: asset(\"ast_1\", \"arev_2\") neg: -1.5e1 ok: true}\n";
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(parsed.stmts.len(), 2);
        let note = &parsed.stmts[0];
        assert_eq!(note.name.as_deref(), Some("a"));
        assert_eq!(note.value.ty(), Some("Note"));
        assert_eq!(note.value.field("text").unwrap().str(), Some("hi \"there\""));
        assert_eq!(note.value.field("row").unwrap().name(), Some("story"));
        assert_eq!(note.value.field("color").unwrap().kind, Kind::Color(0xff5c39ff));
        let span = note.value.field("span").unwrap();
        assert_eq!(&src[span.start..span.end], "[0.0, 2.5]");
        let board = &parsed.stmts[1].value;
        assert_eq!(board.field("neg").unwrap().num(), Some(-15.0));
        match &board.field("music").unwrap().kind {
            Kind::Call { name, args } => assert_eq!((name.as_str(), args[1].str()), ("asset", Some("arev_2"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn errors_name_the_place_and_keep_going() {
        let parsed = parse("let a = Note{text: \"x\"\nlet b = Note{text: \"y\"}\n");
        assert!(!parsed.errors.is_empty());
        let parsed = parse("let = 3\nlet b = 4\n");
        assert_eq!(parsed.stmts.len(), 1);
        assert!(parsed.errors[0].1.contains("needs a name"));
    }

    #[test]
    fn patches_change_only_their_bytes() {
        let src = "let a = Note{\n    // keep me\n    text: \"x\"\n    span: [1.0, 2.0]\n}\n";
        let parsed = parse(src);
        let obj = &parsed.stmts[0].value;
        let out = set_field(src, obj, "text", "\"y\"");
        assert_eq!(out, src.replace("\"x\"", "\"y\""));
        let out = set_field(src, obj, "color", "#fff000");
        assert!(out.contains("    span: [1.0, 2.0]\n    color: #fff000\n}"), "{out}");
        let out = remove_field(src, obj, "text");
        assert_eq!(out, "let a = Note{\n    // keep me\n    span: [1.0, 2.0]\n}\n");
        let one = "let a = Note{text: \"x\", span: [1.0, 2.0]}\n";
        let parsed = parse(one);
        assert_eq!(remove_field(one, &parsed.stmts[0].value, "text"), "let a = Note{span: [1.0, 2.0]}\n");
        assert_eq!(set_field(one, &parsed.stmts[0].value, "z", "1.0"), "let a = Note{text: \"x\", span: [1.0, 2.0] z: 1.0}\n");
    }

    #[test]
    fn literals_round_trip() {
        let src = r#"f(@tag, #aabbcc, false, nil, "a\"b", 0.000001234, [bare, {"two words": mod.x}])"#;
        let parsed = parse(src);
        assert!(parsed.errors.is_empty());
        assert_eq!(write_value(&parsed.stmts[0].value), src);
        for text in ["plain", "quote \" and \\ back", "line\nbreak\ttab", "ünïcødé ✓"] {
            let parsed = parse(&format!("let a = {}", string_literal(text)));
            assert_eq!(parsed.stmts[0].value.str(), Some(text));
        }
        assert_eq!(number(2.0), "2.0");
        assert_eq!(number(1.23456), "1.235");
        assert_eq!(number(-0.0001), "0.0");
        assert_eq!(color_literal(0xff5c39ff), "#ff5c39");
        assert_eq!(parse_color("f00"), Some(0xff0000ff));
        assert!(valid_name("scene_1") && !valid_name("let") && !valid_name("1a"));
    }
}
