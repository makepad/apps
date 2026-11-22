//! Bench edits target written statements and literals; computed values stay code.

use crate::bench::{Stimulus, check_option_number, check_terminals, source_kind, stimulus_unit};
use crate::map::{BenchKey, Origin, SourceMap, SourceRange};
use crate::resolve;
use crate::source::{Doc, Sources, located};
use crate::units::{circuit_units, decimal_text, literal_text, qty_of};
use crate::value_site::{call_at, literal_for};
use fab_elec_circuit::{QtyUnit, Quantity};
use fab_elec_core::{Diag, Sel};
use fab_elec_core::drive::{Drive, Pull};
use fab_elec_design::{ProbeOf, ProbeQuantity, TextEdit, Tip};
use makepad_script::syntax::{
    self, Block, Field, Grouped, Kind, Node, Syntax,
    edit::{At, Replace, add_field_edits, remove_field_edits},
    write::quote,
};
use std::ops::Range;

pub(crate) fn add_probe(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    label: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, bench)?;
    if map
        .bench_site(bench, &BenchKey::Probe(of.clone()))
        .is_some()
    {
        return Ok(Vec::new());
    }
    let pin = if of.quantity == ProbeQuantity::I {
        let (path, pin) = of
            .target
            .rsplit_once('.')
            .unwrap_or(("", of.target.as_str()));
        let mut chars = pin.chars();
        if path.is_empty()
            || !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(Diag::error(
                format!(
                    "{}: pin {pin} has no field name; write this probe by hand",
                    of.target
                ),
                vec![],
            ));
        }
        Some((path, pin))
    } else {
        None
    };
    insert_statement(doc, at, "probe", |b, c| {
        let target = match of.quantity {
            ProbeQuantity::V => match &of.reference {
                Some(reference) => format!(
                    "{b}.v({c}.net({}), {c}.net({}))",
                    quote(&of.target),
                    quote(reference)
                ),
                None => format!("{c}.net({})", quote(&of.target)),
            },
            ProbeQuantity::I => {
                let (path, pin) = pin.expect("a current probe has a field name");
                format!("{b}.i({c}.{path}.{pin})")
            }
            ProbeQuantity::P => format!("{b}.p({c}.{})", of.target),
        };
        format!("{b}.probe({target}, {{label: {}}})", quote(label))
    })
}

fn stimulus_terminals(map: &SourceMap, bench: &str, a: &str, b: Option<&str>) -> Result<(), String> {
    let Some(Stimulus::Circuit(circuit)) = map.bench_stimuli.get(&(bench.into(), BenchKey::Body)) else {
        return Err(format!("no circuit for bench {bench}"));
    };
    let net = |name: &str| {
        circuit.net(name).map(|n| n.id).ok_or_else(|| format!("no net named {name:?}"))
    };
    check_terminals(circuit, net(a)?, b.map(net).transpose()?)
}

fn stimulus_literal(call: &str, key: &str, si: f64) -> Result<String, String> {
    let unit = stimulus_unit(call, key).ok_or_else(|| format!("{call} has no numeric option {key}"))?;
    check_option_number(key, si)?;
    Ok(literal_text(&Quantity { si, unit }))
}

pub(crate) fn add_supply(
    sources: &Sources, map: &SourceMap, bench: &str, net: &str, v: f64,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, bench)?;
    if let Some(site) = map.bench_site(bench, &BenchKey::Supply(net.into())) {
        return Err(located(site.doc, site.span.clone(), format!("{net} already has a supply")));
    }
    let fail = |message| located(doc.id, at.span.clone(), message);
    stimulus_terminals(map, bench, net, None).map_err(fail)?;
    let v = stimulus_literal("supply", "v", v).map_err(fail)?;
    insert_statement(doc, at, "supply", |b, c| {
        format!("{b}.supply({c}.net({}), {{v: {v}}})", quote(net))
    })
}

pub(crate) fn add_source(
    sources: &Sources, map: &SourceMap, bench: &str, kind: &str, a: &str, b: &str, opts: &[(String, f64)],
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, bench)?;
    let fail = |message| located(doc.id, at.span.clone(), message);
    stimulus_terminals(map, bench, a, Some(b)).map_err(fail)?;
    let value = |key: &str| opts.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
    source_kind(
        kind,
        |key| value(key).is_some(),
        |key| Ok(value(key)),
        || Err("step: expected two currents [from to]".into()),
        |_, message| message,
    ).map_err(fail)?;
    let mut fields = Vec::new();
    for (i, (key, si)) in opts.iter().enumerate() {
        if opts[..i].iter().any(|(other, _)| other == key) {
            return Err(fail(format!("{kind}: duplicate option {key}")));
        }
        let literal = stimulus_literal(kind, key, *si).map_err(fail)?;
        fields.push(format!("{key}: {literal}"));
    }
    insert_statement(doc, at, kind, |bench, c| {
        format!("{bench}.{kind}({c}.net({}), {c}.net({}), {{{}}})", quote(a), quote(b), fields.join(" "))
    })
}

pub(crate) fn set_pressed(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    comp: &str,
    pressed: bool,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, at) = mapped(sources, map, bench)?;
    let key = BenchKey::Press(comp.into());
    if pressed == map.bench_site(bench, &key).is_some() {
        return Ok(Vec::new());
    }
    if pressed {
        insert_statement(doc, at, "press", |b, c| format!("{b}.press({c}.{comp})"))
    } else {
        remove_statement(sources, map, bench, &key, comp, "press")
    }
}

fn insert_statement(
    doc: &Doc,
    at: &SourceRange,
    method: &str,
    statement: impl FnOnce(&str, &str) -> String,
) -> Result<Vec<TextEdit>, Diag> {
    let tree = parse(doc)?;
    let InsertSite {
        function, block, close, b, c,
    } = insert_site(&tree, at)?;
    let source = doc.text.as_ref();
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let statement = statement(b, c);
    let callee = format!("{b}.{method}");
    let (pos, text) = if let Some(index) = block
        .stmts
        .iter()
        .rposition(|stmt| stmt.call(&callee).is_some())
    {
        let stmt = &block.stmts[index];
        let limit = block
            .stmts
            .get(index + 1)
            .map_or(close, |next| next.span.start);
        let indent = line_indent(source, stmt.span.start);
        match next_line(&source[stmt.span.end..limit]) {
            Some(delta) => (
                stmt.span.end + delta,
                format!("{indent}{statement}{newline}"),
            ),
            None => (
                limit,
                format!(
                    "{newline}{indent}{statement}{newline}{}",
                    line_indent(source, limit)
                ),
            ),
        }
    } else if let Some(first) = block.stmts.first() {
        let indent = line_indent(source, first.span.start);
        before(
            source,
            first.span.start,
            indent,
            indent,
            &statement,
            newline,
        )
    } else {
        // The function may start on a continuation line of bench(...).
        let mut call_start = function.span.start;
        tree.walk(&mut |_, node| {
            if let Kind::Call { args, .. } = &node.kind {
                if args.get(2).is_some_and(|arg| arg.span == function.span) {
                    call_start = node.span.start;
                }
            }
        });
        let indent = line_indent(source, call_start);
        before(
            source,
            close,
            &format!("{indent}    "),
            indent,
            &statement,
            newline,
        )
    };
    Ok(vec![edit(doc, pos..pos, text)])
}

pub(crate) struct InsertSite<'a> {
    function: &'a Node,
    block: &'a Block,
    close: usize,
    b: &'a str,
    c: &'a str,
}

pub(crate) fn insert_site<'a>(tree: &'a Syntax, at: &SourceRange) -> Result<InsertSite<'a>, Diag> {
    let function = function(tree, at)?;
    let Kind::Fn { params, body, .. } = &function.kind else {
        unreachable!()
    };
    let fail = |message| located(at.doc, at.span.clone(), message);
    let [b, c, ..] = params.as_slice() else {
        return Err(fail(
            "the bench function needs a bench and a circuit parameter",
        ));
    };
    let block = body
        .block()
        .ok_or_else(|| fail("the bench function needs a braced body"))?;
    let close = block
        .close
        .ok_or_else(|| fail("the bench function needs a closing brace"))?;
    Ok(InsertSite {
        function,
        block,
        close,
        b: &b.name,
        c: &c.name,
    })
}

pub(crate) fn remove_probe(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
) -> Result<Vec<TextEdit>, Diag> {
    remove_statement(
        sources,
        map,
        bench,
        &BenchKey::Probe(of.clone()),
        &of.to_string(),
        "probe",
    )
}

