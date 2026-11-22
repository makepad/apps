use super::*;
use crate::report::{GapKind, Refusal, SourceFile};
use crate::sexpr::{Kind, Lexer, Token};
use fab_elec_core::{LayerId, LayerKind};

/// Parse a complete companion file. The first error discards every rule in the file.
pub fn parse_kicad_dru(bytes: &[u8]) -> Result<RuleFile, Refusal> {
    if let Err(error) = std::str::from_utf8(bytes) {
        return Err(Refusal::new(location(bytes, error.valid_up_to()), "invalid rule file encoding"));
    }
    let mut parser = Parser { lex: Lexer::new(bytes) };
    parser.expect(Kind::Open, "expected version declaration")?;
    let (version, token) = parser.atom()?;
    if version != "version" { return Err(Refusal::new(token.location(), "first item must be version")); }
    let (version, token) = parser.atom()?;
    let version = version.parse::<i64>().map_err(|_| Refusal::new(token.location(), "expected integer version"))?;
    parser.expect(Kind::Close, "expected ')' after version")?;
    let mut rules = Vec::new();
    while parser.lex.peek()?.kind != Kind::Eof {
        let open = parser.expect(Kind::Open, "expected rule declaration")?;
        let (head, token) = parser.atom()?;
        if head != "rule" { return Err(Refusal::new(token.location(), "expected rule declaration")); }
        rules.push(parser.rule(open)?);
    }
    let mut report = ImportReport::default();
    report.current = SourceFile::Rules;
    record_variables(bytes, &mut report);
    report.read.insert("rules.custom".into(), rules.len() as u64);
    Ok(RuleFile { version, too_recent: version > 20200610, rules, report })
}

pub(crate) fn record_variables(bytes: &[u8], report: &mut ImportReport) {
    if let Some(offset) = bytes.windows(2).position(|s| s == b"${") {
        report.gap("rules.text_variable", GapKind::Skipped, location(bytes, offset));
    }
}

fn location(bytes: &[u8], offset: usize) -> (u32, u32) {
    let mut at = (1u32, 1u32);
    for &c in bytes.iter().take(offset) {
        if c == b'\n' { at.0 = at.0.saturating_add(1); at.1 = 1; }
        else { at.1 = at.1.saturating_add(1); }
    }
    at
}

struct Parser<'a> { lex: Lexer<'a> }

