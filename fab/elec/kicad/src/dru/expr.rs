use super::{ConstraintKind, MAX_DEPTH};
use crate::report::Refusal;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unit { Scalar, Length, Time, Angle }

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
/// `AB` names the pair itself and only calls functions (`AB.isCoupledDiffPair()`).
pub enum Item { A, B, AB }

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum UnaryOp { Not, Plus, Minus }

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BinaryOp { And, Or, Equal, NotEqual, Less, LessEqual, Greater, GreaterEqual, Add, Subtract, Multiply, Divide }

impl BinaryOp {
    fn precedence(self) -> u8 {
        match self {
            Self::And => 1, Self::Or => 2,
            Self::Equal | Self::NotEqual | Self::Less | Self::LessEqual | Self::Greater | Self::GreaterEqual => 3,
            Self::Add | Self::Subtract => 5, Self::Multiply | Self::Divide => 6,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Converted to nanometres, femtoseconds, degrees, or a scalar, as marked by `unit`.
    Number { value: f64, unit: Unit },
    String { value: String, wildcard: bool },
    Null,
    Property { item: Item, name: String },
    Layer,
    Call { item: Item, name: String, args: Vec<Expr> },
    Unary { op: UnaryOp, value: Box<Expr> },
    Binary { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
}

impl Expr {
    /// Whether any operand is owned by one of `items`.
    pub(super) fn names(&self, items: &[Item]) -> bool {
        match self {
            Self::Property { item, .. } => items.contains(item),
            Self::Call { item, args, .. } => items.contains(item) || args.iter().any(|a| a.names(items)),
            Self::Unary { value, .. } => value.names(items),
            Self::Binary { left, right, .. } => left.names(items) || right.names(items),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64, Unit), String(String), Ident(String), Op(BinaryOp), Not,
    Dot, Comma, Open, Close, Semi, End,
}

/// Locations follow decoded bytes back to their spelling in the outer file.
pub(super) struct Input {
    pub text: String,
    pub locations: Vec<(u32, u32)>,
    pub end: (u32, u32),
}

impl Input {
    fn at(&self, pos: usize) -> (u32, u32) { self.locations.get(pos).copied().unwrap_or(self.end) }
    fn error(&self, pos: usize, message: impl Into<String>) -> Refusal { Refusal::new(self.at(pos), message) }
}

struct Parser<'a> {
    input: &'a Input,
    pos: usize,
    peeked: Option<(Token, usize)>,
    numeric: bool,
}

impl Parser<'_> {
    fn peek(&mut self) -> Result<(Token, usize), Refusal> {
        if let Some(t) = &self.peeked { return Ok(t.clone()); }
        let t = self.scan()?;
        self.peeked = Some(t.clone());
        Ok(t)
    }
    fn next(&mut self) -> Result<(Token, usize), Refusal> {
        if let Some(t) = self.peeked.take() { Ok(t) } else { self.scan() }
    }
    fn scan(&mut self) -> Result<(Token, usize), Refusal> {
        let bytes = self.input.text.as_bytes();
        while bytes.get(self.pos) == Some(&b' ') { self.pos += 1; }
        let start = self.pos;
        let Some(&c) = bytes.get(start) else { return Ok((Token::End, start)); };
        self.pos += 1;
        let token = match c {
            b'0'..=b'9' | b'.' if c != b'.' || bytes.get(self.pos).is_some_and(u8::is_ascii_digit) => {
                // One decimal separator, a point or a comma (`0,2mm` is 0.2 mm).
                let mut dot = c == b'.';
                while let Some(&c) = bytes.get(self.pos) {
                    if c.is_ascii_digit() { self.pos += 1; }
                    else if (c == b'.' || c == b',') && !dot { dot = true; self.pos += 1; }
                    else { break; }
                }
                let value = self.input.text[start..self.pos].replace(',', ".").parse::<f64>()
                    .map_err(|_| self.input.error(start, "invalid number"))?;
                // The unit is its own word and may follow after spaces (`90 deg`); letters right
                // after the digits that name no unit are an error.
                let number_end = self.pos;
                let mut unit_start = self.pos;
                while bytes.get(unit_start) == Some(&b' ') { unit_start += 1; }
                let mut unit_end = unit_start;
                while bytes.get(unit_end).is_some_and(u8::is_ascii_alphanumeric) { unit_end += 1; }
                let word = &self.input.text[unit_start..unit_end];
                let named = match word {
                    "mm" => Some((Unit::Length, 1_000_000.0)), "mil" => Some((Unit::Length, 25_400.0)),
                    "in" => Some((Unit::Length, 25_400_000.0)), "deg" => Some((Unit::Angle, 1.0)),
                    "fs" => Some((Unit::Time, 1.0)), "ps" => Some((Unit::Time, 1000.0)), _ => None,
                };
                let (unit, scale) = match named {
                    Some(named) => { self.pos = unit_end; named }
                    None if unit_start == number_end && !word.is_empty() => {
                        return Err(self.input.error(unit_start, "unknown numeric unit"));
                    }
                    None => { self.pos = number_end; (Unit::Scalar, 1.0) }
                };
                if !(value * scale).is_finite() { return Err(self.input.error(start, "number out of range")); }
                Token::Number(value * scale, unit)
            }
            b'\'' => {
                let mut value = Vec::new();
                loop {
                    match bytes.get(self.pos) {
                        // An unclosed string runs to the end of the condition text.
                        None => break,
                        Some(b'\'') => { self.pos += 1; break; }
                        Some(b'\\') if bytes.get(self.pos + 1) == Some(&b'\'') => {
                            value.push(b'\''); self.pos += 2;
                        }
                        Some(&c) => { value.push(c); self.pos += 1; }
                    }
                }
                Token::String(String::from_utf8(value).map_err(|_| self.input.error(start, "invalid string encoding"))?)
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                while bytes.get(self.pos).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') { self.pos += 1; }
                Token::Ident(self.input.text[start..self.pos].into())
            }
            b'!' if bytes.get(self.pos) != Some(&b'=') => Token::Not,
            b'=' | b'!' | b'<' | b'>' => {
                let equal = bytes.get(self.pos) == Some(&b'=');
                if equal { self.pos += 1; }
                Token::Op(match (c, equal) {
                    (b'=', true) => BinaryOp::Equal, (b'!', true) => BinaryOp::NotEqual,
                    (b'<', true) => BinaryOp::LessEqual, (b'<', false) => BinaryOp::Less,
                    (b'>', true) => BinaryOp::GreaterEqual, (b'>', false) => BinaryOp::Greater,
                    _ => return Err(self.input.error(start, "invalid comparison token")),
                })
            }
            b'&' | b'|' if bytes.get(self.pos) == Some(&c) => {
                self.pos += 1; Token::Op(if c == b'&' { BinaryOp::And } else { BinaryOp::Or })
            }
            b'+' => Token::Op(BinaryOp::Add), b'-' => Token::Op(BinaryOp::Subtract),
            b'*' => Token::Op(BinaryOp::Multiply), b'/' => Token::Op(BinaryOp::Divide),
            b'.' => Token::Dot, b',' => Token::Comma, b'(' => Token::Open, b')' => Token::Close,
            b';' => Token::Semi,
            _ => return Err(self.input.error(start, "bad expression token")),
        };
        Ok((token, start))
    }

    fn expression(&mut self, min: u8, depth: usize) -> Result<(Expr, usize), Refusal> {
        let (token, at) = self.next()?;
        if depth >= MAX_DEPTH { return Err(self.input.error(at, "expression nesting is too deep")); }
        let (mut left, mut height) = match token {
            Token::Number(value, unit) => (Expr::Number { value, unit }, 1),
            Token::String(value) => {
                let wildcard = value.contains(['*', '?']);
                (Expr::String { value, wildcard }, 1)
            }
            Token::Ident(name) if name == "null" => (Expr::Null, 1),
            Token::Ident(name) => self.member(name, at, depth)?,
            Token::Not => {
                let (value, h) = self.expression(4, depth + 1)?;
                (Expr::Unary { op: UnaryOp::Not, value: Box::new(value) }, h + 1)
            }
            Token::Op(op @ (BinaryOp::Add | BinaryOp::Subtract)) if self.numeric => {
                let (value, h) = self.expression(7, depth + 1)?;
                (Expr::Unary { op: if op == BinaryOp::Add { UnaryOp::Plus } else { UnaryOp::Minus }, value: Box::new(value) }, h + 1)
            }
            Token::Open => {
                let value = self.expression(1, depth + 1)?;
                self.expect(Token::Close, "expected ')' in expression")?;
                value
            }
            _ => return Err(self.input.error(at, "expected expression operand")),
        };
        let mut comparison = false;
        loop {
            let (token, pos) = self.peek()?;
            let Token::Op(op) = token else { break; };
            let precedence = op.precedence();
            if precedence < min { break; }
            if precedence == 3 && comparison { return Err(self.input.error(pos, "comparisons cannot be chained")); }
            self.next()?;
            let (right, h) = self.expression(precedence + 1, depth + 1)?;
            height = height.max(h) + 1;
            if height > MAX_DEPTH { return Err(self.input.error(pos, "expression tree is too deep")); }
            left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
            comparison |= precedence == 3;
        }
        if height > MAX_DEPTH { return Err(self.input.error(at, "expression tree is too deep")); }
        Ok((left, height))
    }

    fn expect(&mut self, expected: Token, message: &str) -> Result<(), Refusal> {
        let (token, at) = self.next()?;
        if token == expected { Ok(()) } else { Err(self.input.error(at, message)) }
    }

    fn member(&mut self, owner: String, at: usize, depth: usize) -> Result<(Expr, usize), Refusal> {
        if !matches!(owner.as_str(), "A" | "B" | "AB" | "L") { return Err(self.input.error(at, "expected A, B, AB, L.Layer or null")); }
        self.expect(Token::Dot, "expected '.' after item")?;
        let (name, name_at) = self.next()?;
        let Token::Ident(name) = name else { return Err(self.input.error(name_at, "expected property or function name")); };
        if owner == "L" {
            return if name.eq_ignore_ascii_case("Layer") { Ok((Expr::Layer, 1)) }
                else { Err(self.input.error(name_at, "only L.Layer is defined")) };
        }
        let item = match owner.as_str() { "A" => Item::A, "B" => Item::B, _ => Item::AB };
        if self.peek()?.0 != Token::Open {
            if item == Item::AB { return Err(self.input.error(name_at, "AB takes functions only")); }
            return Ok((Expr::Property { item, name: name.replace('_', " ") }, 1));
        }
        let name = function_name(&name).ok_or_else(|| self.input.error(name_at, format!("unknown function {name}")))?;
        self.next()?;
        let mut args = Vec::new();
        let mut height = 1;
        if self.peek()?.0 != Token::Close {
            loop {
                let (arg, h) = self.expression(1, depth + 1)?;
                height = height.max(h + 1);
                args.push(arg);
                if self.peek()?.0 != Token::Comma { break; }
                self.next()?;
            }
        }
        self.expect(Token::Close, "expected ')' after function arguments")?;
        Ok((Expr::Call { item, name: name.into(), args }, height))
    }
}

fn function_name(name: &str) -> Option<&'static str> {
    let canonical = match name.to_ascii_lowercase().as_str() {
        "insidearea" => "intersectsKeepout",
        "insidecourtyard" => "intersectsCourtyard",
        "insidefrontcourtyard" => "intersectsFrontCourtyard",
        "insidebackcourtyard" => "intersectsBackCourtyard",
        "memberof" => "memberOfGroup",
        _ => return FUNCTIONS.iter().copied().find(|s| s.eq_ignore_ascii_case(name)),
    };
    Some(canonical)
}

const FUNCTIONS: &[&str] = &[
    "hasNetclass", "hasExactNetclass", "intersectsArea", "intersectsKeepout", "enclosedByArea",
    "intersectsCourtyard", "intersectsFrontCourtyard", "intersectsBackCourtyard", "memberOfFootprint",
    "memberOfGroup", "fromTo", "inDiffPair", "isCoupledDiffPair", "existsOnLayer", "isPlated",
    "hasComponentClass", "isMicroVia", "isBlindVia", "isBuriedVia", "isBlindBuriedVia", "isStackedVia",
    "memberOfSheet", "memberOfSheetOrChildren", "inNetChain", "hasNetChain", "inNetChainClass",
    "getField", "customProperty", "hasCustomProperty",
];

pub(super) fn parse(input: &Input, numeric: bool) -> Result<Expr, Refusal> {
    let mut parser = Parser { input, pos: 0, peeked: None, numeric };
    let (expr, _) = parser.expression(1, 0)?;
    parser.expect(Token::End, "unexpected token after expression")?;
    Ok(expr)
}

pub(super) fn value(input: &Input, kind: ConstraintKind) -> Result<(i64, Unit), Refusal> {
    let expr = parse(input, true)?;
    let fail = |message| input.error(0, message);
    let mut units = 0u8;
    fn eval(expr: &Expr, units: &mut u8) -> Option<f64> {
        let v = match expr {
            Expr::Number { value, unit } => {
                *units |= match unit { Unit::Scalar => 0, Unit::Length => 1, Unit::Time => 2, Unit::Angle => 4 };
                *value
            }
            Expr::Unary { op, value } => match op {
                UnaryOp::Plus => eval(value, units)?,
                UnaryOp::Minus => -eval(value, units)?,
                UnaryOp::Not => return None,
            },
            Expr::Binary { op, left, right } => {
                let a = eval(left, units)?;
                let b = eval(right, units)?;
                match op {
                    BinaryOp::Add => a + b, BinaryOp::Subtract => a - b,
                    BinaryOp::Multiply => a * b, BinaryOp::Divide if b != 0.0 => a / b,
                    _ => return None,
                }
            }
            _ => return None,
        };
        v.is_finite().then_some(v)
    }
    let mut number = eval(&expr, &mut units).ok_or_else(|| fail("invalid or non-finite numeric expression"))?;
    if units.count_ones() > 1 { return Err(fail("mixed units in value")); }
    if (kind.count() || kind.ratio()) && units != 0 { return Err(fail("counts and ratios cannot have units")); }
    if units == 2 && !kind.time_allowed() { return Err(fail("time units are not allowed for this constraint")); }
    if (units == 4 && kind != ConstraintKind::TrackAngle) || (kind == ConstraintKind::TrackAngle && units != 0 && units != 4) {
        return Err(fail("wrong unit for angle constraint"));
    }
    let unit = match units { 1 => Unit::Length, 2 => Unit::Time, 4 => Unit::Angle, _ => Unit::Scalar };
    let unit = if unit == Unit::Scalar && !kind.count() && !kind.ratio() && kind != ConstraintKind::TrackAngle {
        return Err(fail("missing units"));
    } else if kind == ConstraintKind::TrackAngle { Unit::Angle } else { unit };
    if kind.ratio() { number *= 1000.0; }
    let rounded = number.round();
    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= -(i64::MIN as f64) {
        return Err(fail("numeric value exceeds integer range"));
    }
    Ok((rounded as i64, unit))
}