pub(crate) fn remove_supply(
    sources: &Sources, map: &SourceMap, bench: &str, net: &str,
) -> Result<Vec<TextEdit>, Diag> {
    remove_statement(sources, map, bench, &BenchKey::Supply(net.into()), net, "supply")
}

pub(crate) fn remove_source(
    sources: &Sources, map: &SourceMap, bench: &str, a: &str, b: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let key = BenchKey::Source(a.into(), b.into());
    remove_statement(sources, map, bench, &key, &format!("source ({a}, {b})"), "source or load")
}

pub(crate) fn set_probe_ref(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    reference: Option<&str>,
) -> Result<Vec<TextEdit>, Diag> {
    if of.quantity != ProbeQuantity::V {
        return Err(Diag::error(
            format!("{of}: only a voltage probe has a reference"),
            vec![],
        ));
    }
    if of.reference.as_deref() == reference {
        return Ok(vec![]);
    }
    let key = BenchKey::Probe(of.clone());
    let at = map
        .bench_site(bench, &key)
        .ok_or_else(|| Diag::error(format!("{bench}: no probe {of}"), vec![]))?;
    let new = ProbeOf {
        reference: reference.map(str::to_string),
        ..of.clone()
    };
    if let Some(site) = map.bench_site(bench, &BenchKey::Probe(new.clone())) {
        return Err(located(
            site.doc,
            site.span.clone(),
            format!("{new}: already probed"),
        ));
    }
    let (doc, body_at) = mapped(sources, map, bench)?;
    let tree = parse(doc)?;
    let span = remove_site(
        &tree,
        map,
        bench,
        body_at,
        at,
        &key,
        &of.to_string(),
        "probe",
    )?;
    let InsertSite { b, c, .. } = insert_site(&tree, body_at)?;
    let Kind::Call { args, .. } = &call_at(&tree, &span)
        .expect("remove_site found a direct call")
        .kind
    else {
        unreachable!()
    };
    let target = &args[0];
    let (span, text) = if of.reference.is_none() {
        (
            target.span.clone(),
            format!(
                "{b}.v({}, {c}.net({}))",
                tree.text(target),
                quote(reference.expect("a changed ground reference is a net"))
            ),
        )
    } else {
        let nets = target
            .call(&format!("{b}.v"))
            .filter(|args| args.len() == 2)
            .ok_or_else(|| {
                located(
                    doc.id,
                    target.span.start as u32..target.span.end as u32,
                    format!("{of}: this probe's nets are made by code; edit it there"),
                )
            })?;
        match reference {
            Some(reference) => (
                nets[1].span.clone(),
                format!("{c}.net({})", quote(reference)),
            ),
            None => (target.span.clone(), tree.text(&nets[0]).to_string()),
        }
    };
    Ok(vec![edit(doc, span, text)])
}

pub(crate) fn set_probe_tip(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    tip: &Tip,
) -> Result<Vec<TextEdit>, Diag> {
    if of.quantity != ProbeQuantity::V && (tip.r.is_some() || tip.c.is_some()) {
        return Err(Diag::error(
            "a current or power probe has no tip load",
            vec![],
        ));
    }
    let key = BenchKey::Probe(of.clone());
    let at = map
        .bench_site(bench, &key)
        .ok_or_else(|| Diag::error(format!("{bench}: no probe {of}"), vec![]))?;
    let (doc, body_at) = mapped(sources, map, bench)?;
    let tree = parse(doc)?;
    let span = remove_site(
        &tree,
        map,
        bench,
        body_at,
        at,
        &key,
        &of.to_string(),
        "probe",
    )?;
    let Kind::Call { args, .. } = &call_at(&tree, &span)
        .expect("remove_site found a direct call")
        .kind
    else {
        unreachable!()
    };
    let fail = |node: &Node, message: String| {
        located(doc.id, node.span.start as u32..node.span.end as u32, message)
    };
    let Some(object) = args.get(1) else {
        let mut fields = Vec::new();
        if let Some(load) = load_text(tip) {
            fields.push(format!("load: {load}"));
        }
        if let Some(si) = tip.bw {
            fields.push(format!("bw: {}", literal_text(&Quantity { si, unit: QtyUnit::Hertz })));
        }
        if fields.is_empty() {
            return Ok(vec![]);
        }
        return Ok(vec![edit(
            doc,
            args[0].span.end..args[0].span.end,
            format!(", {{{}}}", fields.join(" ")),
        )]);
    };
    if !matches!(object.kind, Kind::Object { ty: None, .. }) {
        return Err(fail(
            object,
            format!("{of}: this probe's options are made by code; edit them there"),
        ));
    }
    let refuse = |message: String| fail(object, format!("{of}: {message}"));
    let mut text = tree.text(object).to_string();
    let node = syntax::parse_value_with(&text, &circuit_units()).map_err(|e| refuse(e.message))?;
    // The bandwidth keeps the form the source writes it in: a `rise:` stays a rise time.
    let band = if node.field("rise").is_some() {
        ("rise", tip.rise(), QtyUnit::Second)
    } else {
        ("bw", tip.bw, QtyUnit::Hertz)
    };
    let edits = match (node.field("load"), load_text(tip)) {
        (Some(field), Some(_)) => {
            if !matches!(field.value.kind, Kind::Object { ty: None, .. }) {
                return Err(refuse("the tip load is made by code; edit it there".into()));
            }
            let mut load = text[field.value.span.clone()].to_string();
            set_numbers(&mut load, &[("r", tip.r, QtyUnit::Ohm), ("c", tip.c, QtyUnit::Farad)])
                .map_err(refuse)?;
            vec![Replace::new(field.value.span.clone(), load)]
        }
        (Some(_), None) => remove_field_edits(&text, &node, "load").map_err(|e| refuse(e.message))?,
        (None, Some(load)) => {
            add_field_edits(&text, &node, At::End, "load", &load).map_err(|e| refuse(e.message))?
        }
        (None, None) => vec![],
    };
    for replacement in edits.into_iter().rev() {
        text.replace_range(replacement.range, &replacement.text);
    }
    set_numbers(&mut text, &[band]).map_err(refuse)?;
    if text == tree.text(object) {
        return Ok(vec![]);
    }
    Ok(vec![changed_text(doc, object.span.clone(), &text)])
}

/// Renames a probe: writes `label` into its options object, adding the field (or the object)
/// when the probe has none. A label already reading `label` is a no-op; a label made by code is
/// refused, as are options made by code.
pub(crate) fn set_probe_label(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    label: &str,
) -> Result<Vec<TextEdit>, Diag> {
    set_probe_text(sources, map, bench, of, "label", Some(label), At::Start)
}

/// Places a probe's named connection on the drawings: writes `at` (a pin of its net,
/// `"<refdes>.<pad>"`) into its options object, or removes the field for None (each view picks).
/// Refused as [`set_probe_label`] refuses.
pub(crate) fn set_probe_at(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    at: Option<&str>,
) -> Result<Vec<TextEdit>, Diag> {
    set_probe_text(sources, map, bench, of, "at", at, At::End)
}

/// Writes the string option `field` of a probe (`value`; None removes it), adding the field (at
/// `place`) or the options object when absent. Unchanged is a no-op; a value or options made by
/// code are refused.
fn set_probe_text(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    of: &ProbeOf,
    field: &str,
    value: Option<&str>,
    place: At,
) -> Result<Vec<TextEdit>, Diag> {
    let key = BenchKey::Probe(of.clone());
    let at = map
        .bench_site(bench, &key)
        .ok_or_else(|| Diag::error(format!("{bench}: no probe {of}"), vec![]))?;
    let (doc, body_at) = mapped(sources, map, bench)?;
    let tree = parse(doc)?;
    let span = remove_site(&tree, map, bench, body_at, at, &key, &of.to_string(), "probe")?;
    let Kind::Call { args, .. } = &call_at(&tree, &span)
        .expect("remove_site found a direct call")
        .kind
    else {
        unreachable!()
    };
    let written = value.map(quote);
    let Some(object) = args.get(1) else {
        let Some(written) = written else { return Ok(vec![]) };
        return Ok(vec![edit(
            doc,
            args[0].span.end..args[0].span.end,
            format!(", {{{field}: {written}}}"),
        )]);
    };
    let refuse = |message: &str| {
        located(doc.id, object.span.start as u32..object.span.end as u32, format!("{of}: {message}"))
    };
    if !matches!(object.kind, Kind::Object { ty: None, .. }) {
        return Err(refuse("this probe's options are made by code; edit them there"));
    }
    let mut text = tree.text(object).to_string();
    let node = syntax::parse_value_with(&text, &circuit_units()).map_err(|e| refuse(&e.message))?;
    let by_code = format!("the {field} is made by code; edit it there");
    let edits = match (node.field(field), written) {
        (Some(found), written) => match (&found.value.kind, written) {
            (Kind::Str(now), Some(_)) if Some(now.as_str()) == value => vec![],
            (Kind::Str(_), Some(written)) => vec![Replace::new(found.value.span.clone(), written)],
            (Kind::Str(_), None) => remove_field_edits(&text, &node, field).map_err(|e| refuse(&e.message))?,
            _ => return Err(refuse(&by_code)),
        },
        (None, Some(written)) => add_field_edits(&text, &node, place, field, &written).map_err(|e| refuse(&e.message))?,
        (None, None) => vec![],
    };
    if edits.is_empty() {
        return Ok(vec![]);
    }
    for replacement in edits.into_iter().rev() {
        text.replace_range(replacement.range, &replacement.text);
    }
    Ok(vec![changed_text(doc, object.span.clone(), &text)])
}

