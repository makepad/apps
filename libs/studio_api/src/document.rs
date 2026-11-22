//! The document tools' common, source-preserving patch format. An edit names
//! a parsed Splash node (a value, call, object, `let` statement, or one
//! `name: value` entry) by its exact text, `old`, optionally with the `line`
//! it starts on; the patch replaces that node's span and nothing else. A text
//! that is not a whole node, or that names several nodes, is refused with
//! where to look, so a stale or partial edit never lands. Programs that
//! already hold a node may name its byte `span` directly.
use makepad_script::syntax::{self, edit::Replace};
use makepad_strict_json::{self as json, Value as Json};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Edit {
    /// The exact text of the node to replace (for a `span` edit: the text
    /// expected there, to refuse a stale edit).
    pub old: String,
    /// The replacement Splash code.
    pub text: String,
    /// The 1-based line the node starts on, when `old` alone names several.
    pub line: Option<usize>,
    /// A node's byte span, for callers that hold the parsed node.
    pub span: Option<Range<usize>>,
}

impl Edit {
    /// An edit of a node a program holds: its span and its current text.
    pub fn at(span: Range<usize>, old: impl Into<String>, text: impl Into<String>) -> Self {
        Edit { old: old.into(), text: text.into(), line: None, span: Some(span) }
    }
}

/// The `edits` argument of `doc_patch`: `[{old, new, line?}]`.
pub fn edits(args: &Json) -> Result<Vec<Edit>, String> {
    let items = args.get("edits").and_then(Json::as_arr).ok_or("give `edits`: [{\"old\": exact text of the node to replace, \"new\": its replacement}]")?;
    if items.is_empty() {
        return Err("`edits` is empty: give at least one {\"old\", \"new\"}".into());
    }
    items.iter().enumerate().map(|(index, value)| {
        let text = |key: &str| value.get(key).and_then(Json::as_str).map(str::to_string);
        let old = text("old").filter(|old| !old.trim().is_empty()).ok_or_else(|| format!("edits[{index}] needs `old`: the exact text to replace, copied from doc_read"))?;
        let new = text("new").ok_or_else(|| format!("edits[{index}] needs `new`: the replacement text (\"\" removes `old`)"))?;
        let line = value.get("line").and_then(Json::as_u64).map(|line| line as usize);
        Ok(Edit { old, text: new, line, span: None })
    }).collect()
}

/// The places an edit can name: every parsed node, and every `name: value`
/// entry of an object, deduplicated, in source order.
fn targets(syntax: &syntax::Syntax) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    syntax.walk(&mut |_, node| {
        ranges.push(node.span.clone());
        if let Some((_, fields)) = node.object() {
            for field in fields {
                ranges.push(field.span.clone());
            }
        }
    });
    ranges.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
    ranges.dedup();
    ranges
}

/// Whole `//` comment lines that read exactly `old`: comments are not
/// nodes, but a comment that says what the code does changes with it.
fn comment_lines(source: &str, old: &str) -> Vec<Range<usize>> {
    if old.is_empty() || !old.lines().all(|line| line.trim_start().starts_with("//")) {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find(old).map(|at| at + from) {
        let end = at + old.len();
        let line_start = source[..at].rfind('\n').map_or(0, |n| n + 1);
        let at_line_start = source[line_start..at].trim().is_empty();
        let at_line_end = source[end..].starts_with('\n') || end == source.len();
        if at_line_start && at_line_end {
            found.push(at..end);
        }
        from = at + 1;
    }
    found
}

/// Whitespace runs as one space: `y: 500` names `y:  500`.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn snippet(text: &str) -> String {
    let one = squash(text);
    if one.chars().count() > 120 { format!("{}…", one.chars().take(120).collect::<String>()) } else { one }
}

