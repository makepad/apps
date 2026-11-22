//! Value arguments, parameter defaults and source edits for quantity changes.

use crate::resolve;
use crate::units::{circuit_units, literal_text, qty_of};
use fab_elec_circuit::Quantity;
use makepad_script::syntax::{self, Kind, Node, Syntax};
use makepad_script::tokenizer::lex_with;
use makepad_script::units::UnitTable;
use std::ops::Range;

/// Where a value argument is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueArg {
    /// A number literal, including its unit suffix.
    Literal(Range<usize>),
    /// A field of the enclosing block build function's second parameter.
    Param {
        /// The parameter field's name.
        name: String,
        /// The whole field access as written.
        span: Range<usize>,
    },
    /// An expression with no directly editable value.
    Expr(Range<usize>),
}

/// Find the value argument of the builder call at `call`.
/// Resistors, capacitors and inductors use argument zero; parts use the
/// option object's `value` field. Other calls have no value argument.
pub fn value_arg(syntax: &Syntax, call: Range<usize>) -> Option<ValueArg> {
    let node = call_at(syntax, &call)?;
    let (callee, args) = node.as_call()?;
    let value = match callee {
        "e.r" | "e.c" | "e.l" => args.first()?,
        "e.part" => &args.get(1)?.field("value")?.value,
        "e.led" => &args.first()?.field("color")?.value,
        _ => return None,
    };
    if matches!(value.kind, Kind::Number(_) | Kind::Str(_) | Kind::Name(_)) {
        return Some(ValueArg::Literal(value.span.clone()));
    }
    if let Some(name) = param_name(syntax, value) {
        return Some(ValueArg::Param {
            name: name.to_string(),
            span: value.span.clone(),
        });
    }
    Some(ValueArg::Expr(value.span.clone()))
}

/// A field of the enclosing block build function's second parameter.
pub(crate) fn param_name<'a>(syntax: &Syntax, value: &'a Node) -> Option<&'a str> {
    let (base, name) = value.ident()?.split_once('.')?;
    (!name.is_empty() && !name.contains('.') && build_param(syntax, &value.span) == Some(base)).then_some(name)
}

/// Argument position of net `index` in a two-terminal call. The native determines the shift
/// from the evaluated second argument and the pin map preserves it for later source edits.
pub(crate) fn two_pin_net_arg(index: usize, positional_tol: bool) -> usize {
    index + 1 + usize::from(positional_tol)
}

/// A tolerance bound's syntax. `Some("")` is a bare number; `None` is an expression.
pub(crate) struct TolItem {
    pub span: Range<usize>,
    pub suffix: Option<String>,
}

/// A symmetric tolerance argument or an options field containing two signed bounds.
pub(crate) struct TolArg {
    pub span: Range<usize>,
    pub items: Vec<TolItem>,
}

/// Find the written tolerance and its literal suffix(es). Use the evaluated argument shift,
/// since a variable can hold either a tolerance, a net handle or an options object.
pub(crate) fn tol_arg(syntax: &Syntax, call: Range<usize>, positional_tol: bool) -> Option<TolArg> {
    let (callee, args) = call_at(syntax, &call)?.as_call()?;
    if !matches!(callee, "e.r" | "e.c" | "e.l") {
        return None;
    }
    let value = if positional_tol {
        args.get(1)?
    } else {
        &args.get(two_pin_net_arg(1, false) + 1)?.field("tol")?.value
    };
    let units = circuit_units();
    let item = |node: &Node| {
        let literal = match &node.kind {
            Kind::Number(_) => Some(node),
            Kind::Unary { op, operand } if op == "-" && matches!(operand.kind, Kind::Number(_)) => Some(operand.as_ref()),
            _ => None,
        };
        let suffix = literal.map(|literal| {
            let written = syntax.text(literal);
            units.entries().iter().find(|entry| {
                written.strip_suffix(entry.suffix.as_str()).is_some_and(|digits| digits.parse::<f64>().is_ok())
            }).map_or_else(String::new, |entry| entry.suffix.clone())
        });
        TolItem { span: node.span.clone(), suffix }
    };
    let items = match &value.kind {
        Kind::Array { items, .. } => items.iter().map(item).collect(),
        _ => vec![item(value)],
    };
    Some(TolArg { span: value.span.clone(), items })
}

/// Convert a fractional tolerance to percent, removing decimal conversion noise.
pub(crate) fn tolerance_percent(fraction: f64) -> f64 {
    let percent = fraction * 100.0;
    if percent.abs() < f64::MAX / 1e9 { (percent * 1e9).round() / 1e9 } else { percent }
}

