//! Bus expansion and whole-name automata. Matching never backtracks.

#[derive(Clone, Copy)]
struct Letter { c: char, literal: bool }

/// Expanded members and whether a range inside formatting had to remain literal.
pub fn expand_bus(pattern: &str) -> (Vec<String>, bool) {
    let mut letters = Vec::new(); let mut chars = pattern.chars().peekable(); let mut quoted = false;
    while let Some(c) = chars.next() {
        if c == '"' { quoted = !quoted; continue; }
        if c == '\\' && (quoted || chars.peek() == Some(&' ')) {
            if let Some(c) = chars.next() { letters.push(Letter { c, literal: true }); continue; }
        }
        letters.push(Letter { c, literal: quoted });
    }
    if quoted { return (vec![pattern.into()], false); }
    let mut pending = vec![(letters, false)]; let mut out = Vec::new(); let mut approximated = false;
    while let Some((word, member)) = pending.pop() {
        let mut formatting = 0usize; let mut group = None; let mut vector = None;
        for (i, l) in word.iter().enumerate() {
            if l.literal { continue; }
            if l.c == '{' {
                if i > 0 && matches!(word[i - 1].c, '_' | '^' | '~') { formatting += 1; }
                else if formatting == 0 { group = Some(i); break; }
                else { formatting += 1; }
            } else if l.c == '}' { formatting = formatting.saturating_sub(1); }
            else if l.c == '[' {
                if formatting > 0 {
                    if word[i..].windows(2).any(|w| w[0].c == '.' && w[1].c == '.') { approximated = true; }
                } else { vector = Some(i); }
                break;
            }
        }
        if let Some(open) = group {
            if word[..open].iter().all(|l| l.c != ' ' && !matches!(l.c, '[' | ']')) {
                let mut depth = 1usize; let mut end = None;
                for (i, l) in word.iter().enumerate().skip(open + 1) {
                    if !l.literal {
                        if l.c == '{' { depth += 1; }
                        if l.c == '}' { depth -= 1; if depth == 0 { end = Some(i); break; } }
                    }
                }
                if end == Some(word.len().saturating_sub(1)) {
                    let mut parts = Vec::new(); let mut start = open + 1; let mut depth = 0usize;
                    for i in open + 1..word.len() - 1 {
                        let l = word[i];
                        if !l.literal {
                            if l.c == '{' { depth += 1; }
                            if l.c == '}' { depth = depth.saturating_sub(1); }
                            if depth == 0 && (l.c == ',' || l.c == ' ') {
                                if start < i { parts.push(word[start..i].to_vec()); } start = i + 1;
                            }
                        }
                    }
                    if start < word.len() - 1 { parts.push(word[start..word.len() - 1].to_vec()); }
                    pending.extend(parts.into_iter().rev().map(|p| (p, true)));
                    continue;
                }
            }
        }
        if let Some(open) = vector {
            if word[..open].iter().all(|l| l.c != ' ' && !matches!(l.c, '[' | ']')) {
                if let Some(close) = word.iter().enumerate().skip(open + 1).find(|(_, l)| l.c == ']' && !l.literal).map(|(i, _)| i) {
                    let range: String = word[open + 1..close].iter().map(|l| l.c).collect();
                    if let Some((a, b)) = range.split_once("..") {
                        if !a.is_empty() && !b.is_empty() && a.bytes().all(|b| b.is_ascii_digit()) && b.bytes().all(|b| b.is_ascii_digit())
                            && word[close + 1..].iter().all(|l| matches!(l.c, '+' | '-' | 'P' | 'N' | '}')) {
                            let canon = |s: &str| { let s = s.trim_start_matches('0'); if s.is_empty() { "0".to_string() } else { s.to_string() } };
                            let (a_num, b_num) = (canon(a), canon(b));
                            if a_num != b_num {
                                let width = if (a.len() > 1 && a.starts_with('0')) || (b.len() > 1 && b.starts_with('0')) { a.len().max(b.len()) } else { 0 };
                                let (mut low, high) = if (a_num.len(), &a_num) < (b_num.len(), &b_num) { (a_num, b_num) } else { (b_num, a_num) };
                                let prefix: String = word[..open].iter().map(|l| l.c).collect();
                                let suffix: String = word[close + 1..].iter().map(|l| l.c).collect();
                                loop {
                                    let name = format!("{prefix}{low:0>width$}{suffix}");
                                    out.push(if member { name.replace('/', "{slash}") } else { name });
                                    if low == high { break; }
                                    let mut digits = low.into_bytes(); let mut carry = true;
                                    for d in digits.iter_mut().rev() { if *d == b'9' { *d = b'0'; } else { *d += 1; carry = false; break; } }
                                    if carry { digits.insert(0, b'1'); }
                                    low = digits.into_iter().map(char::from).collect();
                                }
                                continue;
                            }
                        }
                    }
                }
            }
        }
        let name: String = word.iter().map(|l| l.c).collect();
        out.push(if member { name.replace('/', "{slash}") } else { name });
    }
    (out, approximated)
}