/// A tip load's object text, `{r: 10Mohm c: 12pF}`; None for no load.
fn load_text(tip: &Tip) -> Option<String> {
    let fields: Vec<String> = [("r", tip.r, QtyUnit::Ohm), ("c", tip.c, QtyUnit::Farad)]
        .into_iter()
        .filter_map(|(key, si, unit)| Some(format!("{key}: {}", literal_text(&Quantity { si: si?, unit }))))
        .collect();
    (!fields.is_empty()).then(|| format!("{{{}}}", fields.join(" ")))
}

/// Sets, adds or removes number fields of an object literal's text in place. A written field
/// keeps its suffix, and one that already reads its value keeps its bytes.
fn set_numbers(text: &mut String, fields: &[(&str, Option<f64>, QtyUnit)]) -> Result<(), String> {
    for &(key, value, unit) in fields {
        // Reparse after each edit so adjacent fields and their separators never overlap.
        let node = syntax::parse_value_with(text, &circuit_units()).map_err(|e| e.message)?;
        let edits = match (node.field(key), value) {
            (Some(field), Some(si)) => {
                if number_literal(&field.value).is_none() {
                    return Err(format!("{key} is made by code; edit it there"));
                }
                let written = &text[field.value.span.clone()];
                let new = number_text(si, written).unwrap_or_else(|| literal_text(&Quantity { si, unit }));
                if new == written { vec![] } else { vec![Replace::new(field.value.span.clone(), new)] }
            }
            (Some(_), None) => remove_field_edits(text, &node, key).map_err(|e| e.message)?,
            (None, Some(si)) => add_field_edits(text, &node, At::End, key, &literal_text(&Quantity { si, unit }))
                .map_err(|e| e.message)?,
            (None, None) => vec![],
        };
        for replacement in edits.into_iter().rev() {
            text.replace_range(replacement.range, &replacement.text);
        }
    }
    Ok(())
}

fn remove_statement(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    key: &BenchKey,
    what: &str,
    kind: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, body_at) = mapped(sources, map, bench)?;
    let Some(at) = map.bench_site(bench, key) else {
        return Ok(Vec::new());
    };
    if matches!(key, BenchKey::Source(..)) {
        unique_stimulus(map, bench, key, at, what)?;
    }
    let tree = parse(doc)?;
    let span = remove_site(&tree, map, bench, body_at, at, key, what, kind)?;
    Ok(vec![edit(
        doc,
        removal_span(&doc.text, &span),
        String::new(),
    )])
}

pub(crate) fn remove_site(
    tree: &Syntax,
    map: &SourceMap,
    bench: &str,
    body_at: &SourceRange,
    at: &SourceRange,
    key: &BenchKey,
    what: &str,
    kind: &str,
) -> Result<Range<usize>, Diag> {
    let refuse = || {
        located(
            at.doc,
            at.span.clone(),
            format!("{what}: this {kind} is made by code (a loop or a function); edit it there"),
        )
    };
    let shared = map.bench_sites.iter().any(|((name, other), site)| {
        std::mem::discriminant(other) == std::mem::discriminant(key)
            && (name != bench || other != key)
            && &site.at == at
    });
    if shared || at.doc != body_at.doc {
        return Err(refuse());
    }
    let function = function(tree, body_at).map_err(|_| refuse())?;
    let Kind::Fn { body, .. } = &function.kind else {
        unreachable!()
    };
    let stmt = body
        .block()
        .and_then(|block| {
            block
                .stmts
                .iter()
                .find(|stmt| matches!(stmt.kind, Kind::Call { .. }) && same_span(stmt, at))
        })
        .ok_or_else(refuse)?;
    Ok(stmt.span.clone())
}

pub(crate) fn set_range(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    expect: &str,
    min: f64,
    max: f64,
    resolve_ranges: bool,
) -> Result<Vec<TextEdit>, Diag> {
    mapped(sources, map, bench)?;
    let (at, current) = range_entry(map, bench, expect)?;
    let doc = document(sources, at)?;
    let invalid = || located(
        at.doc,
        at.span.clone(),
        format!("{expect}: invalid expectation range"),
    );
    if min.is_nan() || max.is_nan() || min == f64::INFINITY || max == f64::NEG_INFINITY {
        return Err(invalid());
    }
    let tree = parse(doc)?;
    let unit = range_unit(current);
    let mut edits = Vec::new();
    let mut solutions = Vec::new();
    let mut changes = Vec::new();
    let mut object = None;
    let mut structural = false;
    let Some((old_min, old_max)) = current.range() else {
        return Err(located(
            at.doc,
            at.span.clone(),
            format!("{expect}: the expectation has a text bound, not a range"),
        ));
    };
    for (key, target, old) in [("min", min, old_min), ("max", max, old_max)] {
        // An unchanged target is not a constraint on a computed expression.
        if target == old {
            continue;
        }
        let what = format!("{expect} {key}");
        let RangeSite { bounds, field, derived } = range_site(
            &tree, at, map.bench_origin(bench, &BenchKey::Expect(expect.into())), key, &what, unit,
        )?;
        object = Some(bounds);
        if let Some(input) = derived {
            if !resolve_ranges || !target.is_finite() {
                return Err(input.refusal);
            }
            solve_input(doc, &tree, &input.leaf, input.a, input.b, target, &what, &mut solutions, &mut edits)?;
            continue;
        }
        let text = if target.is_finite() {
            Some(match field {
                Some(field) => number_text(
                    target,
                    tree.text(number_literal(&field.value).expect("literal bound")),
                )
                .ok_or_else(|| unrepresentable(doc, &field.value, &what))?,
                None => literal_text(&Quantity { si: target, unit }),
            })
        } else {
            None
        };
        if let (Some(field), Some(text)) = (field, &text) {
            edits.push(edit(doc, field.value.span.clone(), text.clone()));
        } else {
            structural = true;
        }
        if field.is_none() || text.is_none() {
            changes.push((key, text));
        }
    }
    if min > max {
        return Err(invalid());
    }
    if !structural {
        return Ok(edits);
    }
    // Apply structural changes in order so two added/removed fields cannot produce
    // overlapping edits (including separators). Literal-only edits keep separate spans.
    let object = object.expect("a changed bound has an object");
    let original = tree.text(object);
    let mut text = original.to_string();
    let mut inside: Vec<_> = edits.iter().filter(|edit| {
        object.span.start <= edit.span.start as usize && edit.span.end as usize <= object.span.end
    }).collect();
    inside.sort_by_key(|edit| edit.span.start);
    for change in inside.into_iter().rev() {
        text.replace_range(change.span.start as usize - object.span.start..change.span.end as usize - object.span.start, &change.text);
    }
    edits.retain(|edit| !(object.span.start <= edit.span.start as usize && edit.span.end as usize <= object.span.end));
    for (key, value) in changes {
        let node = syntax::parse_value_with(&text, &circuit_units())
            .map_err(|error| located(at.doc, at.span.clone(), error.message))?;
        let replacements = match value {
            Some(value) => match node.field(key) {
                Some(field) => vec![Replace::new(field.value.span.clone(), value)],
                None => add_field_edits(&text, &node, At::End, key, &value)
                    .map_err(|error| located(at.doc, at.span.clone(), error.message))?,
            },
            None => remove_field_edits(&text, &node, key)
                .map_err(|error| located(at.doc, at.span.clone(), error.message))?,
        };
        for replacement in replacements.into_iter().rev() {
            text.replace_range(replacement.range, &replacement.text);
        }
    }
    edits.push(changed_text(doc, object.span.clone(), &text));
    Ok(edits)
}