/// Find the default literal (a number, a string or a `@name`) for `param` in the block call's
/// `params` object.
/// Computed defaults and missing fields have no literal span.
pub fn param_default(
    syntax: &Syntax,
    block_call: Range<usize>,
    param: &str,
) -> Option<Range<usize>> {
    let value = &call_at(syntax, &block_call)?
        .call("e.block")?
        .first()?
        .field("params")?
        .value
        .field(param)?
        .value;
    matches!(value.kind, Kind::Number(_) | Kind::Str(_) | Kind::Name(_)).then(|| value.span.clone())
}

/// Set an override in a call's options object (the first argument of a block instance,
/// the last argument of a two-terminal builder). A builder without options gains an object.
/// Replace an existing field's value, or append a field using the object's
/// separators and last field's indentation. Return the replacement span and text.
pub fn override_edit(
    syntax: &Syntax,
    instance_call: Range<usize>,
    param: &str,
    literal: &str,
) -> Option<(Range<usize>, String)> {
    let call = call_at(syntax, &instance_call)?;
    let Kind::Call { args, close, .. } = &call.kind else {
        return None;
    };
    let object = if matches!(call.callee_path(), Some("e.r" | "e.c" | "e.l")) {
        match args.last().filter(|n| matches!(n.kind, Kind::Object { .. })) {
            Some(object) => object,
            None => {
                let end = args.last()?.span.end;
                let separator = if has_comma(&syntax.source()[end..*close], &circuit_units()) { " " } else { ", " };
                return Some((*close..*close, format!("{separator}{{{}: {literal}}}", syntax::write::key(param).ok()?)));
            }
        }
    } else {
        args.first()?
    };
    let Kind::Object { fields, open, close, .. } = &object.kind else {
        return None;
    };
    if let Some(field) = object.field(param) {
        return Some((field.value.span.clone(), literal.to_string()));
    }
    let key = syntax::write::key(param).ok()?;
    let field_text = format!("{key}: {literal}");
    let source = syntax.source();
    let start = fields.last().map_or(open + 1, |field| field.span.end);
    let tail = &source[start..*close];
    let units = circuit_units();
    let trailing_comma = has_comma(tail, &units);
    let commas = trailing_comma || fields.windows(2).any(|pair| {
        has_comma(&source[pair[0].span.end..pair[1].span.start], &units)
    });
    let mut text = String::new();
    if commas && !trailing_comma && !fields.is_empty() {
        text.push(',');
    }
    let trailing = if trailing_comma { "," } else { "" };
    if source[open + 1..*close].contains('\n') {
        let newline = if source[open + 1..*close].contains("\r\n") { "\r\n" } else { "\n" };
        let indent = fields.last().map_or_else(
            || format!("{}    ", line_indent(source, *open)),
            |field| line_indent(source, field.span.start).to_string(),
        );
        let line_start = source[..*close].rfind('\n').map_or(0, |at| at + 1);
        let closing_indent = &source[line_start..*close];
        if line_start >= start && closing_indent.chars().all(|ch| ch == ' ' || ch == '\t') {
            text.push_str(&source[start..line_start]);
            text.push_str(&format!("{indent}{field_text}{trailing}{newline}{closing_indent}"));
        } else {
            text.push_str(tail);
            text.push_str(&format!("{newline}{indent}{field_text}{trailing}"));
        }
    } else {
        text.push_str(tail);
        if !tail.ends_with([' ', '\t']) {
            text.push(' ');
        }
        text.push_str(&field_text);
        text.push_str(trailing);
    }
    Some((start..*close, text))
}

/// Write an exact quantity literal, keeping the written suffix when it gives
/// a coefficient with magnitude at least one and at most four significant digits.
/// Zero also keeps its suffix; other values use the prefix from [`literal_text`].
///
/// # Panics
/// Panics if the SI value is not finite.
pub fn literal_for(q: &Quantity, written: &str) -> String {
    assert!(q.si.is_finite(), "quantity literal requires a finite SI value");
    let units = circuit_units();
    let written = written.trim();
    let entry = units.entries().iter().find(|entry| {
        qty_of(&entry.suffix).is_some_and(|(unit, _, _)| unit == q.unit)
            && written.strip_suffix(entry.suffix.as_str())
                .is_some_and(|digits| digits.parse::<f64>().is_ok())
    });
    if let Some(entry) = entry {
        let scaled = q.si / entry.mul * entry.div;
        if scaled.is_finite() && (scaled == 0.0 || scaled.abs() >= 1.0) {
            let number = compact_decimal(scaled);
            // Rounding is allowed only when the SI bits stay the same.
            if entry.literal(&number).to_bits() == q.si.to_bits() {
                return format!("{number}{}", entry.suffix);
            }
        }
    }
    literal_text(q)
}