impl Parser<'_> {
    fn expect(&mut self, kind: Kind, message: &str) -> Result<Token, Refusal> {
        let token = self.lex.next_token()?;
        if token.kind == kind { Ok(token) } else { Err(Refusal::new(token.location(), message)) }
    }
    fn atom(&mut self) -> Result<(String, Token), Refusal> {
        let token = self.lex.next_token()?;
        if !matches!(token.kind, Kind::Symbol | Kind::String | Kind::Number) {
            return Err(Refusal::new(token.location(), "expected symbol or string"));
        }
        Ok((self.lex.text(token)?, token))
    }
    fn name(&mut self) -> Result<(String, Token), Refusal> {
        let (text, token) = self.atom()?;
        if token.kind == Kind::Number { return Err(Refusal::new(token.location(), "expected symbol or quoted name")); }
        Ok((text, token))
    }

    fn rule(&mut self, open: Token) -> Result<Rule, Refusal> {
        let (name, _) = self.name()?;
        let mut rule = Rule {
            name, location: open.location(), offset: open.start,
            layers: None, layer_pattern: None, severity: None, condition: None,
            condition_text: None, constraints: Vec::new(),
        };
        let mut condition_at = open.location();
        while self.lex.peek()?.kind != Kind::Close {
            self.expect(Kind::Open, "expected rule clause")?;
            let (head, token) = self.atom()?;
            match head.as_str() {
                "constraint" => {
                    let (kind, at) = self.atom()?;
                    let kind = ConstraintKind::parse(&kind).ok_or_else(|| Refusal::new(at.location(), format!("unknown constraint {kind}")))?;
                    if rule.constraints.iter().any(|c| c.kind == kind) {
                        return Err(Refusal::new(at.location(), format!("repeated constraint {}", kind.name())));
                    }
                    rule.constraints.push(self.constraint(kind, at.location())?);
                }
                "condition" => {
                    let input = self.quoted()?;
                    condition_at = input.locations.first().copied().unwrap_or(input.end);
                    rule.condition = Some(expr::parse(&input, false)?);
                    rule.condition_text = Some(input.text);
                }
                "layer" => {
                    if rule.layers.is_some() { return Err(Refusal::new(token.location(), "second layer clause")); }
                    let (pattern, token) = self.name()?;
                    rule.layers = Some(layers(&pattern).ok_or_else(|| Refusal::new(token.location(), format!("unknown layer {pattern}")))?);
                    rule.layer_pattern = Some(pattern);
                }
                "severity" => {
                    let (value, token) = self.atom()?;
                    rule.severity = Some(match value.as_str() {
                        "ignore" => RuleSeverity::Ignore, "warning" => RuleSeverity::Warning,
                        "error" => RuleSeverity::Error, "exclusion" => RuleSeverity::Exclusion,
                        _ => return Err(Refusal::new(token.location(), format!("unknown severity {value}"))),
                    });
                }
                _ => return Err(Refusal::new(token.location(), format!("unknown rule clause {head}"))),
            }
            self.expect(Kind::Close, "expected ')' after rule clause")?;
        }
        self.lex.next_token()?;
        // A condition that names the second item (or the pair) needs two items; single-item
        // constraints refuse it, except that the length-like ones accept the pair without B.
        if let Some(condition) = &rule.condition {
            let names_b = condition.names(&[Item::B]);
            if names_b || condition.names(&[Item::AB]) {
                let refused = rule.constraints.iter().filter(|c| names_b || !c.kind.pair_allowed()).find(|c| c.kind.single_item());
                if let Some(constraint) = refused {
                    return Err(Refusal::new(condition_at, format!("B is not allowed with single-item constraint {}", constraint.kind.name())));
                }
            }
        }
        Ok(rule)
    }

    fn constraint(&mut self, kind: ConstraintKind, at: (u32, u32)) -> Result<Constraint, Refusal> {
        let mut out = Constraint {
            kind, location: at, min: None, opt: None, max: None, in_time: false,
            within_diff_pairs: false, disallow: DisallowKinds::default(), zone_connection: None,
            assertion: None, return_path_layer: None, return_path_net: None,
        };
        match kind {
            ConstraintKind::Disallow => {
                while self.lex.peek()?.kind != Kind::Close {
                    let (name, token) = self.atom()?;
                    let kinds = DisallowKinds::parse(&name).ok_or_else(|| Refusal::new(token.location(), format!("unknown disallow kind {name}")))?;
                    out.disallow.0 |= kinds.0;
                }
            }
            ConstraintKind::Assertion => {
                let input = self.quoted()?;
                let assertion = expr::parse(&input, false)?;
                if assertion.names(&[Item::B, Item::AB]) { return Err(Refusal::new(at, "B is not allowed in an assertion")); }
                out.assertion = Some(assertion);
            }
            ConstraintKind::ZoneConnection => {
                let (name, token) = self.atom()?;
                out.zone_connection = Some(match name.as_str() {
                    "solid" => ZoneConnection::Solid, "thermal_reliefs" => ZoneConnection::ThermalReliefs,
                    "none" => ZoneConnection::None,
                    _ => return Err(Refusal::new(token.location(), "unknown zone connection")),
                });
            }
            ConstraintKind::ReturnPath => {
                while self.lex.peek()?.kind != Kind::Close {
                    self.expect(Kind::Open, "expected return-path clause")?;
                    let (head, token) = self.atom()?;
                    if !matches!(head.as_str(), "layer" | "net") { return Err(Refusal::new(token.location(), "unknown return-path clause")); }
                    let (name, token) = self.name()?;
                    if head == "layer" {
                        if layers(&name).is_none() { return Err(Refusal::new(token.location(), format!("unknown layer {name}"))); }
                        out.return_path_layer = Some(name);
                    } else { out.return_path_net = Some(name); }
                    self.expect(Kind::Close, "expected ')' after return-path clause")?;
                }
            }
            _ => {
                let mut units = None;
                while self.lex.peek()?.kind != Kind::Close {
                    self.expect(Kind::Open, "expected value clause")?;
                    let (head, token) = self.atom()?;
                    if head == "within_diff_pairs" && kind == ConstraintKind::Skew {
                        out.within_diff_pairs = true;
                    } else if matches!(head.as_str(), "min" | "opt" | "max") {
                        let input = self.numeric()?;
                        let (value, unit) = expr::value(&input, kind)?;
                        if units.is_some_and(|previous| previous != unit) {
                            return Err(Refusal::new(token.location(), "mixed units across constraint values"));
                        }
                        units = Some(unit);
                        out.in_time = unit == Unit::Time;
                        match head.as_str() { "min" => out.min = Some(value), "opt" => out.opt = Some(value), _ => out.max = Some(value) }
                    } else { return Err(Refusal::new(token.location(), format!("unknown constraint clause {head}"))); }
                    self.expect(Kind::Close, "expected ')' after value clause")?;
                }
            }
        }
        Ok(out)
    }

    fn quoted(&mut self) -> Result<expr::Input, Refusal> {
        let token = self.expect(Kind::String, "expected quoted expression")?;
        self.input(token)
    }

    fn input(&self, token: Token) -> Result<expr::Input, Refusal> {
        let text = self.lex.text(token)?;
        let raw = self.lex.raw(token);
        let base = token.column.saturating_add(u32::from(token.kind == Kind::String));
        let at = |i: usize| (token.line, base.saturating_add(i.min(u32::MAX as usize) as u32));
        let mut locations = Vec::with_capacity(text.len());
        let mut i = 0;
        while i < raw.len() {
            let start = i;
            let mut width = 1;
            if token.kind == Kind::String && raw[i] == b'\\' && i + 1 < raw.len() {
                i += 1;
                match raw[i] {
                    b'x' => { for _ in 0..2 { if raw.get(i + 1).is_some_and(u8::is_ascii_hexdigit) { i += 1; } else { break; } } }
                    b'0'..=b'7' => { for _ in 0..2 { if raw.get(i + 1).is_some_and(|c| matches!(c, b'0'..=b'7')) { i += 1; } else { break; } } }
                    b'"' | b'\\' | b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' => {}
                    _ => width = 2,
                }
            }
            for _ in 0..width { locations.push(at(start)); }
            i += 1;
        }
        Ok(expr::Input { text, locations, end: at(raw.len()) })
    }

    fn numeric(&mut self) -> Result<expr::Input, Refusal> {
        let mut out = expr::Input { text: String::new(), locations: Vec::new(), end: self.lex.peek()?.location() };
        let mut depth = 0;
        loop {
            let token = self.lex.peek()?;
            if token.kind == Kind::Close && depth == 0 { out.end = token.location(); break; }
            self.lex.next_token()?;
            match token.kind {
                Kind::Open => { depth += 1; if depth > MAX_DEPTH { return Err(Refusal::new(token.location(), "numeric nesting is too deep")); } }
                Kind::Close => depth -= 1,
                Kind::Eof => return Err(Refusal::new(token.location(), "end of input inside value")),
                _ => {}
            }
            let input = self.input(token)?;
            out.text.push_str(&input.text);
            out.locations.extend(input.locations);
        }
        if out.text.is_empty() { return Err(Refusal::new(out.end, "empty value")); }
        Ok(out)
    }
}

fn layers(pattern: &str) -> Option<LayerSet> {
    let layers = match pattern {
        "outer" => LayerSet::EMPTY.with(LayerId::F_CU).with(LayerId::B_CU),
        "inner" => LayerSet::ALL_COPPER.without(LayerId::F_CU).without(LayerId::B_CU),
        _ => {
            let mut layers = LayerSet::EMPTY;
            for n in 0..=u8::MAX {
                let layer = LayerId(n);
                if layer.kind() != LayerKind::Unassigned && wildcard_matches(pattern, &layer.name()) { layers = layers.with(layer); }
            }
            layers
        }
    };
    (!layers.is_empty()).then_some(layers)
}