fn unique_stimulus<'a>(
    map: &'a SourceMap, bench: &str, key: &BenchKey, at: &SourceRange, what: &str,
) -> Result<Option<&'a Stimulus>, Diag> {
    match map.bench_stimuli.get(&(bench.into(), key.clone())) {
        Some(Stimulus::Ambiguous) => Err(located(at.doc, at.span.clone(), format!("{what}: ambiguous source pair"))),
        value => Ok(value),
    }
}

pub(crate) fn set_stimulus(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    target: BenchKey,
    key: &str,
    value: f64,
    resolve: bool,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, body_at) = mapped(sources, map, bench)?;
    let (statement, kind, arg) = match &target {
        BenchKey::Supply(net) => (format!("supply {net}"), "supply", 1),
        BenchKey::Source(a, b) => (format!("source ({a}, {b})"), "source or load", 2),
        _ => unreachable!("a stimulus edit targets a supply or source"),
    };
    let missing = || located(body_at.doc, body_at.span.clone(), format!("{bench}: no {statement}"));
    let at = map.bench_site(bench, &target).ok_or_else(missing)?;
    let stimulus = unique_stimulus(map, bench, &target, at, &statement)?.ok_or_else(missing)?;
    let option = stimulus.option(key).and_then(|option| {
        check_option_number(key, value)?;
        Ok(option)
    });
    if matches!(option, Ok((_, _, Some(old))) if old == value) {
        return Ok(Vec::new());
    }

    let tree = parse(doc)?;
    let span = remove_site(&tree, map, bench, body_at, at, &target, &statement, kind)?;
    let what = format!("{statement} {key}");
    let object = call_at(&tree, &span).and_then(Node::as_call)
        .and_then(|(_, args)| args.get(arg)).ok_or_else(|| computed(at, &what))?;
    if !matches!(object.kind, Kind::Object { ty: None, .. }) {
        return Err(computed_node(at, object, &what));
    }
    let field = object.field(key);
    let here = field.map_or(object, |f| &f.value);
    let (_, unit, _) = option.map_err(|message| {
        located(at.doc, here.span.start as u32..here.span.end as u32, format!("{statement}: {message}"))
    })?;
    if let Some(field) = field {
        if field.op != ":" {
            return Err(computed_node(at, here, &what));
        }
        if let Some(number) = number_literal(here) {
            let text = number_text(value, tree.text(number))
                .ok_or_else(|| unrepresentable(doc, here, &what))?;
            return Ok(vec![edit(doc, here.span.clone(), text)]);
        }
        let origin = map.bench_origin(bench, &target).filter(|_| resolve)
            .ok_or_else(|| computed_node(at, here, &what))?;
        let term = resolve::resolve(&tree, here, origin);
        let (leaf, a, b) = choose_input(&tree, here, origin, &term, Some(unit), &what)?;
        let mut edits = Vec::new();
        solve_input(doc, &tree, leaf, a, b, value, &what, &mut Vec::new(), &mut edits)?;
        return Ok(edits);
    }
    let text = literal_text(&Quantity { si: value, unit });
    let changes = add_field_edits(&doc.text, object, At::End, key, &text)
        .map_err(|error| located(doc.id, object.span.start as u32..object.span.end as u32, error.message))?;
    Ok(changes.into_iter().map(|r| edit(doc, r.range, r.text)).collect())
}

pub(crate) fn range_entry<'a>(
    map: &'a SourceMap,
    bench: &str,
    expect: &str,
) -> Result<(&'a SourceRange, &'a fab_elec_bench::expect::Expect), Diag> {
    let missing = || Diag::error(format!("{bench}: no expectation {expect}"), vec![]);
    let at = map
        .bench_site(bench, &BenchKey::Expect(expect.into()))
        .ok_or_else(missing)?;
    let current = map
        .bench_expects
        .get(&(bench.into(), expect.into()))
        .ok_or_else(missing)?;
    Ok((at, current))
}

/// Like a circuit's `Literal { count }`, a bound written once may make many
/// expectations through loops or helper functions. Editing it changes every
/// expectation made by that call; the call itself need not be a direct statement.
pub(crate) struct RangeSite<'a> {
    pub(crate) bounds: &'a Node,
    pub(crate) field: Option<&'a Field>,
    pub(crate) derived: Option<RangeInput>,
}

pub(crate) struct RangeInput {
    pub(crate) leaf: resolve::Leaf,
    a: f64,
    b: f64,
    refusal: Diag,
}

pub(crate) fn range_site<'a>(
    tree: &'a Syntax,
    at: &SourceRange,
    origin: Option<&Origin>,
    key: &str,
    what: &str,
    unit: QtyUnit,
) -> Result<RangeSite<'a>, Diag> {
    let call = call_at(tree, &(at.span.start as usize..at.span.end as usize))
        .ok_or_else(|| computed(at, what))?;
    let bounds = call
        .as_call()
        .and_then(|(_, args)| args.get(2))
        .ok_or_else(|| computed(at, what))?;
    if !matches!(bounds.kind, Kind::Object { ty: None, .. }) {
        return Err(computed_node(at, bounds, what));
    }
    let field = bounds.field(key);
    if let Some(field) = field {
        if field.op != ":" {
            return Err(computed_node(at, &field.value, what));
        }
        if number_literal(&field.value).is_none() {
            let origin = origin.ok_or_else(|| computed_node(at, &field.value, what))?;
            let term = resolve::resolve(tree, &field.value, origin);
            let (leaf, a, b) = choose_input(tree, &field.value, origin, &term, Some(unit), what)?;
            return Ok(RangeSite { bounds, field: Some(field), derived: Some(RangeInput {
                leaf: leaf.clone(), a, b, refusal: derived_refusal(tree, &field.value, origin, what),
            }) });
        }
    }
    Ok(RangeSite { bounds, field, derived: None })
}

pub(crate) fn range_unit(current: &fab_elec_bench::expect::Expect) -> QtyUnit {
    match current.probe.unit() {
        "V" => QtyUnit::Volt,
        "A" => QtyUnit::Ampere,
        _ => QtyUnit::Watt,
    }
}

fn choose_input<'a>(
    tree: &Syntax,
    expr: &Node,
    origin: &Origin,
    term: &'a resolve::Term,
    unit: Option<QtyUnit>,
    what: &str,
) -> Result<(&'a resolve::Leaf, f64, f64), Diag> {
    term.choose(tree, origin, unit).map_err(|reason| derived_reason(tree, expr, origin, format!(
        "{what}: this value is computed here; {reason}"
    )))
}

fn solve_input(
    doc: &Doc,
    tree: &Syntax,
    leaf: &resolve::Leaf,
    a: f64,
    b: f64,
    target: f64,
    what: &str,
    solutions: &mut Vec<(Range<usize>, f64)>,
    edits: &mut Vec<TextEdit>,
) -> Result<(), Diag> {
    let value = (target - a) / b;
    if let Some((_, previous)) = solutions.iter().find(|(at, _)| at == &leaf.at) {
        if *previous != value {
            return Err(located(doc.id, leaf.at.start as u32..leaf.at.end as u32, format!(
                "{what}: the targets require different values for the same input `{}`", &tree.source()[leaf.at.clone()]
            )));
        }
        return Ok(());
    }
    let written = &tree.source()[leaf.at.clone()];
    // The shortest decimal from six significant digits up to an f64 round trip whose result
    // passes verification's tolerance; the writer keeps the suffix and decimal style.
    let text = (6..=17).find_map(|digits| {
        let rounded: f64 = format!("{value:.*e}", digits - 1).parse().ok()?;
        crate::verify::close(a + b * rounded, target).then(|| number_text(rounded, written)).flatten()
    }).ok_or_else(|| located(
        doc.id, leaf.at.start as u32..leaf.at.end as u32, format!("{what}: the solution cannot be written with its current suffix")
    ))?;
    solutions.push((leaf.at.clone(), value));
    edits.push(edit(doc, leaf.at.clone(), text));
    Ok(())
}