/// The span `edit` names in `source`, or why it names none or several.
fn resolve(syntax: &syntax::Syntax, source: &str, ranges: &[Range<usize>], edit: &Edit) -> Result<Range<usize>, String> {
    if let Some(span) = &edit.span {
        if !ranges.contains(span) { return Err(format!("{}..{} is not a parsed node span; read the file again", span.start, span.end)); }
        if source.get(span.clone()) != Some(edit.old.as_str()) { return Err("the parsed span changed; read the file again".into()); }
        return Ok(span.clone());
    }
    let old = edit.old.trim();
    let line_of = |range: &Range<usize>| syntax.line_col(range.start).0;
    let last_line_of = |range: &Range<usize>| syntax.line_col(range.end.saturating_sub(1).max(range.start)).0;
    let mut found: Vec<Range<usize>> = ranges.iter().filter(|range| &source[(*range).clone()] == old).cloned().collect();
    if found.is_empty() {
        let want = squash(old);
        found = ranges.iter().filter(|range| squash(&source[(*range).clone()]) == want).cloned().collect();
    }
    if found.is_empty() {
        found = comment_lines(source, old);
    }
    // Nodes that are the same text twice nested (a call and its only
    // argument never are; a paren and its item can be): the outer one.
    found.dedup_by(|inner, outer| outer.start <= inner.start && inner.end <= outer.end && source[inner.clone()].trim() == source[outer.clone()].trim());
    if let Some(line) = edit.line {
        // The line it starts on, else a line it spans (the line of its
        // enclosing `Text{` is a natural thing to give).
        let mut at_line: Vec<Range<usize>> = found.iter().filter(|range| line_of(range) == line).cloned().collect();
        if at_line.is_empty() {
            at_line = found.iter().filter(|range| line_of(range) <= line && line <= last_line_of(range)).cloned().collect();
        }
        if at_line.is_empty() {
            // Inside a node that starts on that line (`line` of `Text{` for
            // its `opacity: tween(…)` a line below).
            let parents: Vec<&Range<usize>> = ranges.iter().filter(|range| line_of(range) == line && last_line_of(range) > line).collect();
            at_line = found.iter().filter(|range| parents.iter().any(|parent| parent.start <= range.start && range.end <= parent.end)).cloned().collect();
        }
        if at_line.is_empty() && !found.is_empty() {
            let lines: Vec<String> = found.iter().map(|range| line_of(range).to_string()).collect();
            return Err(format!("`{}` is not a node starting on line {line}; it starts on line {}", snippet(old), lines.join(", ")));
        }
        found = at_line;
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => match source.find(old) {
            Some(at) => {
                // Part of a node: name the smallest whole node around it,
                // unless that is a whole statement list (a comment or the
                // space between statements).
                let around = ranges.iter().filter(|range| range.start <= at && at + old.len() <= range.end).min_by_key(|range| range.end - range.start);
                let in_comment = source[source[..at].rfind('\n').map_or(0, |n| n + 1)..at].contains("//") || old.starts_with("//");
                match around {
                    _ if in_comment => Err(format!("`{}` is in a comment; a comment is edited as whole `//` lines: give `old` as the complete comment line(s)", snippet(old))),
                    Some(range) if last_line_of(range) - line_of(range) < 40 => Err(format!(
                        "`{}` is only part of a node; `old` must be a whole value, call, `name: value` entry or `let`. The smallest whole node there (line {}): `{}`",
                        snippet(old), line_of(range), snippet(&source[range.clone()])
                    )),
                    _ => Err(format!("`{}` is not a whole node (it spans parts of several statements); edit one value, call, `name: value` entry or `let` at a time", snippet(old))),
                }
            }
            None => Err(format!("`{}` is not in the file (it changed, or the text differs); doc_read it again and copy `old` exactly", snippet(old))),
        },
        n => {
            let lines: Vec<String> = found.iter().map(|range| line_of(range).to_string()).collect();
            Err(format!("`{}` is in {n} places (lines {}): add `line` with the line it starts on, or a longer `old`", snippet(old), lines.join(", ")))
        }
    }
}

pub fn patch(source: &str, edits: &[Edit]) -> Result<String, String> {
    let syntax = syntax::parse(source).map_err(|e| e.to_string())?;
    let ranges = targets(&syntax);
    let mut replacements = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        let range = resolve(&syntax, source, &ranges, edit).map_err(|why| if edits.len() > 1 { format!("edits[{index}]: {why}") } else { why })?;
        replacements.push(Replace::new(range, &edit.text));
    }
    syntax.apply(&replacements).map(|next| next.print()).map_err(|e| e.to_string())
}

