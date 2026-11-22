//! Matching rendered G-code against the installed profile's own templates.
//!
//! A template is cut into lines of literal text and placeholders (`{expr}`,
//! `[name]`); `{if}`/`{elsif}`/`{else}`/`{endif}` blocks make their lines
//! optional. Only the command part of a line counts (before `;`, trimmed):
//! comments and blank lines are not compared. A rendered block matches when
//! its lines are, in order, lines of the template (skipping only optional
//! ones) with every literal part byte-equal. Lines that came through a
//! placeholder are marked, so their numbers are checked like Fab's own moves.

/// Literal text, a value capture (a strict decimal), or a capture from a placeholder with a
/// fixed set of choices (an index into [`CHOICES`]).
#[derive(Clone, Debug, PartialEq)]
enum Part { Lit(String), Wild, Choice(usize) }

/// The second family's text placeholders with a fixed set of choices. A capture from one of
/// them must be one of its words; every other capture is a strict decimal.
const CHOICES: &[(&str, &[&str])] = &[("bed_mesh_algo", &["bicubic", "lagrange"])];

/// A choice capture's stand-in while a line passes through its text form (a private-use code
/// point per entry of [`CHOICES`]).
const CHOICE_MARK: u32 = 0xE000;

fn choice_mark(k: usize) -> char { char::from_u32(CHOICE_MARK + k as u32).unwrap_or('\u{1}') }

fn is_capture(p: &Part) -> bool { matches!(p, Part::Wild | Part::Choice(_)) }

#[derive(Clone, Debug)]
pub struct Line { parts: Vec<Part>, pub optional: bool, pub placeholder: bool }

#[derive(Clone, Debug)]
pub struct Template { pub name: String, pub lines: Vec<Line>, named: bool, words: Vec<String> }

/// The command part of a G-code line: before any `;`, trimmed.
pub fn code(line: &str) -> &str {
    line.split(';').next().unwrap_or("").trim()
}

fn control(expr: &str) -> Option<i32> {
    let e = expr.trim();
    let word = e.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
    match word { "if" | "unless" => Some(1), "elsif" | "else" => Some(0), "endif" => Some(-1), _ => None }
}