pub(crate) fn set_drive(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    stub: &str,
    pin: &str,
    drive: &Drive,
    resolve_drives: bool,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, body_at) = mapped(sources, map, bench)?;
    let key = BenchKey::Stub(stub.into(), pin.into());
    let (at, current) = drive_entry(map, bench, body_at, stub, pin)?;
    let what = format!("{stub}.{pin}");
    let tree = parse(doc)?;
    if resolve_drives && current == drive {
        return Ok(Vec::new());
    }
    let site = drive_site(
        &tree,
        map,
        body_at,
        at,
        map.bench_origin(bench, &key),
        &what,
        current,
        DriveChange::between(current, drive),
        resolve_drives,
    )?;
    let (call, args, b) = match site {
        DriveSite::Literal { call, args, b } => (call, args, b),
        DriveSite::Derived { site, refusal } => {
            if !resolve_drives {
                return Err(refusal);
            }
            return resolved_drive(doc, &tree, site, current, drive, &what);
        }
        DriveSite::Alternatives { site, refusal } => {
            if !resolve_drives {
                return Err(refusal);
            }
            return resolved_drive_arms(doc, &tree, site, current, drive, &what);
        }
    };
    if let Drive::Pwm { freq, duty } = drive {
        if !freq.is_finite() || *freq <= 0.0 || !duty.is_finite() || !(0.0..=1.0).contains(duty) {
            return Err(located(
                at.doc,
                call.span.start as u32..call.span.end as u32,
                format!("{what}: PWM needs a finite positive frequency and duty between 0 and 1"),
            ));
        }
    }
    if current == drive {
        return Ok(Vec::new());
    }
    match (current, drive) {
        (
            Drive::Pwm {
                freq: old_freq,
                duty: old_duty,
            },
            Drive::Pwm { freq, duty },
        ) => {
            let mut edits = Vec::new();
            for (node, old, target) in [(&args[0], old_freq, freq), (&args[1], old_duty, duty)] {
                if target != old {
                    let literal = number_literal(node).expect("a changed PWM argument is literal");
                    let text = number_text(*target, tree.text(literal))
                        .ok_or_else(|| unrepresentable(doc, node, &what))?;
                    edits.push(edit(doc, node.span.clone(), text));
                }
            }
            Ok(edits)
        }
        (Drive::Input(_), Drive::Input(pull)) => Ok(vec![edit(
            doc,
            args[0].span.clone(),
            pull_text(*pull).into(),
        )]),
        _ => Ok(vec![edit(doc, call.span.clone(), drive_text(b, drive))]),
    }
}

/// An explicit literal write never asks the resolver to change the drive's inputs.
pub(crate) fn write_drive(
    sources: &Sources,
    map: &SourceMap,
    bench: &str,
    stub: &str,
    pin: &str,
    drive: &Drive,
) -> Result<Vec<TextEdit>, Diag> {
    let (doc, body_at) = mapped(sources, map, bench)?;
    let at = map
        .bench_stubs
        .get(&(bench.into(), stub.into()))
        .ok_or_else(|| Diag::error(format!("{bench}: no stub {stub}"), vec![]))?;
    let what = format!("{stub}.{pin}");
    let refuse = |reason: &str| located(
        at.doc,
        at.span.clone(),
        format!("{what}: cannot write this drive literally: {reason}"),
    );
    if at.doc != doc.id {
        return Err(refuse("the stub call is outside the bench document"));
    }
    if let Drive::Pwm { freq, duty } = drive {
        if !freq.is_finite() || *freq <= 0.0 || !duty.is_finite() || !(0.0..=1.0).contains(duty) {
            return Err(located(
                at.doc,
                at.span.clone(),
                format!("{what}: PWM needs a finite positive frequency and duty between 0 and 1"),
            ));
        }
    }
    let tree = parse(doc)?;
    if let Ok((drive_at, current)) = drive_entry(map, bench, body_at, stub, pin) {
        let change = match current {
            Drive::Pwm { .. } => DriveChange::Pwm { freq: true, duty: true },
            Drive::Input(_) => DriveChange::Input { pull: true },
            Drive::Level(_) => DriveChange::Replace,
        };
        if drive_literal_site(&tree, map, body_at, drive_at, &what, current, change).is_ok() {
            return set_drive(sources, map, bench, stub, pin, drive, false);
        }
    }
    let InsertSite { block, b, .. } = insert_site(&tree, body_at)
        .map_err(|_| refuse("the bench build needs a function literal with a braced body"))?;
    let call = call_at(&tree, &(at.span.start as usize..at.span.end as usize))
        .ok_or_else(|| refuse("the stub call is not written here"))?;
    let pins = call
        .call(&format!("{b}.stub"))
        .and_then(|args| args.get(1))
        .ok_or_else(|| refuse("the stub must call the bench parameter with a pins argument"))?;
    let constructor = drive_text(b, drive);
    if let Some(name) = pins.ident() {
        let index = block.stmts.iter().position(|stmt| stmt.span == call.span)
            .ok_or_else(|| refuse("the stub must be a statement in the bench function body"))?;
        if !pins_object_binding(&tree, block, index, name) {
            return Err(refuse("the pins name must be bound by let to an object in this function body"));
        }
        let key = syntax::write::key(pin).map_err(|reason| refuse(&reason))?;
        let target = if key == pin {
            format!("{name}.{key}")
        } else {
            format!("{name}[{key}]")
        };
        let indent = line_indent(&doc.text, call.span.start);
        let newline = if doc.text.contains("\r\n") { "\r\n" } else { "\n" };
        let (pos, text) = before(
            &doc.text,
            call.span.start,
            indent,
            indent,
            &format!("{target} = {constructor}"),
            newline,
        );
        return Ok(vec![edit(doc, pos..pos, text)]);
    }
    if matches!(pins.kind, Kind::Object { ty: None, .. }) {
        if let Some(field) = pins.field(pin) {
            if field.op != ":" {
                return Err(refuse("the pin must be an ordinary object field"));
            }
            return Ok(vec![edit(doc, field.value.span.clone(), constructor)]);
        }
        return add_field_edits(&doc.text, pins, At::End, pin, &constructor)
            .map(|edits| edits.into_iter().map(|e| edit(doc, e.range, e.text)).collect())
            .map_err(|error| refuse(&error.message));
    }
    Err(refuse("the pins argument must be an object literal or a local let-bound object; call results and other expressions are not writable"))
}

fn pins_object_binding(tree: &Syntax, block: &Block, index: usize, name: &str) -> bool {
    let mut bindings = block.stmts[..index].iter().filter(|stmt| stmt.let_name() == Some(name));
    let Some(binding) = bindings.next() else { return false };
    if bindings.next().is_some() {
        return false;
    }
    let Kind::Let { keyword, value: Some(value), .. } = &binding.kind else { return false };
    if keyword != "let" || !matches!(value.kind, Kind::Object { ty: None, .. }) {
        return false;
    }
    let mut assigned = false;
    tree.walk(&mut |_, node| {
        if binding.span.end <= node.span.start && node.span.end <= block.stmts[index].span.start {
            if let Kind::Assign { target, .. } = &node.kind {
                assigned |= target.ident() == Some(name);
            }
        }
    });
    !assigned
}

pub(crate) fn drive_entry<'a>(
    map: &'a SourceMap,
    bench: &str,
    body_at: &SourceRange,
    stub: &str,
    pin: &str,
) -> Result<(&'a SourceRange, &'a Drive), Diag> {
    let missing = || Diag::error(format!("{bench}: no stub pin {stub}.{pin}"), vec![]);
    let at = map
        .bench_site(bench, &BenchKey::Stub(stub.into(), pin.into()))
        .ok_or_else(missing)?;
    let current = map
        .bench_drives
        .get(&(bench.into(), stub.into(), pin.into()))
        .ok_or_else(missing)?;
    if at.doc != body_at.doc {
        return Err(computed(at, &format!("{stub}.{pin}")));
    }
    Ok((at, current))
}

/// Which arguments a request changes, without inventing numeric target values for a view.
#[derive(Clone, Copy)]
pub(crate) enum DriveChange {
    Pwm { freq: bool, duty: bool },
    Input { pull: bool },
    Replace,
}

impl DriveChange {
    pub(crate) fn same_kind(current: &Drive) -> Self {
        match current {
            // A card's same-kind PWM edit changes duty; frequency leaves are
            // still included when the constructor needs resolution.
            Drive::Pwm { .. } => Self::Pwm { freq: false, duty: true },
            Drive::Input(_) => Self::Input { pull: true },
            Drive::Level(_) => Self::Replace,
        }
    }

