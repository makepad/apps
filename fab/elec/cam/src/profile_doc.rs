//! Board-house capabilities read from literal statements in a profile document.

use crate::{Capabilities, PROFILES};
use fab_elec_core::{Diag, DocId, Nm, Sel};
use makepad_script::{
    syntax::{self, Kind, Node},
    tokenizer::{lex_with, ScriptTokenTag},
    units::UnitTable,
};
use std::{collections::{BTreeMap, HashSet}, ops::Range, sync::{Arc, LazyLock}};

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileDef {
    pub name: String,
    pub tier: Option<String>,
    /// The built-in file set used to export this profile.
    pub format: String,
    pub caps: Arc<Capabilities>,
    pub span: Range<u32>,
}

/// Resolve the spelling used in an action or export to the factory's document name.
pub fn lookup<'a>(profiles: &'a [ProfileDef], name: &str) -> Result<&'a ProfileDef, String> {
    profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| {
        let names = profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ");
        format!("no factory profile {name}; available profiles are {names}")
    })
}

pub fn builtins() -> &'static [ProfileDef] {
    static DEFINITIONS: LazyLock<Vec<ProfileDef>> = LazyLock::new(|| {
        let (defs, errors) = read(DocId(0), include_str!("../resources/factories.export.splash"));
        assert!(errors.is_empty(), "invalid built-in factory document: {errors:?}");
        defs
    });
    &DEFINITIONS
}

const REQUIRED: &[&str] = &[
    "track", "clearance", "drill", "annular", "layers", "via_in_pad",
    "board_min", "board_max", "thickness", "thickness_tol_percent",
];

type Error = (Range<usize>, String);