impl Template {
    pub fn parse(name: &str, text: &str) -> Self {
        // Tokenise: literal chars, placeholders and control tokens; newlines split lines.
        let chars: Vec<char> = text.chars().collect();
        let mut lines = Vec::new();
        let mut parts: Vec<Part> = Vec::new();
        let mut lit = String::new();
        let mut depth = 0i32;
        let mut optional;
        let mut placeholder = false;
        let mut had_control = false;
        let mut i = 0;
        let flush_line = |parts: &mut Vec<Part>, lit: &mut String, optional: &mut bool, placeholder: &mut bool, had_control: &mut bool, lines: &mut Vec<Line>, depth: i32| {
            if !lit.is_empty() { parts.push(Part::Lit(std::mem::take(lit))); }
            let line = Line { parts: std::mem::take(parts), optional: *optional || depth > 0, placeholder: *placeholder };
            // Keep only the command part: a comment cuts the line there.
            let line = cut_comment(line);
            if !line.parts.is_empty() && !(*had_control && line.parts.iter().all(|p| matches!(p, Part::Lit(s) if s.trim().is_empty()))) {
                lines.push(line);
            }
            *optional = depth > 0;
            *placeholder = false;
            *had_control = false;
        };
        optional = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' {
                flush_line(&mut parts, &mut lit, &mut optional, &mut placeholder, &mut had_control, &mut lines, depth);
                i += 1;
                continue;
            }
            if c == '{' {
                // Matching brace, across lines, quotes respected.
                let (mut j, mut nest, mut quoted) = (i + 1, 1, false);
                while j < chars.len() && nest > 0 {
                    match chars[j] { '"' => quoted = !quoted, '{' if !quoted => nest += 1, '}' if !quoted => nest -= 1, _ => {} }
                    j += 1;
                }
                let expr: String = chars[i + 1..j.saturating_sub(1).max(i + 1)].iter().collect();
                if let Some(d) = control(&expr) {
                    if d > 0 { depth += 1; }
                    if d < 0 { depth = (depth - 1).max(0); }
                    optional = true;
                    had_control = true;
                } else {
                    if !lit.is_empty() { parts.push(Part::Lit(std::mem::take(&mut lit))); }
                    parts.push(Part::Wild);
                    placeholder = true;
                }
                i = j;
                continue;
            }
            if c == '[' {
                let j = chars[i + 1..].iter().position(|c| *c == ']' || *c == '\n').map(|p| i + 1 + p);
                if let Some(j) = j.filter(|j| chars[*j] == ']') {
                    let inner: String = chars[i + 1..j].iter().collect();
                    if inner.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && inner.chars().all(|c| c.is_ascii_alphanumeric() || "_[]+-* ".contains(c)) {
                        if !lit.is_empty() { parts.push(Part::Lit(std::mem::take(&mut lit))); }
                        parts.push(Part::Wild);
                        placeholder = true;
                        i = j + 1;
                        continue;
                    }
                }
            }
            lit.push(c);
            i += 1;
        }
        flush_line(&mut parts, &mut lit, &mut optional, &mut placeholder, &mut had_control, &mut lines, depth);
        Template { name: name.to_string(), lines, named: false, words: Vec::new() }
    }

    /// Keep the default family's parser and matching semantics unchanged.
    pub fn for_family(name: &str, text: &str, flavor: super::Flavor) -> Self {
        if flavor != super::Flavor::Named { return Self::parse(name, text); }
        // A template that holds a choice capture's stand-in code point is unusable: its lines
        // are checked as Fab's own.
        if text.chars().any(|c| (c as u32).checked_sub(CHOICE_MARK).is_some_and(|k| (k as usize) < CHOICES.len())) {
            return Self { name: name.into(), lines: Vec::new(), named: true, words: Vec::new() };
        }
        let mut words = word_names(text);
        let lines = named_text(text, &mut words).map(|pieces| {
            let mut output = Output::default();
            output.pieces(&pieces);
            output.finish().into_iter().map(|line| {
                let text: String = line.parts.iter().map(|p| match p { Part::Lit(s) => s.clone(), Part::Wild => "\u{1}".into(), Part::Choice(k) => choice_mark(*k).to_string() }).collect();
                split_choices(finish(vec![Part::Lit(normalize_named(&text))], line.optional, line.placeholder), line.placeholder)
            }).collect()
        }).unwrap_or_default();
        Self { name: name.into(), lines, named: true, words }
    }

    fn matches_line(&self, index: usize, text: &str) -> bool {
        let line = &self.lines[index];
        // A value alone authorizes no line, in every family: a template line that is only a
        // placeholder would otherwise match any line, whatever its command.
        line.parts.iter().any(|p| matches!(p, Part::Lit(s) if !s.trim().is_empty()))
            && if self.named { glob(&line.parts, &normalize_named(text)).is_some() } else { matches_line(line, text) }
    }

    /// A whole literal command, never a command assembled from captured text.
    pub fn literal_command(&self, index: usize, command: &str) -> bool {
        let parts = &self.lines[index].parts;
        let Some(Part::Lit(first)) = parts.first() else { return false };
        let word = first.split_ascii_whitespace().next().unwrap_or("");
        let complete = first.len() > word.len() || parts.len() == 1;
        if complete && word.eq_ignore_ascii_case(command)
            && self.words.iter().any(|w| w.eq_ignore_ascii_case(command)) { return true; }
        // The sole command capture exception: a literal T and one integer capture.
        if first.eq_ignore_ascii_case("T") && matches!(parts.get(1), Some(Part::Wild))
            && (parts.len() == 2 || matches!(parts.get(2), Some(Part::Lit(s)) if s.starts_with(' '))) {
            return command.get(1..).is_some_and(|n| command.starts_with(['T', 't'])
                && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        }
        false
    }

    pub fn word_count(&self, index: usize) -> usize {
        self.lines[index].parts.iter().map(|p| match p { Part::Lit(s) => s.clone(), Part::Wild | Part::Choice(_) => "0".into() })
            .collect::<String>().split_ascii_whitespace().count()
    }

    pub fn is_empty(&self) -> bool { self.lines.is_empty() }

    /// Matches `rendered` (command parts, blank ones already dropped) from its start: the
    /// longest block that is a rendering of the template. Returns the count of lines consumed
    /// and, per rendered line, its template line (None for transparent lines). Exact search over
    /// (template line, rendered line) states, so an optional line that looks like a later
    /// mandatory one cannot derail it.
    pub fn match_at(&self, rendered: &[&str]) -> Option<(usize, Vec<Option<usize>>)> {
        let tl = self.lines.len();
        let w = rendered.len().min(tl * 2 + 64);
        // How each state was reached: 0 unreached, 1 skip optional, 2 match, 3 transparent, 4 start.
        let mut how = vec![0u8; (tl + 1) * (w + 1)];
        let at = |t: usize, r: usize| r * (tl + 1) + t;
        how[at(0, 0)] = 4;
        for r in 0..=w {
            for t in 0..=tl {
                if how[at(t, r)] == 0 || t == tl { continue; }
                if self.lines[t].optional && how[at(t + 1, r)] == 0 { how[at(t + 1, r)] = 1; }
                if r < w {
                    if how[at(t + 1, r + 1)] == 0 && self.matches_line(t, rendered[r]) { how[at(t + 1, r + 1)] = 2; }
                    if how[at(t, r + 1)] == 0 && transparent(rendered[r]) { how[at(t, r + 1)] = 3; }
                }
            }
        }
        let end = (1..=w).rev().find(|r| how[at(tl, *r)] != 0)?;
        // Walk back for the matched template lines.
        let mut matched = vec![None; end];
        let (mut t, mut r) = (tl, end);
        while (t, r) != (0, 0) {
            match how[at(t, r)] {
                1 => t -= 1,
                2 => { t -= 1; r -= 1; matched[r] = Some(t); }
                3 => r -= 1,
                _ => return None,
            }
        }
        Some((end, matched))
    }

    /// The first successful wildcard split, on normalized rendered text.
    pub fn captures(&self, t: usize, rendered: &str) -> Option<Vec<String>> {
        glob(&self.lines.get(t)?.parts, &if self.named { normalize_named(rendered) } else { normalize(rendered) })
    }

    /// Every capture of template line `t` in `rendered` is a strict decimal or, from a
    /// placeholder with fixed choices, one of its words.
    pub fn captures_valid(&self, t: usize, rendered: &str) -> bool {
        let (Some(line), Some(values)) = (self.lines.get(t), self.captures(t, rendered)) else { return false };
        let kinds: Vec<Option<usize>> = line.parts.iter().filter_map(|p| match p { Part::Wild => Some(None), Part::Choice(k) => Some(Some(*k)), Part::Lit(_) => None }).collect();
        values.len() == kinds.len() && values.iter().zip(kinds).all(|(v, k)| match k {
            None => decimal(v),
            Some(k) => CHOICES.get(k).is_some_and(|(_, words)| words.contains(&v.as_str())),
        })
    }

    /// The first line that must appear (a block starts with it, or with an optional one before it).
    pub fn could_start(&self, line: &str) -> bool {
        for (i, l) in self.lines.iter().enumerate() {
            if self.matches_line(i, line) { return true; }
            if !l.optional { return false; }
        }
        false
    }
}