    fn between(current: &Drive, target: &Drive) -> Self {
        match (current, target) {
            (
                Drive::Pwm {
                    freq: old_freq,
                    duty: old_duty,
                },
                Drive::Pwm { freq, duty },
            ) => Self::Pwm { freq: old_freq != freq, duty: old_duty != duty },
            (Drive::Input(old), Drive::Input(new)) => Self::Input { pull: old != new },
            _ => Self::Replace,
        }
    }
}

pub(crate) enum DriveSite<'a> {
    Literal {
        call: &'a Node,
        args: &'a [Node],
        b: &'a str,
    },
    Derived {
        site: ResolvedDriveSite<'a>,
        refusal: Diag,
    },
    Alternatives {
        site: DriveArms<'a>,
        refusal: Diag,
    },
}

pub(crate) struct ResolvedDriveSite<'a> {
    pub(crate) origin: &'a Origin,
    args: &'a [Node],
    pub(crate) terms: [resolve::Term; 2],
}

pub(crate) struct DriveArms<'a> {
    pub(crate) origin: &'a Origin,
    expr: &'a Node,
    arms: Vec<DriveArm<'a>>,
}

struct DriveArm<'a> {
    call: &'a Node,
    form: ArmForm<'a>,
}

enum ArmForm<'a> {
    Pwm { args: &'a [Node], terms: [resolve::Term; 2] },
    Fixed { drive: Drive, guard: ArmGuard },
}

enum ArmGuard {
    Else,
    Equality { term: resolve::Term, value: f64 },
    Opaque,
}

impl DriveArm<'_> {
    fn terms(&self) -> Vec<(&resolve::Term, Option<QtyUnit>)> {
        match &self.form {
            ArmForm::Pwm { terms, .. } => vec![(&terms[0], Some(QtyUnit::Hertz)), (&terms[1], None)],
            ArmForm::Fixed { guard: ArmGuard::Equality { term, .. }, .. } => vec![(term, None)],
            _ => Vec::new(),
        }
    }
}

impl DriveArms<'_> {
    /// Keep a running PWM's arguments first. A fixed drive needs another arm's inputs.
    pub(crate) fn terms(&self) -> impl Iterator<Item = (&resolve::Term, Option<QtyUnit>)> {
        self.arms.iter().filter(|arm| {
            same_span(arm.call, &self.origin.call) && matches!(arm.form, ArmForm::Pwm { .. })
        }).chain(self.arms.iter().filter(|arm| !same_span(arm.call, &self.origin.call)))
            .flat_map(DriveArm::terms)
    }
}

/// The shared alternative-form classifier for both verified splices and bench views.
/// Only a direct arm tail on the bench parameter participates; guards stay independent.
fn candidate_arms<'a>(
    tree: &'a Syntax,
    body_at: &SourceRange,
    origin: &'a Origin,
) -> Option<DriveArms<'a>> {
    if origin.call.doc != body_at.doc || origin.call.doc.0 & crate::source::LIB_DOC != 0 {
        return None;
    }
    let expr = resolve::expression_at_origin(tree, origin)?;
    let Kind::Fn { params, body, .. } = &function(tree, body_at).ok()?.kind else {
        return None;
    };
    if expr.span.start < body.span.start || expr.span.end > body.span.end {
        return None;
    }
    let b = params.first()?.name.as_str();
    let tail = |node: &'a Node| node.block().and_then(|block| block.stmts.last()).unwrap_or(node);
    let mut tails = Vec::new();
    let mut next = expr;
    while let Kind::If { cond, then, otherwise } = &next.kind {
        tails.push((Some(cond.as_ref()), tail(then)));
        let Some(otherwise) = otherwise else { break };
        if matches!(otherwise.kind, Kind::If { .. }) {
            next = otherwise;
        } else {
            tails.push((None, tail(otherwise)));
            break;
        }
    }
    let mut found_origin = false;
    let mut arms = Vec::new();
    for (guard, call) in tails {
        let Some((callee, args)) = call.as_call() else { continue };
        let Some(method) = callee.strip_prefix(b).and_then(|rest| rest.strip_prefix('.')) else {
            continue;
        };
        let valid = match method {
            "pwm" => args.len() == 2,
            "high" | "low" => args.is_empty(),
            "input" => args.len() == 1,
            _ => false,
        };
        if !valid {
            continue;
        }
        found_origin |= same_span(call, &origin.call);
        let form = if method == "pwm" {
            ArmForm::Pwm {
                args,
                terms: [resolve::resolve(tree, &args[0], origin), resolve::resolve(tree, &args[1], origin)],
            }
        } else {
            let drive = match method {
                "high" | "low" => Drive::Level(method == "high"),
                "input" => Drive::Input(match args[0].name() {
                    Some("up") => Pull::Up,
                    Some("down") => Pull::Down,
                    Some("none") => Pull::None,
                    _ => continue,
                }),
                _ => unreachable!(),
            };
            ArmForm::Fixed {
                drive,
                guard: guard.map_or(ArmGuard::Else, |guard| equality_guard(tree, guard, origin)),
            }
        };
        arms.push(DriveArm { call, form });
    }
    found_origin.then_some(DriveArms { origin, expr, arms })
}

fn equality_guard(tree: &Syntax, guard: &Node, origin: &Origin) -> ArmGuard {
    let guard = unparen(guard);
    let Grouped::Binary { op: "==", lhs, rhs, .. } = guard.grouped() else {
        return ArmGuard::Opaque;
    };
    let (Grouped::Operand(lhs), Grouped::Operand(rhs)) = (*lhs, *rhs) else {
        return ArmGuard::Opaque;
    };
    for (expr, constant) in [(lhs, rhs), (rhs, lhs)] {
        let constant = unparen(constant);
        if number_literal(constant).is_none() {
            continue;
        }
        let constant = resolve::resolve(tree, constant, origin);
        let Some((leaf, scale)) = constant.leaf() else { continue };
        let term = resolve::resolve(tree, expr, origin);
        if term.choose(tree, origin, None).is_ok() {
            return ArmGuard::Equality { term, value: leaf.value * scale };
        }
    }
    ArmGuard::Opaque
}

fn unparen(mut node: &Node) -> &Node {
    while let Kind::Paren { items, .. } = &node.kind {
        if items.len() != 1 {
            break;
        }
        node = &items[0];
    }
    node
}

pub(crate) fn drive_site<'a>(
    tree: &'a Syntax,
    map: &SourceMap,
    body_at: &SourceRange,
    at: &SourceRange,
    origin: Option<&'a Origin>,
    what: &str,
    current: &Drive,
    change: DriveChange,
    resolve_drives: bool,
) -> Result<DriveSite<'a>, Diag> {
    let refusal = match drive_literal_site(tree, map, body_at, at, what, current, change) {
        Ok((call, args, b)) => return Ok(DriveSite::Literal { call, args, b }),
        Err(error) => error,
    };
    let Some(origin) = origin else {
        return Err(refusal);
    };
    if let Some(site) = candidate_arms(tree, body_at, origin) {
        if resolve_drives || site.terms().any(|(term, unit)| term.choose(tree, origin, unit).is_ok()) {
            return Ok(DriveSite::Alternatives { site, refusal });
        }
    }
    match resolved_drive_site(tree, origin, current, change, what) {
        Ok(site) => Ok(DriveSite::Derived { site, refusal }),
        // If no PWM argument resolves, neither entry point can edit it. Both
        // report the same located inputs that the view shows.
        Err(error) if resolve_drives || matches!(change, DriveChange::Pwm { .. }) => Err(error),
        Err(_) => Err(refusal),
    }
}

/// Outside alternative arms, resolve only arguments of the constructor that actually ran.
fn resolved_drive_site<'a>(
    tree: &'a Syntax,
    origin: &'a Origin,
    current: &Drive,
    change: DriveChange,
    what: &str,
) -> Result<ResolvedDriveSite<'a>, Diag> {
    let span = origin.call.span.start as usize..origin.call.span.end as usize;
    let call = call_at(tree, &span).ok_or_else(|| computed(&origin.call, what))?;
    let context = resolve::expression_at_origin(tree, origin).unwrap_or(call);
    if !matches!((current, change), (Drive::Pwm { .. }, DriveChange::Pwm { .. })) {
        return Err(derived_refusal(tree, context, origin, what));
    }
    let (_, args) = call
        .as_call()
        .filter(|(callee, args)| callee.ends_with(".pwm") && args.len() == 2)
        .ok_or_else(|| derived_refusal(tree, context, origin, what))?;
    let terms = [
        resolve::resolve(tree, &args[0], origin),
        resolve::resolve(tree, &args[1], origin),
    ];
    if let DriveChange::Pwm { freq, duty } = change {
        for (index, changed, unit) in [(0, freq, Some(QtyUnit::Hertz)), (1, duty, None)] {
            if changed {
                choose_input(tree, &args[index], origin, &terms[index], unit, what)?;
            }
        }
    }
    Ok(ResolvedDriveSite { origin, args, terms })
}