/// Keep each well-formed profile. All selections are byte ranges in the original document.
pub fn read(doc: DocId, text: &str) -> (Vec<ProfileDef>, Vec<Diag>) {
    let units = UnitTable::standard().with("um", 1.0, 1e6).with("nm", 1.0, 1e9)
        .with("mil", 254.0, 1e7).with("inch", 254.0, 1e4)
        // Not a unit a profile takes: only so "5%" reads as one value and its error names it.
        .with("%", 1.0, 100.0);
    let mut profiles = Vec::new();
    let mut errors = Vec::new();
    let mut names = HashSet::new();
    let mut accept = |start: usize, source: &str, parsed: Result<syntax::Syntax, syntax::SyntaxError>| {
        let mut report = |(span, message): Error| {
            errors.push(Diag::error(message, vec![Sel::Source {
                doc, start: (start + span.start) as u32, end: (start + span.end) as u32,
            }]));
        };
        match parsed {
            Ok(tree) => {
                for n in tree.statements() {
                    match statement(n, source) {
                        Ok(mut p) if names.insert(p.name.to_ascii_lowercase()) => {
                            p.span = (start + n.span.start) as u32..(start + n.span.end) as u32;
                            profiles.push(p);
                        }
                        Ok(p) => report((n.span.clone(), format!("factory profile {:?} is already defined", p.name))),
                        Err(error) => report(error),
                    }
                }
            }
            Err(e) => {
                let end = e.at + source[e.at..].chars().next().map_or(0, char::len_utf8);
                report((e.at..end, format!("syntax: {}", e.message)));
            }
        }
    };
    match syntax::parse_with(text, &units) {
        Ok(tree) => accept(0, text, Ok(tree)),
        Err(_) => {
            // A profile can span lines. Recover at statement boundaries using Splash's tokens
            // so brackets and profile calls inside comments or strings never start a statement.
            let mut start = 0;
            let mut depth = 0usize;
            // Tokens are read through their accessors: `LexedToken` is a provided type whose
            // fields only rustc lays out, so JIT-compiled code never reaches them directly.
            for t in lex_with(text, &units) {
                let word = &text[t.span()];
                if t.preceded_by_newline() && (depth == 0 || word == "profile") {
                    let part = &text[start..t.start()];
                    accept(start, part, syntax::parse_with(part, &units));
                    start = t.start();
                    depth = 0;
                }
                match t.token().kind() {
                    ScriptTokenTag::OpenCurly | ScriptTokenTag::OpenRound | ScriptTokenTag::OpenSquare => depth += 1,
                    ScriptTokenTag::CloseCurly | ScriptTokenTag::CloseRound | ScriptTokenTag::CloseSquare => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            accept(start, &text[start..], syntax::parse_with(&text[start..], &units));
        }
    }
    (profiles, errors)
}

fn statement(n: &Node, text: &str) -> Result<ProfileDef, Error> {
    let Some(("profile", [name, options])) = n.as_call() else {
        return Err((n.span.clone(), "expected profile(name, {capabilities})".into()));
    };
    let name = name.string().map(str::trim).filter(|s| !s.is_empty())
        .ok_or_else(|| (name.span.clone(), "profile needs a name in quotes".into()))?;
    let Kind::Object { ty: None, fields, .. } = &options.kind else {
        return Err((options.span.clone(), "profile capabilities must be an object".into()));
    };
    let mut values = BTreeMap::new();
    for f in fields {
        let key = f.name.as_deref().unwrap_or("");
        if f.op != ":" || !(REQUIRED.contains(&key) || matches!(key, "format" | "tier")) {
            return Err((f.span.clone(), format!("unknown profile field or assignment: {}", &text[f.span.clone()])));
        }
        if values.insert(key, &f.value).is_some() {
            return Err((f.span.clone(), format!("profile gives {key} twice")));
        }
    }
    for key in REQUIRED {
        if !values.contains_key(key) {
            return Err((options.span.clone(), format!("profile is missing {key}")));
        }
    }
    let string = |key: &str| values.get(key).map(|n| n.string().ok_or_else(|| (n.span.clone(), format!("{key} needs text")))).transpose();
    let format = match string("format")? {
        Some(s) => PROFILES.iter().find(|format| s.eq_ignore_ascii_case(format))
            .copied().ok_or_else(|| (values["format"].span.clone(), format!("unknown format {s:?}; choose {}", PROFILES.join(" or "))))?,
        None => PROFILES[0],
    };
    let length = |n: &Node| {
        let v = literal(n, text, &["inch", "mil", "mm", "um", "nm", "cm", "m"], "a length")?;
        let nm = (v * 1e9).round();
        if nm > f64::from(Nm::MAX) { return Err((n.span.clone(), "length exceeds the board coordinate range".into())); }
        Ok(nm as Nm)
    };
    let size = |n: &Node| {
        let [x, y] = array(n)? else { return Err((n.span.clone(), "board size needs two lengths".into())) };
        Ok((length(x)?, length(y)?))
    };
    let board_min = size(values["board_min"])?;
    let board_max = size(values["board_max"])?;
    if board_min.0 > board_max.0 || board_min.1 > board_max.1 {
        return Err((values["board_min"].span.clone(), "board_min exceeds board_max".into()));
    }
    let layers = array(values["layers"])?.iter().map(|n| {
        let Kind::Number(v) = n.kind else { return Err((n.span.clone(), "layers needs whole layer counts".into())) };
        if !(1.0..=f64::from(u8::MAX)).contains(&v) || v.fract() != 0.0 || n.text(text).parse::<f64>().is_err() {
            return Err((n.span.clone(), "layers needs positive whole layer counts in range".into()));
        }
        Ok(v as u8)
    }).collect::<Result<_, _>>()?;
    let via_in_pad = match &values["via_in_pad"].kind {
        Kind::Ident(word) if word == "true" || word == "false" => word == "true",
        _ => return Err((values["via_in_pad"].span.clone(), "via_in_pad needs true or false".into())),
    };
    // A plain number: standard Splash has no percent unit, and a profile document is standard Splash.
    let tol = values["thickness_tol_percent"];
    let percent = match tol.kind {
        Kind::Number(v) if v.is_finite() && v >= 0.0 && tol.text(text).parse::<f64>().is_ok() => v,
        _ => return Err((tol.span.clone(), "thickness_tol_percent needs a plain number of percent, such as 5".into())),
    };
    let ppm = (percent * 1e4).round();
    if ppm > f64::from(u32::MAX) { return Err((tol.span.clone(), "thickness_tol_percent exceeds the tolerance range".into())); }
    Ok(ProfileDef {
        name: name.into(), tier: string("tier")?.map(str::to_owned), format: format.into(),
        span: n.span.start as u32..n.span.end as u32,
        caps: Arc::new(Capabilities {
            min_track: length(values["track"])?, min_clearance: length(values["clearance"])?,
            min_drill: length(values["drill"])?, min_annular: length(values["annular"])?,
            via_in_pad, layers, board_min, board_max,
            thickness: array(values["thickness"])?.iter().map(length).collect::<Result<_, _>>()?,
            thickness_tol_ppm: ppm as u32,
        }),
    })
}

fn array(n: &Node) -> Result<&[Node], Error> {
    match &n.kind {
        Kind::Array { items, .. } => Ok(items),
        _ => Err((n.span.clone(), "expected a list in brackets".into())),
    }
}

fn literal(n: &Node, text: &str, suffixes: &[&str], want: &str) -> Result<f64, Error> {
    let (value, number) = match &n.kind {
        Kind::Number(v) => (*v, n),
        Kind::Unary { op, operand } if op == "-" => match operand.kind {
            Kind::Number(v) => (-v, operand.as_ref()),
            _ => return Err((n.span.clone(), format!("expected {want}"))),
        },
        _ => return Err((n.span.clone(), format!("expected {want}"))),
    };
    if !value.is_finite() || !suffixes.iter().any(|s| number.text(text).ends_with(s)) {
        return Err((n.span.clone(), format!("expected {want}")));
    }
    if value < 0.0 { return Err((n.span.clone(), format!("expected {want} at least zero"))); }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"profile("pilot", {
        layers: [2 4] track: 6mil clearance: 0.2mm drill: 300um annular: 0.1mm
        via_in_pad: true board_min: [5mm 5mm] board_max: [10cm 12cm]
        thickness: [1mm 1.6mm] thickness_tol_percent: 10
    })"#;

    #[test]
    fn literal_capabilities_and_format() {
        let (profiles, errors) = read(DocId(7), GOOD);
        assert!(errors.is_empty(), "{errors:?}");
        let [p] = profiles.as_slice() else { panic!("expected one profile") };
        assert_eq!(p.format, PROFILES[0]);
        assert_eq!(p.tier, None);
        assert_eq!(p.caps.min_track, 152_400);
        assert_eq!(p.caps.board_max, (100_000_000, 120_000_000));
        assert_eq!(p.caps.thickness_tol_ppm, 100_000);
        assert_eq!(p.caps.layers, [2, 4]);
        assert!(p.caps.via_in_pad);
        // The shipped document is standard Splash: it reads without the profile reader's units
        // and prints back byte for byte, as every tracked .splash file must.
        let shipped = include_str!("../resources/factories.export.splash");
        let plain = syntax::parse(shipped).expect("the shipped profiles parse as standard Splash");
        assert!(plain.diagnostics().is_empty(), "shipped profiles: {:?}", plain.diagnostics().first());
        assert_eq!(plain.print(), shipped, "the shipped profiles print back as written");
        assert_eq!(builtins().iter().find(|p| p.name == "standard 2-layer").map(|p| p.caps.thickness_tol_ppm), Some(50_000));
    }

    #[test]
    fn errors_are_located_and_other_statements_survive() {
        for (from, to, at) in [
            ("6mil", "-1mm", "-1mm"), ("6mil", "1ms", "1ms"), ("6mil", "get()", "get()"),
            ("[2 4]", "[2mm]", "2mm"), ("true", "1", "1"),
            ("[5mm 5mm]", "[20cm 5mm]", "[20cm 5mm]"), ("[1mm 1.6mm]", "false", "false"),
            ("percent: 10", "percent: 10mm", "10mm"), ("percent: 10", "percent: 10%", "10%"), ("track: 6mil", "track: 1mm track: 2mm", "track: 2mm"),
            ("layers:", "format: \"other\" layers:", "\"other\""),
            ("layers:", "tier: false layers:", "false"),
            ("layers:", "other: true layers:", "other: true"),
            ("clearance: 0.2mm", "", "{"),
            ("profile(", "unknown(", "unknown("), ("[2 4]", "[2 4}", "["),
        ] {
            let broken = GOOD.replace(from, to);
            let text = format!("{broken}\n{GOOD}");
            let (profiles, errors) = read(DocId(7), &text);
            assert_eq!(profiles.len(), 1, "{to}: {errors:?}");
            let [error] = errors.as_slice() else { panic!("{to}: {errors:?}") };
            let [Sel::Source { doc: DocId(7), start, end }] = error.sel.as_slice() else { panic!("{error:?}") };
            assert!((*end as usize) <= broken.len());
            assert!(text[*start as usize..*end as usize].starts_with(at), "{to}: {error:?}");
        }
        let (profiles, errors) = read(DocId(7), &format!("{GOOD}\n{GOOD}"));
        assert_eq!(profiles.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0].sel[..], [Sel::Source { start, .. }] if start as usize == GOOD.len() + 1));
    }
}