#[cfg(test)]
impl Template {
    /// Where a match stops: the rendered line and the next mandatory template line.
    pub fn explain(&self, rendered: &[&str]) -> String {
        let (mut t, mut r) = (0, 0);
        while r < rendered.len() {
            let mut k = t;
            let found = loop {
                if k >= self.lines.len() { break None; }
                if matches_line(&self.lines[k], rendered[r]) { break Some(k); }
                if !self.lines[k].optional { break None; }
                k += 1;
            };
            match found { Some(k) => { t = k + 1; r += 1; } None if transparent(rendered[r]) => r += 1, None => break }
        }
        let want = self.lines[t..].iter().find(|l| !l.optional).map(|l| format!("{:?}", l.parts)).unwrap_or_default();
        format!("{}: matched {r} lines; next rendered {:?}; next mandatory {want}; first lines {:?}", self.name, rendered.get(r), self.lines.iter().take(3).map(|l| (&l.parts, l.optional)).collect::<Vec<_>>())
    }
}

/// A progress line (`M73 P.. R..`, inserted by time estimates) or a bare feed-rate move: never motion.
fn transparent(line: &str) -> bool {
    super::split_cmd(line).is_ok_and(|(cmd, _)| cmd == "M73") || feed_only(line)
}

/// A move with only feed words (placeholder markers are allowed while parsing a template).
fn feed_only(line: &str) -> bool {
    let Ok((cmd, rest)) = super::split_cmd(line) else { return false };
    matches!(cmd, "G0" | "G1" | "G2" | "G3") && rest.split_ascii_whitespace().all(|w| {
        w.strip_prefix('F').is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-' | 1)))
    })
}