fn resolved_drive(
    doc: &Doc,
    tree: &Syntax,
    site: ResolvedDriveSite<'_>,
    current: &Drive,
    target: &Drive,
    what: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let ResolvedDriveSite { origin, args, terms } = site;
    let (
        Drive::Pwm {
            freq: old_freq,
            duty: old_duty,
        },
        Drive::Pwm { freq, duty },
    ) = (current, target)
    else {
        unreachable!("a resolved drive site changes PWM arguments")
    };
    if !freq.is_finite() || *freq <= 0.0 || !duty.is_finite() || !(0.0..=1.0).contains(duty) {
        return Err(located(
            origin.call.doc,
            origin.call.span.clone(),
            format!("{what}: PWM needs a finite positive frequency and duty between 0 and 1"),
        ));
    }
    let mut edits: Vec<TextEdit> = Vec::new();
    let mut solutions = Vec::new();
    for ((expr, old, target, unit), term) in [(&args[0], old_freq, freq, Some(QtyUnit::Hertz)), (&args[1], old_duty, duty, None)]
        .into_iter()
        .zip(&terms)
    {
        if target == old {
            continue;
        }
        let (leaf, a, b) = choose_input(tree, expr, origin, term, unit, what)?;
        solve_input(doc, tree, leaf, a, b, *target, what, &mut solutions, &mut edits)?;
    }
    Ok(edits)
}

fn resolved_drive_arms(
    doc: &Doc,
    tree: &Syntax,
    site: DriveArms<'_>,
    current: &Drive,
    target: &Drive,
    what: &str,
) -> Result<Vec<TextEdit>, Diag> {
    let DriveArms { origin, expr, arms } = site;
    if let Drive::Pwm { freq, duty } = target {
        if !freq.is_finite() || *freq <= 0.0 || !duty.is_finite() || !(0.0..=1.0).contains(duty) {
            return Err(located(
                origin.call.doc,
                origin.call.span.clone(),
                format!("{what}: PWM needs a finite positive frequency and duty between 0 and 1"),
            ));
        }
    }
    let mut matched = false;
    for DriveArm { call, form } in arms {
        let edits = match form {
            ArmForm::Pwm { args, terms } if matches!(target, Drive::Pwm { .. }) => {
                matched = true;
                let old = if same_span(call, &origin.call) && matches!(current, Drive::Pwm { .. }) {
                    current.clone()
                } else {
                    let (Some(freq), Some(duty)) = (terms[0].value(), terms[1].value()) else {
                        continue;
                    };
                    Drive::Pwm { freq, duty }
                };
                let site = ResolvedDriveSite { origin, args, terms };
                let Ok(edits) = resolved_drive(doc, tree, site, &old, target, what) else {
                    continue;
                };
                edits
            }
            ArmForm::Fixed { drive, guard } if &drive == target => {
                matched = true;
                match guard {
                    ArmGuard::Equality { term, value } => {
                        let (leaf, a, b) = term.choose(tree, origin, None).expect("an equality guard has a chosen input");
                        if term.value() == Some(value) {
                            Vec::new()
                        } else {
                            let mut edits = Vec::new();
                            if solve_input(doc, tree, leaf, a, b, value, what, &mut Vec::new(), &mut edits).is_err() {
                                continue;
                            }
                            edits
                        }
                    }
                    ArmGuard::Else => Vec::new(),
                    ArmGuard::Opaque => continue,
                }
            }
            _ => continue,
        };
        if !edits.is_empty() || crate::verify::same_drive(current, target) {
            // Selection is final. The caller evaluates once and never tries another arm.
            return Ok(edits);
        }
    }
    let kind = match target {
        Drive::Pwm { .. } => "a PWM",
        Drive::Level(true) => "a high",
        Drive::Level(false) => "a low",
        Drive::Input(_) => "an input",
    };
    let reason = if matched {
        format!("no matching arm has a solvable edit for {kind}")
    } else {
        format!("no arm makes {kind}")
    };
    Err(derived_reason(tree, expr, origin, format!(
        "{what}'s drive is made by this `if`; {reason}"
    )))
}

fn derived_refusal(tree: &Syntax, expr: &Node, origin: &Origin, what: &str) -> Diag {
    derived_reason(tree, expr, origin, format!(
        "{what}: this value is computed here; edit the values it is made from"
    ))
}