/// The call written exactly at `span`: the innermost such call on the path down to `span`
/// (`resolve::ancestors`, O(depth x log fan-out)). A native asks this once per component, so a
/// walk of the whole document here made a circuit's evaluation quadratic in its size.
pub(crate) fn call_at<'a>(syntax: &'a Syntax, span: &Range<usize>) -> Option<&'a Node> {
    resolve::ancestors(syntax, span).into_iter().rev()
        .find(|node| node.span == *span && matches!(node.kind, Kind::Call { .. }))
}

/// The second parameter's name of the innermost `e.block` build function whose body holds `call`,
/// found on the path down to `call` (`resolve::ancestors`) instead of a walk of the whole document.
fn build_param<'a>(syntax: &'a Syntax, call: &Range<usize>) -> Option<&'a str> {
    resolve::ancestors(syntax, call).into_iter().rev().find_map(|node| {
        let build = node.call("e.block")?.first()?.field("build")?;
        let Kind::Fn { params, body, .. } = &build.value.kind else { return None };
        let holds = body.span.start <= call.start && call.end <= body.span.end;
        holds.then(|| params.get(1).map(|param| param.name.as_str()))
    }).flatten()
}

fn has_comma(text: &str, units: &UnitTable) -> bool {
    lex_with(text, units).iter().any(|token| &text[token.span()] == ",")
}