#[derive(Clone, Debug)]
enum Part { Range(char, char), Digit(bool), Space(bool), Word(bool) }
impl Part {
    fn matches(&self, c: char) -> bool {
        match self {
            Self::Range(a, b) => *a <= c && c <= *b,
            Self::Digit(neg) => c.is_ascii_digit() != *neg,
            Self::Space(neg) => c.is_whitespace() != *neg,
            Self::Word(neg) => (c.is_alphanumeric() || c == '_') != *neg,
        }
    }
}
#[derive(Clone, Debug)]
enum Atom { Literal(char), Any, Set { neg: bool, parts: Vec<Part> } }
impl Atom {
    fn matches(&self, c: char) -> bool {
        match self { Self::Literal(a) => *a == c, Self::Any => true, Self::Set { neg, parts } => parts.iter().any(|p| p.matches(c)) != *neg }
    }
}
#[derive(Debug)]
enum Expr { Empty, Atom(Atom), Concat(Vec<usize>), Alt(Vec<usize>), Repeat { child: usize, min: usize, max: Option<usize> } }
#[derive(Default)]
struct Group { sequence: Vec<usize>, alternatives: Vec<usize> }
fn node(arena: &mut Vec<Expr>, expr: Expr) -> usize { let id = arena.len(); arena.push(expr); id }
fn sequence(arena: &mut Vec<Expr>, seq: Vec<usize>) -> usize {
    if seq.len() == 1 { seq[0] } else { node(arena, Expr::Concat(seq)) }
}
fn finish_group(arena: &mut Vec<Expr>, group: Group) -> usize {
    let mut branches = group.alternatives; branches.push(sequence(arena, group.sequence));
    if branches.len() == 1 { branches[0] } else { node(arena, Expr::Alt(branches)) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error { Malformed, Unsupported }
struct RegexReader { chars: Vec<char>, pos: usize, unsupported: bool }
impl RegexReader {
    fn peek(&self) -> Option<char> { self.chars.get(self.pos).copied() }
    fn take(&mut self) -> Result<char, Error> { let c = self.peek().ok_or(Error::Malformed)?; self.pos += 1; Ok(c) }
    fn escape(&mut self) -> Result<Atom, Error> {
        let c = self.take()?;
        let part = match c {
            'd' | 'D' => Part::Digit(c == 'D'), 's' | 'S' => Part::Space(c == 'S'), 'w' | 'W' => Part::Word(c == 'W'),
            _ => {
                if c.is_alphanumeric() {
                    if c.is_ascii_digit() || matches!(c, 'a' | 'b' | 'B' | 'c' | 'e' | 'f' | 'n' | 'r' | 't' | 'v' | 'V' | 'x' | 'u' | 'U' | 'A' | 'Z' | 'z' | 'G' | 'k' | 'g' | 'p' | 'P' | 'Q' | 'E' | 'R' | 'X' | 'h' | 'H' | 'N') {
                        self.unsupported = true;
                    } else { return Err(Error::Malformed); }
                }
                return Ok(Atom::Literal(c));
            }
        };
        Ok(Atom::Set { neg: false, parts: vec![part] })
    }
    fn bracket_atom(&mut self) -> Result<Atom, Error> { if self.peek() == Some('\\') { self.pos += 1; self.escape() } else { Ok(Atom::Literal(self.take()?)) } }
    fn bracket(&mut self) -> Result<Atom, Error> {
        let neg = self.peek() == Some('^'); if neg { self.pos += 1; }
        let mut parts = Vec::new(); let mut first = true;
        loop {
            if self.peek() == Some(']') && !first { self.pos += 1; return Ok(Atom::Set { neg, parts }); }
            first = false;
            if self.peek() == Some('[') && self.chars.get(self.pos + 1).is_some_and(|c| matches!(c, ':' | '.' | '=')) {
                self.unsupported = true; self.pos += 1; let delimiter = self.take()?;
                loop { let c = self.take()?; if c == delimiter && self.peek() == Some(']') { self.pos += 1; break; } }
                continue;
            }
            let a = self.bracket_atom()?;
            if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                self.pos += 1; let b = self.bracket_atom()?;
                match (a, b) { (Atom::Literal(a), Atom::Literal(b)) if a <= b => parts.push(Part::Range(a, b)), _ => return Err(Error::Malformed) }
            } else {
                match a { Atom::Literal(c) => parts.push(Part::Range(c, c)), Atom::Set { parts: p, .. } => parts.extend(p), Atom::Any => () }
            }
        }
    }
    fn integer(&mut self) -> Result<usize, Error> {
        let mut n = 0usize; let start = self.pos;
        while let Some(c) = self.peek().filter(|c| c.is_ascii_digit()) {
            n = n.checked_mul(10).and_then(|n| n.checked_add(c as usize - '0' as usize)).ok_or(Error::Malformed)?; self.pos += 1;
        }
        if self.pos == start { Err(Error::Malformed) } else { Ok(n) }
    }
    fn quantifier(&mut self) -> Result<Option<(usize, Option<usize>)>, Error> {
        let bounds = match self.peek() {
            Some('*') => { self.pos += 1; (0, None) }
            Some('+') => { self.pos += 1; (1, None) }
            Some('?') => { self.pos += 1; (0, Some(1)) }
            Some('{') if self.chars.get(self.pos + 1).is_some_and(|c| c.is_ascii_digit()) => {
                self.pos += 1; let min = self.integer()?;
                let max = if self.peek() == Some(',') {
                    self.pos += 1; if self.peek() == Some('}') { None } else { Some(self.integer()?) }
                } else { Some(min) };
                if self.take()? != '}' || max.is_some_and(|n| min > n) { return Err(Error::Malformed); }
                (min, max)
            }
            _ => return Ok(None),
        };
        if self.peek() == Some('?') { self.pos += 1; }
        Ok(Some(bounds))
    }
}
fn regex(pattern: &str) -> Result<(Vec<Expr>, usize), Error> {
    let mut r = RegexReader { chars: pattern.chars().collect(), pos: 0, unsupported: false };
    if r.peek() == Some('^') { r.pos += 1; }
    let mut arena = Vec::new(); let mut groups: Vec<Group> = Vec::new(); let mut group = Group::default();
    while let Some(c) = r.peek() {
        r.pos += 1;
        let mut id = match c {
            '(' => {
                if r.peek() == Some('?') {
                    r.pos += 1;
                    if r.peek() == Some(':') { r.pos += 1; }
                    else {
                        r.unsupported = true;
                        if matches!(r.peek(), Some('=' | '!')) { r.pos += 1; }
                        else if r.peek() == Some('<') {
                            r.pos += 1; if !matches!(r.take()?, '=' | '!') { return Err(Error::Malformed); }
                        } else {
                            while r.peek().is_some_and(|c| c.is_ascii_alphabetic() || c == '-') { r.pos += 1; }
                            if r.peek() == Some(':') { r.pos += 1; }
                            else if r.peek() != Some(')') { return Err(Error::Malformed); }
                        }
                    }
                }
                groups.push(std::mem::take(&mut group)); continue;
            }
            ')' => {
                let closed = finish_group(&mut arena, std::mem::take(&mut group));
                group = groups.pop().ok_or(Error::Malformed)?; closed
            }
            '|' => { let seq = std::mem::take(&mut group.sequence); group.alternatives.push(sequence(&mut arena, seq)); continue; }
            '[' => node(&mut arena, Expr::Atom(r.bracket()?)),
            '\\' => node(&mut arena, Expr::Atom(r.escape()?)),
            '.' => node(&mut arena, Expr::Atom(Atom::Any)),
            '*' | '+' | '?' => return Err(Error::Malformed),
            '{' if r.peek().is_some_and(|c| c.is_ascii_digit()) => return Err(Error::Malformed),
            '$' if r.pos == r.chars.len() => node(&mut arena, Expr::Empty),
            '^' | '$' => { r.unsupported = true; node(&mut arena, Expr::Empty) }
            _ => node(&mut arena, Expr::Atom(Atom::Literal(c))),
        };
        if let Some((min, max)) = r.quantifier()? { id = node(&mut arena, Expr::Repeat { child: id, min, max }); }
        group.sequence.push(id);
    }
    if !groups.is_empty() { return Err(Error::Malformed); }
    if r.unsupported { return Err(Error::Unsupported); }
    let root = finish_group(&mut arena, group); Ok((arena, root))
}

#[derive(Clone, Debug)]
enum State { Char(Atom, usize), Split(usize, usize), Jump(usize), Accept }
#[derive(Clone, Debug)]
enum Fast { Exact(String), Prefix(String), Suffix(String), Contains(String) }
#[derive(Clone, Debug)]
struct Machine { states: Vec<State>, start: usize, prefix: String, fast: Option<Fast> }
struct Fragment { start: usize, exits: Vec<(usize, bool)> }
fn state(states: &mut Vec<State>, value: State) -> usize { let id = states.len(); states.push(value); id }
fn patch(states: &mut [State], exits: Vec<(usize, bool)>, target: usize) {
    for (id, second) in exits {
        match &mut states[id] {
            State::Char(_, to) | State::Jump(to) => *to = target,
            State::Split(a, b) => if second { *b = target; } else { *a = target; },
            State::Accept => (),
        }
    }
}
fn concat(states: &mut Vec<State>, parts: Vec<Fragment>) -> Fragment {
    let mut iter = parts.into_iter();
    let Some(mut out) = iter.next() else { let id = state(states, State::Jump(0)); return Fragment { start: id, exits: vec![(id, false)] }; };
    for next in iter { patch(states, out.exits, next.start); out.exits = next.exits; }
    out
}
fn compile(arena: &[Expr], root: usize) -> Machine {
    let mut states = Vec::new(); let mut pending = vec![(root, false)]; let mut built = Vec::new();
    while let Some((id, ready)) = pending.pop() {
        if !ready {
            pending.push((id, true));
            match &arena[id] {
                Expr::Concat(ids) | Expr::Alt(ids) => pending.extend(ids.iter().rev().map(|&id| (id, false))),
                Expr::Repeat { child, min, max } => for _ in 0..max.unwrap_or_else(|| min.saturating_add(1)) { pending.push((*child, false)); },
                _ => (),
            }
            continue;
        }
        let part = match &arena[id] {
            Expr::Empty => concat(&mut states, Vec::new()),
            Expr::Atom(atom) => { let start = state(&mut states, State::Char(atom.clone(), 0)); Fragment { start, exits: vec![(start, false)] } }
            Expr::Concat(ids) => { let parts = built.split_off(built.len() - ids.len()); concat(&mut states, parts) }
            Expr::Alt(ids) => {
                let parts = built.split_off(built.len() - ids.len()); let mut iter = parts.into_iter();
                let mut out = iter.next().unwrap_or_else(|| concat(&mut states, Vec::new()));
                for next in iter { out.start = state(&mut states, State::Split(out.start, next.start)); out.exits.extend(next.exits); } out
            }
            Expr::Repeat { min, max, .. } => {
                let count = max.unwrap_or_else(|| min.saturating_add(1));
                let mut parts = built.split_off(built.len() - count);
                for (i, part) in parts.iter_mut().enumerate().skip(*min) {
                    let split = state(&mut states, State::Split(part.start, 0));
                    if max.is_none() && i == *min { patch(&mut states, std::mem::take(&mut part.exits), split); }
                    part.exits.push((split, true)); part.start = split;
                }
                concat(&mut states, parts)
            }
        };
        built.push(part);
    }
    let part = built.pop().unwrap_or_else(|| concat(&mut states, Vec::new()));
    let accept = state(&mut states, State::Accept); patch(&mut states, part.exits, accept);
    let mut prefix = String::new(); let mut at = part.start;
    for _ in 0..states.len() {
        match states[at] { State::Char(Atom::Literal(c), to) => { prefix.push(c); at = to; }, State::Jump(to) => at = to, _ => break }
    }
    let fast = matches!(states[at], State::Accept).then(|| Fast::Exact(prefix.clone()));
    Machine { states, start: part.start, prefix, fast }
}
impl Machine {
    fn close_small(&self, mut todo: u128) -> u128 {
        let mut seen = 0; let mut active = 0;
        while todo != 0 {
            let i = todo.trailing_zeros() as usize; let bit = 1u128 << i; todo &= !bit;
            if seen & bit != 0 { continue; } seen |= bit;
            match self.states[i] {
                State::Split(a, b) => todo |= ((1u128 << a) | (1u128 << b)) & !seen,
                State::Jump(to) => todo |= (1u128 << to) & !seen,
                _ => active |= bit,
            }
        }
        active
    }
    fn close_large(&self, pending: &mut Vec<usize>, active: &mut Vec<usize>, seen: &mut [usize], generation: usize) {
        active.clear();
        while let Some(i) = pending.pop() {
            if seen[i] == generation { continue; } seen[i] = generation;
            match self.states[i] { State::Split(a, b) => { pending.push(a); pending.push(b); }, State::Jump(to) => pending.push(to), _ => active.push(i) }
        }
    }
    fn matches(&self, name: &str) -> bool {
        if let Some(fast) = &self.fast {
            return match fast { Fast::Exact(s) => name == s, Fast::Prefix(s) => name.starts_with(s), Fast::Suffix(s) => name.ends_with(s), Fast::Contains(s) => name.contains(s) };
        }
        if !name.starts_with(&self.prefix) { return false; }
        if self.states.len() <= 128 {
            let mut active = self.close_small(1u128 << self.start);
            for c in name.chars() {
                let mut next = 0;
                while active != 0 {
                    let i = active.trailing_zeros() as usize; active &= !(1u128 << i);
                    if let State::Char(atom, to) = &self.states[i] { if atom.matches(c) { next |= 1u128 << to; } }
                }
                if next == 0 { return false; } active = self.close_small(next);
            }
            active & (1u128 << (self.states.len() - 1)) != 0
        } else {
            let mut seen = vec![0; self.states.len()]; let mut pending = vec![self.start]; let mut active = Vec::new();
            self.close_large(&mut pending, &mut active, &mut seen, 1);
            for (i, c) in name.chars().enumerate() {
                for &s in &active { if let State::Char(atom, to) = &self.states[s] { if atom.matches(c) { pending.push(*to); } } }
                if pending.is_empty() { return false; }
                self.close_large(&mut pending, &mut active, &mut seen, i + 2);
            }
            active.contains(&(self.states.len() - 1))
        }
    }
}

/// Compiled once per expanded member; either whole-name interpretation may assign the class.
#[derive(Clone, Debug)]
pub struct Matcher { wildcard: Machine, regex: Option<Machine>, pub regex_unsupported: bool }
impl Matcher {
    pub fn new(pattern: &str) -> Self {
        let mut arena = Vec::new(); let mut seq = Vec::new();
        for c in pattern.chars() {
            let id = match c {
                '*' => { let child = node(&mut arena, Expr::Atom(Atom::Any)); node(&mut arena, Expr::Repeat { child, min: 0, max: None }) }
                '?' => node(&mut arena, Expr::Atom(Atom::Any)),
                _ => node(&mut arena, Expr::Atom(Atom::Literal(c))),
            }; seq.push(id);
        }
        let root = sequence(&mut arena, seq); let mut wildcard = compile(&arena, root);
        // These common automata have a direct string-operation equivalent, avoiding state work.
        if !pattern.contains('?') {
            let inner = pattern.trim_matches('*');
            if !inner.contains('*') {
                wildcard.fast = Some(match (pattern.starts_with('*'), pattern.ends_with('*')) {
                    (true, true) => Fast::Contains(inner.into()), (true, false) => Fast::Suffix(inner.into()),
                    (false, true) => Fast::Prefix(inner.into()), (false, false) => Fast::Exact(inner.into()),
                });
            }
        }
        let compiled = regex(pattern); let regex_unsupported = matches!(compiled, Err(Error::Unsupported));
        let regex = compiled.ok().map(|(arena, root)| compile(&arena, root));
        Self { wildcard, regex, regex_unsupported }
    }
    pub fn is_match(&self, name: &str) -> bool { self.wildcard.matches(name) || self.regex.as_ref().is_some_and(|m| m.matches(name)) }
}