fn cut_comment(line: Line) -> Line {
    let mut out = Vec::new();
    for p in line.parts {
        match p {
            Part::Lit(s) => match s.find(';') {
                Some(at) => { out.push(Part::Lit(s[..at].to_string())); return finish(out, line.optional, line.placeholder); }
                None => out.push(Part::Lit(s)),
            },
            w => out.push(w),
        }
    }
    finish(out, line.optional, line.placeholder)
}

/// The comparable form: whitespace runs collapsed, all words retained.
pub fn normalize(code: &str) -> String {
    code.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn finish(parts: Vec<Part>, optional: bool, placeholder: bool) -> Line {
    // Normalise as rendered lines are: through a text form with a marker per placeholder.
    const MARK: char = '\u{1}';
    let choices = parts.iter().any(|p| matches!(p, Part::Choice(_)));
    let text: String = parts.iter().map(|p| match p { Part::Lit(s) => s.clone(), Part::Wild => MARK.to_string(), Part::Choice(k) => choice_mark(*k).to_string() }).collect();
    let text = normalize(&text);
    let mut parts = Vec::new();
    let mut lit = String::new();
    for c in text.chars() {
        if c == MARK { if !lit.is_empty() { parts.push(Part::Lit(std::mem::take(&mut lit))); } parts.push(Part::Wild); } else { lit.push(c); }
    }
    if !lit.is_empty() { parts.push(Part::Lit(lit)); }
    // A bare feed-rate line may be dropped by a renderer.
    let optional = optional || feed_only(&text);
    let line = finish_parts(parts, optional, placeholder);
    // Only a line that had choice captures gets them back (the default family never has any).
    if choices { split_choices(line, placeholder) } else { line }
}

fn finish_parts(parts: Vec<Part>, optional: bool, placeholder: bool) -> Line {
    // Trim the outer whitespace; an all-blank line has no parts.
    let mut parts: Vec<Part> = parts.into_iter().filter(|p| !matches!(p, Part::Lit(s) if s.is_empty())).collect();
    if let Some(Part::Lit(s)) = parts.first_mut() { *s = s.trim_start().to_string(); }
    if let Some(Part::Lit(s)) = parts.last_mut() { *s = s.trim_end().to_string(); }
    parts.retain(|p| !matches!(p, Part::Lit(s) if s.is_empty()));
    let placeholder = placeholder && parts.iter().any(is_capture);
    Line { parts, optional, placeholder }
}

/// A choice capture back from its stand-in code point.
fn split_choices(line: Line, placeholder: bool) -> Line {
    let mut parts = Vec::new();
    for p in line.parts {
        let Part::Lit(s) = p else { parts.push(p); continue };
        let mut lit = String::new();
        for c in s.chars() {
            match (c as u32).checked_sub(CHOICE_MARK).map(|k| k as usize).filter(|k| *k < CHOICES.len()) {
                Some(k) => { if !lit.is_empty() { parts.push(Part::Lit(std::mem::take(&mut lit))); } parts.push(Part::Choice(k)); }
                None => lit.push(c),
            }
        }
        if !lit.is_empty() { parts.push(Part::Lit(lit)); }
    }
    let placeholder = placeholder && parts.iter().any(is_capture);
    Line { parts, optional: line.optional, placeholder }
}

fn matches_line(line: &Line, text: &str) -> bool { glob(&line.parts, &normalize(text)).is_some() }

fn glob(parts: &[Part], text: &str) -> Option<Vec<String>> {
    match parts.first() {
        None => text.is_empty().then(Vec::new),
        Some(Part::Lit(s)) => glob(&parts[1..], text.strip_prefix(s.as_str())?),
        Some(Part::Wild | Part::Choice(_)) => {
            if parts.len() == 1 { return Some(vec![text.to_string()]); }
            for i in (0..=text.len()).filter(|i| text.is_char_boundary(*i)) {
                if let Some(mut captures) = glob(&parts[1..], &text[i..]) {
                    captures.insert(0, text[..i].to_string());
                    return Some(captures);
                }
            }
            None
        }
    }
}

// The second family's code is parsed as output pieces, never as one wildcard line.
#[derive(Clone, Debug)]
enum Piece { Text(String), Value, Choice(usize), Branch(Vec<Vec<Piece>>) }

/// A placeholder or operand that is exactly a name with fixed choices.
fn choice(ts: &[Token]) -> Option<usize> {
    let [Token::Name(n)] = ts else { return None };
    CHOICES.iter().position(|(name, _)| name == n)
}

#[derive(Clone, Debug, PartialEq)]
enum Token { Name(String), String(String), Symbol(char) }

fn word_names(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '.')
        .filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// Quotes and brackets are validated even in statements that produce no output.
fn tokens(text: &str, words: &mut Vec<String>) -> Option<Vec<Token>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut brackets = Vec::new();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut s = String::new();
            loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => s.push(match chars.next()? { 'n' => '\n', 'r' => '\r', '"' => '"', '\\' => '\\', _ => return None }),
                    c => s.push(c),
                }
            }
            words.extend(word_names(&s));
            out.push(Token::String(s));
        } else {
            if !c.is_ascii() || (c.is_ascii_control() && !c.is_ascii_whitespace()) { return None; }
            if c.is_ascii_whitespace() { continue; }
            if c.is_ascii_alphanumeric() || c == '_' {
                let mut s = c.to_string();
                while chars.peek().is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') { s.push(chars.next()?); }
                out.push(Token::Name(s));
            } else {
                match c {
                    '(' | '[' => brackets.push(c),
                    ')' if brackets.pop() != Some('(') => return None,
                    ']' if brackets.pop() != Some('[') => return None,
                    '{' | '}' | '\\' => return None,
                    _ => {}
                }
                out.push(Token::Symbol(c));
            }
        }
    }
    brackets.is_empty().then_some(out)
}