fn line_indent(source: &str, at: usize) -> &str {
    let start = source[..at].rfind('\n').map_or(0, |at| at + 1);
    let line = &source[start..at];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

fn compact_decimal(value: f64) -> String {
    let scientific = format!("{value:.3e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("finite scientific notation");
    let exponent: i32 = exponent.parse().expect("decimal exponent");
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |rest| ("-", rest));
    let digits = mantissa.replace('.', "");
    let point = exponent + 1;
    let mut text = if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!("{sign}{digits}{}", "0".repeat(point as usize - digits.len()))
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    };
    if text.contains('.') {
        text.truncate(text.trim_end_matches('0').trim_end_matches('.').len());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_circuit::QtyUnit;

    fn call_span(syntax: &Syntax, prefix: &str) -> Range<usize> {
        let mut found = None;
        syntax.walk(&mut |_, node| {
            if matches!(node.kind, Kind::Call { .. }) && syntax.text(node).starts_with(prefix) {
                found = Some(node.span.clone());
            }
        });
        found.expect("the requested call exists")
    }

    fn block_span(syntax: &Syntax, name: &str) -> Range<usize> {
        let mut found = None;
        syntax.walk(&mut |_, node| {
            if node.let_name() == Some(name) {
                if let Kind::Let { value: Some(value), .. } = &node.kind {
                    if value.callee_path() == Some("e.block") {
                        found = Some(value.span.clone());
                    }
                }
            }
        });
        found.expect("the named block exists")
    }

    #[test]
    fn value_sites_and_exact_literals() {
        let units = circuit_units();
        let source = include_str!("../../fixtures/ledctl.ckt.splash");
        let tree = syntax::parse_with(source, &units).expect("the fixture parses with circuit units");
        for (prefix, expected) in [("e.r(o.rd,", "rd"), ("e.c(o.c_in,", "c_in")] {
            let value = value_arg(&tree, call_span(&tree, prefix));
            assert!(
                matches!(value, Some(ValueArg::Param { name, span })
                    if name == expected && source[span.clone()] == format!("o.{expected}")),
                "{expected}: a build parameter keeps its name and access span"
            );
        }
        for (prefix, expected) in [("e.c(100nF,", "100nF"), ("e.r(10kohm,", "10kohm")] {
            assert!(
                matches!(value_arg(&tree, call_span(&tree, prefix)),
                    Some(ValueArg::Literal(span)) if &source[span.clone()] == expected),
                "{expected}: a literal span includes the whole unit suffix"
            );
        }
        assert!(
            matches!(value_arg(&tree, call_span(&tree, "e.r(e.series(")),
                Some(ValueArg::Expr(span))
                    if &source[span.clone()] == "e.series((p.vdd.volts - d.vf) / o.current - o.sink, @E24)"),
            "a computed value keeps the whole expression span, including the sinking pin's resistance"
        );
        for (block, param, expected) in [
            ("usb_c_power", "rd", "5.1kohm"),
            ("ldo", "c_in", "1uF"),
        ] {
            let span = param_default(&tree, block_span(&tree, block), param)
                .expect("the block parameter has a literal default");
            assert_eq!(&source[span.clone()], expected, "{param}: default lookup selects its value");
            assert_eq!(span.start, source.find(expected).unwrap(), "{param}: default lookup selects the first matching literal");
        }
        let instance = call_span(&tree, "ldo({");
        let close = instance.end - 2;
        let (span, text) = override_edit(&tree, instance, "c_in", "2.2uF")
            .expect("an instance object accepts an override");
        assert_eq!(span, close..close, "a missing override inserts before the closing brace");
        assert_eq!(text, " c_in: 2.2uF", "a space-separated object keeps its separator style");
        let mut edited = source.to_string();
        edited.replace_range(span, &text);
        let first = syntax::parse_with(&edited, &units).expect("the first override edit parses");
        let instance = call_span(&first, "ldo({");
        let object = &call_at(&first, &instance).unwrap().as_call().unwrap().1[0];
        assert_eq!(object.object().unwrap().1.len(), 4, "the inserted override adds exactly one field");
        let (span, text) = override_edit(&first, instance, "c_in", "4.7uF")
            .expect("an existing override can be changed");
        assert_eq!(&edited[span.clone()], "2.2uF", "the second edit selects only the existing value");
        assert_eq!(text, "4.7uF", "the second edit writes only the replacement literal");
        edited.replace_range(span, &text);
        let second = syntax::parse_with(&edited, &units).expect("the second override edit parses");
        let instance = call_span(&second, "ldo({");
        let object = &call_at(&second, &instance).unwrap().as_call().unwrap().1[0];
        assert_eq!(object.object().unwrap().1.len(), 4, "replacing an override never duplicates its field");
        assert_eq!(second.text(&object.field("c_in").unwrap().value), "4.7uF", "the replacement is the new override value");

        for (input, expected) in [
            ("f({a: 1, b: 2})", "f({a: 1, b: 2, c: 3V})"),
            ("f({a: 1,})", "f({a: 1, c: 3V,})"),
            ("f({\n  a: 1\n  b: 2\n})", "f({\n  a: 1\n  b: 2\n  c: 3V\n})"),
            ("f({\n\ta: 1,\n\tb: 2 // keep, this\n})", "f({\n\ta: 1,\n\tb: 2, // keep, this\n\tc: 3V\n})"),
            ("f({a: 1 /* keep, this */})", "f({a: 1 /* keep, this */ c: 3V})"),
        ] {
            let tree = syntax::parse_with(input, &units).expect("the separator example parses");
            let (span, text) = override_edit(&tree, call_span(&tree, "f("), "c", "3V")
                .expect("the separator example accepts an override");
            let mut output = input.to_string();
            output.replace_range(span, &text);
            assert_eq!(output, expected, "override insertion preserves separators, indentation and comments");
            assert!(syntax::parse_with(&output, &units).is_ok(), "the preserved object style still parses");
        }

        let renamed = "let b = e.block({params: {v: 1H} build: fn(p, settings) { e.l(settings.v, a, b); e.part(part, {value: settings.v}); e.r(o.v, a, b) }})";
        let renamed = syntax::parse_with(renamed, &units).expect("the renamed build parameter parses");
        for prefix in ["e.l(", "e.part("] {
            assert!(matches!(value_arg(&renamed, call_span(&renamed, prefix)),
                Some(ValueArg::Param { name, .. }) if name == "v"),
                "{prefix}: parameter access uses the build function's actual second parameter");
        }
        assert!(matches!(value_arg(&renamed, call_span(&renamed, "e.r(")), Some(ValueArg::Expr(_))),
            "an unrelated name is not the build parameter");
        assert!(matches!(value_arg(&tree, call_span(&tree, "e.led(")), Some(ValueArg::Param { ref name, .. }) if name == "color"),
            "a light's value is its colour (here the block's colour parameter)");
        assert_eq!(value_arg(&tree, call_span(&tree, "e.part(usb_c_receptacle(")), None, "a part without a value field has no value argument");

        for (written, si, unit, expected) in [
            ("5.1kohm", 4700.0, QtyUnit::Ohm, "4.7kohm"),
            ("1uF", 2.2e-6, QtyUnit::Farad, "2.2uF"),
            ("100nF", 47e-9, QtyUnit::Farad, "47nF"),
            ("270ohm", 330.0, QtyUnit::Ohm, "330ohm"),
            ("10V", 12.0, QtyUnit::Volt, "12V"),
            ("10V", 12.3456, QtyUnit::Volt, "12.3456V"),
            ("4.7uF", 47e-9, QtyUnit::Farad, "47nF"),
        ] {
            let q = Quantity { si, unit };
            let text = literal_for(&q, written);
            assert_eq!(text, expected, "{written}: retain a compact exact suffix or use the canonical prefix");
            let tokens = lex_with(&text, &units);
            assert_eq!(tokens.len(), 1, "{text}: a positive quantity writes one literal token");
            assert_eq!(tokens[0].token().number_f64().map(f64::to_bits), Some(si.to_bits()),
                "{text}: reading the literal preserves every SI bit");
        }
    }
}