fn derived_reason(tree: &Syntax, expr: &Node, origin: &Origin, message: String) -> Diag {
    let range = |span: Range<usize>| SourceRange {
        doc: origin.call.doc,
        span: span.start as u32..span.end as u32,
    };
    let at = range(expr.span.clone());
    let inputs: Vec<_> = resolve::inputs(tree, expr, origin)
        .into_iter()
        .map(|leaf| (range(leaf.at), leaf.value))
        .collect();
    let mut diag = located(at.doc, at.span, message);
    if !inputs.is_empty() {
        diag.message.push_str("; inputs: ");
        diag.message.push_str(
            &inputs
                .iter()
                .map(|(at, value)| {
                    format!(
                        "`{}` = {value}",
                        &tree.source()[at.span.start as usize..at.span.end as usize]
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        for (at, _) in inputs {
            let sel = Sel::Source {
                doc: at.doc,
                start: at.span.start,
                end: at.span.end,
            };
            if !diag.sel.contains(&sel) {
                diag.sel.push(sel);
            }
        }
    }
    diag
}

/// A unique written field or assignment calling a constructor on the bench parameter. Keep
/// all literal-site decisions here so term resolution can replace this boundary.
fn drive_literal_site<'a>(
    tree: &'a Syntax,
    map: &SourceMap,
    body_at: &SourceRange,
    at: &SourceRange,
    what: &str,
    current: &Drive,
    change: DriveChange,
) -> Result<(&'a Node, &'a [Node], &'a str), Diag> {
    let mut field = None;
    tree.walk(&mut |_, node| {
        if let Kind::Object {
            ty: None, fields, ..
        } = &node.kind
        {
            field = fields
                .iter()
                .find(|f| {
                    f.span.start == at.span.start as usize && f.span.end == at.span.end as usize
                })
                .or(field);
        }
    });
    let function = function(tree, body_at).map_err(|_| computed(at, what))?;
    let Kind::Fn { params, body, .. } = &function.kind else {
        unreachable!()
    };
    let call = if let Some(field) = field.filter(|field| field.op == ":") {
        &field.value
    } else {
        body.block().and_then(|block| block.stmts.iter().find_map(|stmt| {
            let Kind::Assign { op, value, .. } = &stmt.kind else { return None };
            (op == "=" && same_span(value, at)).then_some(value.as_ref())
        })).ok_or_else(|| computed(at, what))?
    };
    let refuse = || computed_node(at, call, what);
    // Count keys, rather than values: two pins can share an equal drive legitimately,
    // but editing a field that supplies both would change the other pin too.
    let users = map
        .bench_sites
        .iter()
        .filter(|((_, key), site)| matches!(key, BenchKey::Stub(_, _)) && &site.at == at)
        .count();
    if users != 1 {
        return Err(refuse());
    }
    let b = params.first().ok_or_else(refuse)?.name.as_str();
    if call.span.start < body.span.start || call.span.end > body.span.end {
        return Err(refuse());
    }
    let mut nested = false;
    tree.walk(&mut |_, node| {
        if matches!(node.kind, Kind::Fn { .. }) && node.span != function.span
            && body.span.start <= node.span.start && node.span.end <= body.span.end
            && node.span.start <= call.span.start && call.span.end <= node.span.end
        {
            nested = true;
        }
    });
    if nested {
        return Err(refuse());
    }
    let (callee, args) = call.as_call().ok_or_else(refuse)?;
    let method = callee
        .strip_prefix(b)
        .and_then(|rest| rest.strip_prefix('.'))
        .ok_or_else(refuse)?;
    let valid = match current {
        Drive::Pwm { .. } => method == "pwm" && args.len() == 2,
        Drive::Level(level) => method == (if *level { "high" } else { "low" }) && args.is_empty(),
        Drive::Input(_) => method == "input" && args.len() == 1,
    };
    if !valid {
        return Err(refuse());
    }
    match change {
        DriveChange::Pwm { freq, duty } => {
            for (node, changed) in [(&args[0], freq), (&args[1], duty)] {
                if changed && number_literal(node).is_none() {
                    return Err(computed_node(at, node, what));
                }
            }
        }
        DriveChange::Input { pull: true } => {
            if !matches!(args[0].name(), Some("up" | "down" | "none")) {
                return Err(computed_node(at, &args[0], what));
            }
        }
        _ => {}
    }
    Ok((call, args, b))
}

fn drive_text(b: &str, drive: &Drive) -> String {
    match drive {
        Drive::Pwm { freq, duty } => format!(
            "{b}.pwm({}, {duty})",
            literal_text(&Quantity {
                si: *freq,
                unit: QtyUnit::Hertz
            })
        ),
        Drive::Level(high) => format!("{b}.{}()", if *high { "high" } else { "low" }),
        Drive::Input(pull) => format!("{b}.input({})", pull_text(*pull)),
    }
}

fn pull_text(pull: Pull) -> &'static str {
    match pull {
        Pull::Up => "@up",
        Pull::Down => "@down",
        Pull::None => "@none",
    }
}

fn number_literal(node: &Node) -> Option<&Node> {
    match &node.kind {
        Kind::Number(_) => Some(node),
        Kind::Unary { op, operand }
            if matches!(op.as_str(), "-" | "+") && matches!(operand.kind, Kind::Number(_)) =>
        {
            Some(operand)
        }
        _ => None,
    }
}

/// Preserve the written suffix even for small coefficients and percentages.
/// Shift decimal digits, rather than multiply floats, to keep the requested SI value exact.
fn number_text(value: f64, written: &str) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let units = circuit_units();
    let written = written.replace('_', "");
    let Some(entry) = units.entries().iter().find(|entry| {
        written
            .strip_suffix(entry.suffix.as_str())
            .is_some_and(|digits| digits.parse::<f64>().is_ok())
    }) else {
        return Some(if written.contains('.') && value.fract() == 0.0 {
            format!("{value:.1}")
        } else {
            value.to_string()
        });
    };
    if let Some((unit, _, _)) = qty_of(&entry.suffix) {
        let candidate = literal_for(&Quantity { si: value, unit }, &written);
        if candidate
            .strip_suffix(entry.suffix.as_str())
            .is_some_and(|digits| digits.parse::<f64>().is_ok() && entry.literal(digits) == value)
        {
            return Some(candidate);
        }
    }
    let pow10 = |value| (0..=22).find(|&power| 10f64.powi(power) == value);
    let digits = match (pow10(entry.mul), pow10(entry.div)) {
        (Some(mul), Some(div)) => {
            let scientific = format!("{value:e}");
            let (mantissa, exponent) = scientific.split_once('e').expect("finite decimal");
            decimal_text(
                mantissa,
                exponent.parse::<i32>().expect("decimal exponent") - mul + div,
            )
        }
        _ => (value / entry.mul * entry.div).to_string(),
    };
    (entry.literal(&digits) == value).then(|| format!("{digits}{}", entry.suffix))
}

fn unrepresentable(doc: &Doc, node: &Node, what: &str) -> Diag {
    located(
        doc.id,
        node.span.start as u32..node.span.end as u32,
        format!("{what}: this value cannot be written exactly with its current suffix"),
    )
}

fn computed(at: &SourceRange, what: &str) -> Diag {
    located(
        at.doc,
        at.span.clone(),
        format!("{what}: this value is computed here; edit the values it is made from"),
    )
}

fn computed_node(at: &SourceRange, node: &Node, what: &str) -> Diag {
    computed(
        &SourceRange {
            doc: at.doc,
            span: node.span.start as u32..node.span.end as u32,
        },
        what,
    )
}

/// Trim equal Unicode text around structural object changes, retaining surrounding source.
fn changed_text(doc: &Doc, span: Range<usize>, text: &str) -> TextEdit {
    let original = &doc.text[span.clone()];
    let prefix: usize = original
        .chars()
        .zip(text.chars())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum();
    let suffix: usize = original[prefix..]
        .chars()
        .rev()
        .zip(text[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum();
    edit(
        doc,
        span.start + prefix..span.end - suffix,
        text[prefix..text.len() - suffix].into(),
    )
}

pub(crate) fn document<'a>(sources: &'a Sources, at: &SourceRange) -> Result<&'a Doc, Diag> {
    sources.doc(at.doc).ok_or_else(|| {
        located(
            at.doc,
            at.span.clone(),
            "the bench document is not in the project",
        )
    })
}

pub(crate) fn mapped<'a>(
    sources: &'a Sources,
    map: &'a SourceMap,
    bench: &str,
) -> Result<(&'a Doc, &'a SourceRange), Diag> {
    let at = map
        .bench_site(bench, &BenchKey::Body)
        .ok_or_else(|| Diag::error(format!("no bench named {bench}"), vec![]))?;
    let doc = sources.doc(at.doc).ok_or_else(|| {
        located(
            at.doc,
            at.span.clone(),
            "the bench document is not in the project",
        )
    })?;
    Ok((doc, at))
}

pub(crate) fn parse(doc: &Doc) -> Result<Syntax, Diag> {
    syntax::parse_with(&doc.text, &circuit_units())
        .map_err(|error| located(doc.id, error.at as u32..error.at as u32, error.message))
}

fn function<'a>(tree: &'a Syntax, at: &SourceRange) -> Result<&'a Node, Diag> {
    let mut found = None;
    tree.walk(&mut |_, node| {
        if same_span(node, at) && matches!(node.kind, Kind::Fn { .. }) {
            found = Some(node);
        }
    });
    found.ok_or_else(|| {
        located(
            at.doc,
            at.span.clone(),
            "the bench build must be a function literal: edit it there",
        )
    })
}

fn same_span(node: &Node, at: &SourceRange) -> bool {
    node.span.start == at.span.start as usize && node.span.end == at.span.end as usize
}

fn edit(doc: &Doc, span: Range<usize>, text: String) -> TextEdit {
    TextEdit {
        doc: doc.id,
        span: span.start as u32..span.end as u32,
        text,
    }
}

fn line_start(source: &str, at: usize) -> usize {
    source[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn line_indent(source: &str, at: usize) -> &str {
    let before = &source[line_start(source, at)..at];
    &before[..before.len() - before.trim_start_matches([' ', '\t']).len()]
}

/// Insert on its own line, retaining an inline following statement or closing brace.
fn before(
    source: &str,
    at: usize,
    indent: &str,
    following: &str,
    statement: &str,
    newline: &str,
) -> (usize, String) {
    let start = line_start(source, at);
    if source[start..at].chars().all(|c| c == ' ' || c == '\t') {
        (start, format!("{indent}{statement}{newline}"))
    } else {
        (
            at,
            format!("{newline}{indent}{statement}{newline}{following}"),
        )
    }
}

/// Skip trailing comments before inserting after a statement, including multiline comments.
fn next_line(gap: &str) -> Option<usize> {
    let bytes = gap.as_bytes();
    let (mut at, mut depth, mut line_comment) = (0, 0usize, false);
    while at < bytes.len() {
        if depth == 0 && bytes[at] == b'\n' {
            return Some(at + 1);
        }
        if !line_comment && bytes[at..].starts_with(b"/*") {
            depth += 1;
            at += 2;
        } else if depth > 0 && bytes[at..].starts_with(b"*/") {
            depth -= 1;
            at += 2;
        } else if depth == 0 && bytes[at..].starts_with(b"//") {
            line_comment = true;
            at += 2;
        } else {
            at += 1;
        }
    }
    None
}

fn removal_span(source: &str, stmt: &Range<usize>) -> Range<usize> {
    let start = line_start(source, stmt.start);
    let end = source[stmt.end..]
        .find('\n')
        .map_or(source.len(), |i| stmt.end + i + 1);
    let tail = &source[stmt.end..end];
    let rest = tail.trim_start_matches([' ', '\t', ';', '\r']);
    if source[start..stmt.start]
        .chars()
        .all(|c| c == ' ' || c == '\t')
        && (rest.is_empty() || rest == "\n" || rest.starts_with("//"))
    {
        return start..end;
    }
    let trimmed = tail.trim_start_matches([' ', '\t']);
    let end = if trimmed.starts_with(';') {
        stmt.end + tail.len() - trimmed.len() + 1
    } else {
        stmt.end
    };
    stmt.start..end
}