fn name(token: Option<&Token>, want: &str) -> bool { matches!(token, Some(Token::Name(n)) if n == want) }

fn expression(ts: &[Token]) -> Option<Vec<Piece>> {
    if ts.is_empty() { return None; }
    let mut depth = 0;
    let mut cuts = vec![0];
    for (i, t) in ts.iter().enumerate() {
        match t {
            Token::Symbol('(' | '[') => depth += 1,
            Token::Symbol(')' | ']') => depth -= 1,
            // A choice of values is one value, including any strings within it.
            Token::Symbol('?') if depth == 0 => return Some(vec![Piece::Value]),
            Token::Symbol('+') if depth == 0 => cuts.push(i + 1),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (i, &start) in cuts.iter().enumerate() {
        let end = cuts.get(i + 1).map_or(ts.len(), |next| next - 1);
        let operand = &ts[start..end];
        if operand.is_empty() { return None; }
        out.push(match operand { [Token::String(s)] => Piece::Text(s.clone()), op => choice(op).map_or(Piece::Value, Piece::Choice) });
    }
    Some(out)
}

fn statements(ts: &[Token], at: &mut usize, nested: bool) -> Option<Vec<Piece>> {
    let mut pieces = Vec::new();
    while *at < ts.len() {
        if ["elsif", "else", "endif"].iter().any(|n| name(ts.get(*at), n)) {
            return nested.then_some(pieces);
        }
        if ts[*at] == Token::Symbol(';') { *at += 1; continue; }
        if name(ts.get(*at), "if") {
            let mut branches = Vec::new();
            loop {
                *at += 1; // if or elsif
                let condition = *at;
                let mut depth = 0;
                while *at < ts.len() && !(depth == 0 && name(ts.get(*at), "then")) {
                    match ts[*at] { Token::Symbol('(' | '[') => depth += 1, Token::Symbol(')' | ']') => depth -= 1, _ => {} }
                    *at += 1;
                }
                if *at == ts.len() || *at == condition { return None; }
                *at += 1;
                branches.push(statements(ts, at, true)?);
                if !name(ts.get(*at), "elsif") { break; }
            }
            if name(ts.get(*at), "else") { *at += 1; branches.push(statements(ts, at, true)?); }
            if !name(ts.get(*at), "endif") { return None; }
            *at += 1;
            pieces.push(Piece::Branch(branches));
            continue;
        }
        let start = *at;
        let mut depth = 0;
        while *at < ts.len() {
            let t = &ts[*at];
            if depth == 0 && (*t == Token::Symbol(';') || ["elsif", "else", "endif"].iter().any(|n| name(Some(t), n))) { break; }
            match t { Token::Symbol('(' | '[') => depth += 1, Token::Symbol(')' | ']') => depth -= 1, _ => {} }
            *at += 1;
        }
        let statement = &ts[start..*at];
        if statement.is_empty() { return None; }
        let mut depth = 0;
        let assignment = statement.iter().enumerate().any(|(i, t)| {
            match t { Token::Symbol('(' | '[') => depth += 1, Token::Symbol(')' | ']') => depth -= 1, _ => {} }
            depth == 0 && *t == Token::Symbol('=') && statement.get(i + 1) != Some(&Token::Symbol('='))
                && i.checked_sub(1).and_then(|i| statement.get(i)).is_none_or(|t| !matches!(t, Token::Symbol('=' | '!' | '<' | '>')))
        });
        if !assignment && !name(statement.first(), "local") && !name(statement.first(), "global") {
            pieces.extend(expression(statement)?);
        }
    }
    (!nested).then_some(pieces)
}

fn named_text(text: &str, words: &mut Vec<String>) -> Option<Vec<Piece>> {
    let mut out = Vec::new();
    let mut branches: Vec<(Vec<Piece>, Vec<Vec<Piece>>)> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut literal = String::new();
    while let Some(c) = chars.next() {
        if !matches!(c, '{' | '[' | '}') { literal.push(c); continue; }
        if c == '}' { continue; }
        if !literal.is_empty() { out.push(Piece::Text(std::mem::take(&mut literal))); }
        let closing = if c == '{' { '}' } else { ']' };
        let mut expr = String::new();
        let (mut quoted, mut escaped, mut depth) = (false, false, 1);
        loop {
            let x = chars.next()?;
            if quoted {
                if escaped { escaped = false; }
                else if x == '\\' { escaped = true; }
                else if x == '"' { quoted = false; }
            } else if x == '"' { quoted = true; }
            else if x == c { depth += 1; }
            else if x == closing { depth -= 1; if depth == 0 { break; } }
            expr.push(x);
        }
        let ts = tokens(&expr, words)?;
        if c == '[' { if ts.is_empty() { return None; } out.push(choice(&ts).map_or(Piece::Value, Piece::Choice)); continue; }
        let text_control = control(&expr).filter(|_| !ts.iter().any(|t| name(Some(t), "then")));
        match text_control {
            Some(1) => { branches.push((std::mem::take(&mut out), Vec::new())); }
            Some(0) => { branches.last_mut()?.1.push(std::mem::take(&mut out)); }
            Some(-1) => {
                let (mut before, mut arms) = branches.pop()?;
                arms.push(std::mem::take(&mut out));
                before.push(Piece::Branch(arms));
                out = before;
            }
            _ => out.extend(statements(&ts, &mut 0, false)?),
        }
    }
    if !literal.is_empty() { out.push(Piece::Text(literal)); }
    branches.is_empty().then_some(out)
}

/// Newlines in emitted strings have exactly the same effect as newlines in text.
#[derive(Default)]
struct Output { lines: Vec<Line>, parts: Vec<Part>, value_tail: bool }

impl Output {
    /// One capture for values that print next to each other: `layer_num + 1` adds two numbers,
    /// and a value joined to a value is one piece of text.
    fn value(&mut self) {
        if !(self.value_tail && self.parts.last() == Some(&Part::Wild)) { self.parts.push(Part::Wild); }
        self.value_tail = true;
    }

    /// A branch's part of a line is a capture of its own, never merged with a value.
    fn wild(&mut self) {
        self.parts.push(Part::Wild);
        self.value_tail = false;
    }

    fn line(&mut self, optional: bool) {
        self.value_tail = false;
        let parts = std::mem::take(&mut self.parts);
        let placeholder = parts.iter().any(is_capture);
        self.lines.push(cut_comment(Line { parts, optional, placeholder }));
    }

    fn pieces(&mut self, pieces: &[Piece]) {
        for piece in pieces {
            match piece {
                Piece::Value => self.value(),
                Piece::Choice(k) => { self.parts.push(Part::Choice(*k)); self.value_tail = false; }
                Piece::Text(s) => {
                    for (i, s) in s.split('\n').enumerate() {
                        if i > 0 { self.line(false); }
                        if !s.is_empty() { self.parts.push(Part::Lit(s.into())); self.value_tail = false; }
                    }
                }
                Piece::Branch(arms) => {
                    let prefix = !self.parts.is_empty();
                    let mut partial = false;
                    let mut inside = Vec::new();
                    let mut breaks = false;
                    for arm in arms {
                        let mut output = Output::default();
                        output.pieces(arm);
                        if !output.lines.is_empty() {
                            breaks = true;
                            if prefix { output.lines.remove(0); }
                            inside.extend(output.lines.into_iter().map(|mut l| { l.optional = true; l }));
                        }
                        partial |= !output.parts.is_empty();
                    }
                    if prefix && breaks { self.wild(); self.line(false); }
                    self.lines.extend(inside);
                    if partial { self.wild(); }
                }
            }
        }
    }

    fn finish(mut self) -> Vec<Line> {
        self.line(false);
        self.lines.into_iter().filter(|l| !l.parts.is_empty()).collect()
    }
}

/// Command names and key names are case-insensitive; arbitrary macro text values are not.
fn normalize_named(text: &str) -> String {
    let mut words = text.split_whitespace();
    let command = words.next().unwrap_or("").to_ascii_uppercase();
    let object = command.starts_with("EXCLUDE_OBJECT_");
    let ordinary = super::split_cmd(&command).is_ok();
    let mut out = command;
    for word in words {
        out.push(' ');
        if let Some((key, value)) = word.split_once('=') {
            out.push_str(&key.to_ascii_uppercase());
            out.push('=');
            if object && key.eq_ignore_ascii_case("NAME") { out.push_str(&value.to_ascii_uppercase()); }
            else { out.push_str(value); }
        } else if ordinary {
            out.push_str(&word.to_ascii_uppercase());
        } else { out.push_str(word); }
    }
    out
}

/// A capture is one decimal value, never executable syntax or an exponent.
pub fn decimal(s: &str) -> bool {
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (whole, fraction) = s.split_once('.').map_or((s, None), |(a, b)| (a, Some(b)));
    !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit())
        && fraction.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_placeholders_and_conditionals() {
        let t = Template::parse("t", "; head\nG28 ; home\n{if a > 1}\nM104 S[temp]\n{else}\nM104 S200\n{endif}\nG1 Z{z + 3} F1200\n{if (x == \"A\")\n || (y)}\nM1 P1\n{endif}\nM400\n");
        assert_eq!(t.lines.len(), 6);
        let ok = ["G28", "M104 S215", "G1 Z3.2 F1200", "M400"];
        assert_eq!(t.match_at(&ok).map(|m| m.0), Some(4));
        assert_eq!(t.match_at(&ok).unwrap().1.iter().map(|l| l.is_some_and(|i| t.lines[i].placeholder)).collect::<Vec<_>>(), vec![false, true, true, false]);
        assert_eq!(t.captures(3, "G1  Z3.2 F1200"), Some(vec!["3.2".into()]));
        assert_eq!(t.match_at(&["G28", "M104 S200", "G1 Z3.2 F1200", "M1 P1", "M400", "G1 X5"]).map(|m| m.0), Some(5), "stops at the first line after the block");
        assert!(t.match_at(&["G28", "G1 Z3.2 F1200"]).is_none(), "a mandatory line is missing");
        assert!(t.match_at(&["G28", "M104 S215", "G1 X3.2 F1200", "M400"]).is_none(), "a literal part differs");
        assert!(t.match_at(&["G28", "M104 S215", "G1 Z3.2 F600", "M400"]).is_none(), "a literal feed differs");
        assert_eq!(t.match_at(&["G28", "M104 S215", "G1  Z3.2 F1200", "M73 P5 R2", "G1 F600", "M400"]).map(|m| m.0), Some(6), "spacing, progress and bare feed lines do not count");
        assert!(t.match_at(&["G28 X", "M400"]).is_none());
        assert!(t.could_start("G28") && !t.could_start("M400"));
    }
}
