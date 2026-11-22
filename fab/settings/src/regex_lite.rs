//! The small regular-expression subset profile templates use with `=~`:
//! literals, `.`, classes `[...]` (ranges, negation), escapes (`\d \w \s`
//! and escaped literals), groups with `|`, and the `* + ?` quantifiers.
//! Matching is anchored at both ends (a full match), by backtracking.

#[derive(Debug, Clone)]
enum Atom {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Group(Vec<Vec<Piece>>),
}

#[derive(Debug, Clone)]
struct Piece {
    atom: Atom,
    min: usize,
    max: usize,
}

struct P<'a> {
    c: Vec<char>,
    i: usize,
    _s: &'a str,
}

fn parse_alt(p: &mut P) -> Result<Vec<Vec<Piece>>, String> {
    let mut alts = vec![Vec::new()];
    while p.i < p.c.len() {
        let ch = p.c[p.i];
        match ch {
            '|' => {
                p.i += 1;
                alts.push(Vec::new());
            }
            ')' => break,
            _ => {
                let atom = parse_atom(p)?;
                let (min, max) = match p.c.get(p.i) {
                    Some('*') => {
                        p.i += 1;
                        (0, usize::MAX)
                    }
                    Some('+') => {
                        p.i += 1;
                        (1, usize::MAX)
                    }
                    Some('?') => {
                        p.i += 1;
                        (0, 1)
                    }
                    _ => (1, 1),
                };
                alts.last_mut().unwrap().push(Piece { atom, min, max });
            }
        }
    }
    Ok(alts)
}

fn class_escape(c: char) -> Option<Vec<(char, char)>> {
    Some(match c {
        'd' => vec![('0', '9')],
        'w' => vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
        's' => vec![(' ', ' '), ('\t', '\t'), ('\n', '\n'), ('\r', '\r')],
        _ => return None,
    })
}

fn parse_atom(p: &mut P) -> Result<Atom, String> {
    let ch = p.c[p.i];
    p.i += 1;
    Ok(match ch {
        '.' => Atom::Any,
        '(' => {
            if p.c.get(p.i) == Some(&'?') && p.c.get(p.i + 1) == Some(&':') {
                p.i += 2;
            }
            let alts = parse_alt(p)?;
            if p.c.get(p.i) != Some(&')') {
                return Err("regex: missing ')'".into());
            }
            p.i += 1;
            Atom::Group(alts)
        }
        '[' => {
            let mut neg = false;
            if p.c.get(p.i) == Some(&'^') {
                neg = true;
                p.i += 1;
            }
            let mut ranges = Vec::new();
            let mut first = true;
            loop {
                let Some(&c) = p.c.get(p.i) else { return Err("regex: missing ']'".into()) };
                if c == ']' && !first {
                    p.i += 1;
                    break;
                }
                first = false;
                p.i += 1;
                let lo = if c == '\\' {
                    let e = *p.c.get(p.i).ok_or("regex: trailing backslash")?;
                    p.i += 1;
                    if let Some(cls) = class_escape(e) {
                        ranges.extend(cls);
                        continue;
                    }
                    e
                } else {
                    c
                };
                if p.c.get(p.i) == Some(&'-') && p.c.get(p.i + 1).map_or(false, |c| *c != ']') {
                    let hi = p.c[p.i + 1];
                    p.i += 2;
                    ranges.push((lo, hi));
                } else {
                    ranges.push((lo, lo));
                }
            }
            Atom::Class(ranges, neg)
        }
        '\\' => {
            let e = *p.c.get(p.i).ok_or("regex: trailing backslash")?;
            p.i += 1;
            match class_escape(e) {
                Some(cls) => Atom::Class(cls, false),
                None => Atom::Char(e),
            }
        }
        '*' | '+' | '?' => return Err("regex: nothing to repeat".into()),
        '^' | '$' => Atom::Group(vec![Vec::new()]),
        c => Atom::Char(c),
    })
}

fn atom_matches(a: &Atom, c: char) -> bool {
    match a {
        Atom::Char(x) => *x == c,
        Atom::Any => c != '\n',
        Atom::Class(r, neg) => r.iter().any(|(lo, hi)| c >= *lo && c <= *hi) != *neg,
        Atom::Group(_) => false,
    }
}

/// Match `pieces` against `s[i..]`, then continue with `k(end)`.
fn m(pieces: &[Piece], s: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    let Some((first, rest)) = pieces.split_first() else { return k(i) };
    match &first.atom {
        Atom::Group(alts) => {
            // Repeat the group between min and max times (greedy).
            fn rep(alts: &[Vec<Piece>], count: usize, min: usize, max: usize, rest: &[Piece], s: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
                if count < max {
                    for alt in alts {
                        let mut next = |j: usize| j != i && rep(alts, count + 1, min, max, rest, s, j, k);
                        if m(alt, s, i, &mut next) {
                            return true;
                        }
                    }
                    // An empty-matching group still counts towards min.
                    if count < min {
                        for alt in alts {
                            let mut next = |j: usize| j == i && m(rest, s, j, k);
                            if m(alt, s, i, &mut next) {
                                return true;
                            }
                        }
                    }
                }
                count >= min && m(rest, s, i, k)
            }
            rep(alts, 0, first.min, first.max, rest, s, i, k)
        }
        a => {
            let mut n = 0;
            while n < first.max && i + n < s.len() && atom_matches(a, s[i + n]) {
                n += 1;
            }
            loop {
                if n < first.min {
                    return false;
                }
                if m(rest, s, i + n, k) {
                    return true;
                }
                if n == 0 {
                    return false;
                }
                n -= 1;
            }
        }
    }
}

pub fn full_match(pattern: &str, s: &str) -> Result<bool, String> {
    let mut p = P { c: pattern.chars().collect(), i: 0, _s: pattern };
    let alts = parse_alt(&mut p)?;
    if p.i != p.c.len() {
        return Err("regex: unbalanced ')'".into());
    }
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let top = vec![Piece { atom: Atom::Group(alts), min: 1, max: 1 }];
    let mut done = |j: usize| j == n;
    Ok(m(&top, &chars, 0, &mut done))
}