/// Whole-document writes still enter the syntax patcher at its root span.
pub fn write(source: &str, text: &str) -> Result<String, String> {
    let syntax = syntax::parse(source).map_err(|e| e.to_string())?;
    syntax.apply(&[Replace::new(0..source.len(), text)]).map(|next| next.print()).map_err(|e| e.to_string())
}

/// The most text one `doc_read` answer carries: the chat's result budget is
/// 16 KB, and the JSON around the text needs room.
pub const PAGE_BYTES: usize = 11 * 1024;

/// One page of `source` from 1-based line `from`: whole lines up to
/// [`PAGE_BYTES`], and the line the next page starts on (none at the end).
pub fn page(source: &str, from: usize) -> (String, usize, usize, Option<usize>) {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let total = lines.len().max(1);
    let first = from.clamp(1, total);
    let mut text = String::new();
    let mut last = first - 1;
    for line in &lines[first - 1..] {
        if !text.is_empty() && text.len() + line.len() > PAGE_BYTES { break; }
        text.push_str(line);
        last += 1;
    }
    (text, first, last, (last < lines.len()).then_some(last + 1))
}

/// The top of a Splash file's structure: each `let` and each object with an
/// `name: @id` (a deck's `Slide{name: @open ...}`), with the line it starts
/// on and the `graphic("…")` files it references. Lets a model find what to
/// read without reading every line.
pub fn outline(source: &str) -> Result<Json, String> {
    let syntax = syntax::parse(source).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let graphics = |node: &syntax::Node| {
        let mut out: Vec<Json> = Vec::new();
        node.walk(&Default::default(), &mut |_, inner| {
            if let Some(path) = inner.field("graphic").and_then(|field| field.value.string()) {
                let path = json::s(path);
                if !out.contains(&path) { out.push(path); }
            }
            if let Some((callee, args)) = inner.as_call() {
                if callee == "graphic" {
                    if let Some(path) = args.first().and_then(|arg| arg.string()) {
                        let path = json::s(path);
                        if !out.contains(&path) { out.push(path); }
                    }
                }
            }
        });
        out
    };
    for statement in syntax.statements() {
        if let Some(name) = statement.let_name() {
            rows.push(json::obj(vec![("line", Json::Int(syntax.line_col(statement.span.start).0 as i64)), ("let", json::s(name))]));
        }
    }
    syntax.walk(&mut |_, node| {
        let Some((ty, _)) = node.object() else { return };
        let Some(id) = node.field("name").and_then(|field| field.value.name().map(str::to_string).or_else(|| field.value.string().map(str::to_string))) else { return };
        let mut fields = vec![("line", Json::Int(syntax.line_col(node.span.start).0 as i64)), ("object", json::s(ty.unwrap_or(""))), ("name", json::s(id))];
        let used = graphics(node);
        if !used.is_empty() { fields.push(("graphics", Json::Arr(used))); }
        rows.push(json::obj(fields));
    });
    Ok(Json::Arr(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "let look = import(\"shared/look\")\nlet A = Title{text: \"Hello\" span: [0.0, 6.0] font_size: 100.0}\nlet B = Title{text: \"Hello\" span: [1.0, 6.0] font_size: 100.0}\nDeck{\n    slides: [\n        Slide{name: @intro cut: graphic(\"slides/intro\")}\n    ]\n}\n";

    fn edit(old: &str, new: &str, line: Option<usize>) -> Edit {
        Edit { old: old.into(), text: new.into(), line, span: None }
    }

    #[test]
    fn an_edit_names_a_whole_node_by_its_text() {
        let out = patch(DOC, &[edit("span: [1.0, 6.0]", "span: [2.0, 6.0]", None)]).unwrap();
        assert!(out.contains("span: [2.0, 6.0]") && out.contains("span: [0.0, 6.0]"));
        // Whitespace inside `old` need not match exactly.
        let out = patch(DOC, &[edit("font_size:  100.0}", "x", None)]).err().unwrap();
        assert!(out.contains("part of a node") || out.contains("not in the file"), "{out}");
    }

    #[test]
    fn several_matches_ask_for_a_line_and_the_line_picks_one() {
        let error = patch(DOC, &[edit("\"Hello\"", "\"Hi\"", None)]).err().unwrap();
        assert!(error.contains("2 places (lines 2, 3)") && error.contains("`line`"), "{error}");
        let out = patch(DOC, &[edit("\"Hello\"", "\"Hi\"", Some(3))]).unwrap();
        assert!(out.contains("let B = Title{text: \"Hi\"") && out.contains("let A = Title{text: \"Hello\""));
    }

    #[test]
    fn part_of_a_node_names_the_whole_node_around_it() {
        let error = patch(DOC, &[edit("ell", "x", None)]).err().unwrap();
        assert!(error.contains("only part of a node") && error.contains("\"Hello\""), "{error}");
        let error = patch(DOC, &[edit("Goodbye", "x", None)]).err().unwrap();
        assert!(error.contains("not in the file"), "{error}");
    }

    #[test]
    fn whole_comment_lines_are_edited_and_a_line_inside_a_node_picks_it() {
        let doc = "use mod.motion.*\n\n// one soft cyan for structure\n// and apricot\nlet COOL = #74d4f2\nlet A = Text{\n    o: tween(0, 1)\n}\nlet B = Text{\n    o: tween(0, 1)\n}\n";
        let out = patch(doc, &[edit("// one soft cyan for structure", "// one soft green for structure", None), edit("#74d4f2", "#8fd474", None)]).unwrap();
        assert!(out.contains("// one soft green for structure\n// and apricot") && out.contains("#8fd474"), "{out}");
        let error = patch(doc, &[edit("soft cyan", "x", None)]).err().unwrap();
        assert!(error.contains("in a comment"), "{error}");
        // `line` 9 is where B's `Text{` starts; its tween is on line 10.
        let out = patch(doc, &[edit("tween(0, 1)", "tween(0, 2)", Some(9))]).unwrap();
        assert!(out.contains("let B = Text{\n    o: tween(0, 2)") && out.contains("let A = Text{\n    o: tween(0, 1)"), "{out}");
        let out = patch(doc, &[edit("o: tween(0, 1)", "o: tween(1, 2)", Some(10))]).unwrap();
        assert!(out.contains("let B = Text{\n    o: tween(1, 2)") && out.contains("let A = Text{\n    o: tween(0, 1)"), "{out}");
    }

    #[test]
    fn edits_parse_from_old_and_new() {
        let args = makepad_strict_json::parse(br#"{"edits": [{"old": "1.0", "new": "2.0", "line": 3}]}"#).unwrap();
        let edits = edits(&args).unwrap();
        assert_eq!((edits[0].old.as_str(), edits[0].text.as_str(), edits[0].line), ("1.0", "2.0", Some(3)));
        let args = makepad_strict_json::parse(br#"{"edits": [{"start": 1, "end": 2, "text": "x"}]}"#).unwrap();
        assert!(super::edits(&args).err().unwrap().contains("`old`"));
    }

    #[test]
    fn pages_end_on_whole_lines_and_the_outline_lists_lets_and_slides() {
        let big: String = (0..2000).map(|i| format!("let v{i} = {i}\n")).collect();
        let (text, first, last, next) = page(&big, 1);
        assert!(first == 1 && text.len() <= PAGE_BYTES && text.ends_with('\n') && next == Some(last + 1));
        let (_, _, last2, next2) = page(&big, next.unwrap());
        assert!(last2 > last && (next2.is_none() || next2.unwrap() == last2 + 1));
        let outline = outline(DOC).unwrap().to_json();
        assert!(outline.contains(r#""let":"look""#) && outline.contains(r#""name":"intro""#) && outline.contains(r#""graphics":["slides/intro"]"#), "{outline}");
    }
}
