use super::{At, Rect, Request, ThemeOp, ThemeVal, TreeQuery};
use makepad_strict_json::{self as json, Value};

use crate::app_link::{AppLinks, DesignOp, DesignVerb, LinkId, Ticket, WindowId};

/// The window owns polling G11. Tweaking submits design commands and one-shot window captures.
pub trait AppLink<L = LinkId, T = Ticket> {
    fn png_still(&mut self, id: L, window: WindowId) -> T;
    fn design(&mut self, id: L, window: Option<WindowId>, op: DesignOp) -> T;
}

impl AppLink for AppLinks {
    fn png_still(&mut self, id: LinkId, window: WindowId) -> Ticket {
        AppLinks::png_still(self, id, window)
    }
    fn design(&mut self, id: LinkId, window: Option<WindowId>, op: DesignOp) -> Ticket {
        AppLinks::design(self, id, window, op)
    }
}

pub(super) fn design(request: &Request, owner_window: Option<WindowId>) -> Option<(Option<WindowId>, DesignOp)> {
    let (mut window, mut args) = (None, Vec::new());
    let verb = match request {
        Request::Theme(op) => {
            match op {
                ThemeOp::Get => args.push(("get".into(), "1".into())),
                ThemeOp::Set { name, value } => args.extend([("set".into(), "1".into()),
                    ("name".into(), name.clone()), ("value".into(), value.clone())]),
                ThemeOp::Install(install) => {
                    let entries = |pairs: &[(String, ThemeVal)]| Value::Arr(pairs.iter().map(|(name, value)| {
                        let value = match value {
                            ThemeVal::Color(c) => json::s(format!("#{c:08x}")),
                            ThemeVal::Num(n) => Value::F64(*n),
                        };
                        Value::Arr(vec![json::s(name), value])
                    }).collect());
                    let spec = json::obj(vec![("base", json::s(&install.base)),
                        ("sheet", install.sheet.as_ref().map_or(Value::Null, json::s)),
                        ("globals", entries(&install.globals)), ("overrides", entries(&install.overrides))]);
                    args.extend([("install".into(), "1".into()), ("spec".into(), spec.to_json())]);
                }
            }
            DesignVerb::Theme
        }
        Request::Tree(query) => {
            use makepad_widgets::design_data::tree::{MAX_DEPTH, MAX_LIMIT};
            let bounded = |key, n: usize, max| {
                if n > max { makepad_widgets::log!("Town design tree: clamping {key} from {n} to {max}"); }
                (String::from(key), n.min(max).to_string())
            };
            let limit = match query {
                TreeQuery::Children { path, depth, offset, limit } => {
                    if let Some(p) = path { args.push(("path".into(), p.clone())); }
                    args.push(bounded("depth", usize::from(*depth), MAX_DEPTH));
                    args.push(("offset".into(), offset.to_string()));
                    limit
                }
                TreeQuery::Reveal { path, limit } => { args.push(("reveal".into(), path.clone())); limit }
                TreeQuery::Search { query, offset, limit } => {
                    args.push(("query".into(), query.clone()));
                    args.push(("offset".into(), offset.to_string()));
                    limit
                }
            };
            args.push(bounded("limit", usize::from(*limit), MAX_LIMIT));
            window = owner_window;
            DesignVerb::Tree
        }
        Request::Pick(At::Point { window: w, x, y }) => {
            window = Some(WindowId(*w));
            args.push(("at".into(), format!("{x},{y}")));
            DesignVerb::Pick
        }
        Request::Pick(At::Path(path)) | Request::Props { path } => {
            args.push(("path".into(), path.clone()));
            if matches!(request, Request::Props { .. }) { DesignVerb::Props } else { DesignVerb::Pick }
        }
        Request::Set { path, prop, value } => {
            args.extend([("path".into(), path.clone()), ("prop".into(), prop.clone()), ("value".into(), value.clone())]);
            DesignVerb::Typed
        }
        _ => return None,
    };
    Some((window, DesignOp::Design { verb, args }))
}

fn read_rect(window: usize, coords: &[Value]) -> Result<Rect, &'static str> {
    let [x, y, w, h] = coords else { return Err("invalid pick rect") };
    let number = |v: &Value| match v {
        Value::Int(n) => Ok(*n as f64), Value::F64(n) => Ok(*n), _ => Err("invalid pick coordinate"),
    };
    let rect = Rect { window, x: number(x)?, y: number(y)?, w: number(w)?, h: number(h)? };
    if rect.valid() { Ok(rect) } else { Err("invalid pick dimensions") }
}

fn window_index(v: &Value) -> Option<usize> { v.get("window")?.as_u64()?.try_into().ok() }

pub(super) fn picked(v: &Value) -> Result<(String, String, Rect), String> {
    let coords = v.get("rect").and_then(Value::as_arr).ok_or("missing pick rect")?;
    let rect = read_rect(window_index(v).ok_or("invalid pick window")?, coords)?;
    let string = |key| v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned).ok_or("missing pick field");
    // `ref` names the widget exactly, in the names a person reads (`/dock/card/View.2`). A dotted
    // `path` with unnamed (`-`) segments resolves to the nearest named ancestor instead.
    Ok((string("ref").or_else(|_| string("path"))?, string("type")?, rect))
}

/// An unnamed node's stand-in in a dotted pick path: `-`, or `-2` for a numbered one.
fn unnamed(segment: &str) -> bool {
    segment.strip_prefix('-').is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
}

/// The nearest named ancestor of a pick that landed on an unnamed widget, as an exact address:
/// its `ref` without the unnamed tail, or the dotted path without it from an app that sends no `ref`.
pub(super) fn named_parent(v: &Value) -> Option<String> {
    let path: Vec<&str> = v.get("path")?.as_str()?.split('.').collect();
    let tail = path.iter().rev().take_while(|s| unnamed(s)).count();
    if tail == 0 { return None }
    if let Some(exact) = v.get("ref").and_then(Value::as_str).filter(|r| !r.is_empty()) {
        let segments: Vec<&str> = exact.trim_start_matches('/').split('/').collect();
        return (segments.len() > tail).then(|| format!("/{}", segments[..segments.len() - tail].join("/")))
    }
    let named = &path[..path.len() - tail];
    named.iter().any(|s| !unnamed(s)).then(|| named.join("."))
}

pub(super) fn prop_row(row: &Value) -> Option<(&str, &str, Option<i64>)> {
    Some((row.get("name")?.as_str()?, row.get("value")?.as_str()?, row.get("level").and_then(Value::as_i64)))
}

/// The property names a props answer lists: the only ones a Set may name.
pub(super) fn row_names(v: &Value) -> std::collections::BTreeSet<String> {
    v.get("props").and_then(Value::as_arr).into_iter().flatten()
        .filter_map(|row| prop_row(row).map(|(name, _, _)| name.to_owned())).collect()
}

/// Validated shader function keys for this widget. A Set to one of these is a note.
pub(super) fn shader_fn_names(v: &Value) -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    for layer in v.get("shaders").and_then(Value::as_arr).into_iter().flatten() {
        let Some(name) = super::shader::layer_name(layer) else { continue };
        for f in layer.get("fns").and_then(Value::as_arr).into_iter().flatten() {
            if let Some(function) = super::shader::fn_name(f) {
                keys.insert(format!("{name}.{function}"));
            }
        }
    }
    keys
}

pub(super) fn rows(v: &Value) -> bool {
    v.get("props").and_then(Value::as_arr).is_some_and(|rows| rows.iter().all(|r| prop_row(r).is_some()))
}

pub(super) fn valid_theme(v: &Value) -> bool {
    super::theme::ThemeSnapshot::from_value(v).is_some()
}

pub(super) fn patch_cached_theme(snapshot: &mut String, deltas: &std::collections::HashMap<String, String>) {
    let Ok(Value::Obj(mut fields)) = super::parse_json(snapshot) else { return };
    for (field, value) in &mut fields {
        let Value::Arr(items) = value else { continue };
        match field.as_str() {
            "values" => for token in items {
                let Some(new) = token.get("name").and_then(Value::as_str).and_then(|n| deltas.get(n)) else { continue };
                if let Value::Obj(fields) = token {
                    if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == "value") { *v = json::s(new); }
                }
            }
            "edits" => {
                items.retain(|e| e.as_arr().and_then(|a| a.first()).and_then(Value::as_str).is_none_or(|k| !deltas.contains_key(k)));
                items.extend(deltas.iter().map(|(name, new)| {
                    let literal = new.strip_prefix('#').map_or_else(|| new.clone(), |c| format!("#x{}", c.to_ascii_uppercase()));
                    Value::Arr(vec![json::s(name), json::s(literal)])
                }));
            }
            _ => {}
        }
    }
    *snapshot = Value::Obj(fields).to_json();
}

pub(super) fn theme_applied(v: &Value) -> Result<(String, String, bool), String> {
    if v.get("ok") != Some(&Value::Bool(true)) { return Err("theme set failed".into()) }
    let text = |k| v.get(k).and_then(Value::as_str).ok_or("invalid theme delta");
    let flag = |k| v.get(k).and_then(Value::as_bool).ok_or("invalid theme delta");
    let (global, changed) = (flag("global")?, flag("changed")?);
    let (old, new) = (text("old")?, text("new")?);
    match (super::theme::scalar(old), super::theme::scalar(new)) {
        (Some(ThemeVal::Color(_)), Some(ThemeVal::Color(_))) | (Some(ThemeVal::Num(_)), Some(ThemeVal::Num(_))) =>
            Ok((old.into(), new.into(), global && changed)),
        _ => Err("invalid theme scalar delta".into()),
    }
}

pub(super) fn theme_label_of(v: &Value) -> String {
    super::theme::ThemeSnapshot::from_value(v).map_or_else(String::new, |s| s.label())
}

pub(super) fn valid_tree(v: &Value) -> bool {
    let Some(window) = window_index(v) else { return false };
    let count = |v: &Value, key| v.get(key).and_then(Value::as_u64).is_some();
    let node = |n: &Value| {
        ["path", "name", "ty"].iter().all(|k| n.get(k).and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_control)))
            && count(n, "children") && n.get("visible").and_then(Value::as_bool).is_some()
            && n.get("matched").is_none_or(|m| m.as_bool().is_some())
            && n.get("rect").is_none_or(|r| r.as_arr().is_some_and(|coords| read_rect(window, coords).is_ok()))
    };
    matches!(v.get("mode").and_then(Value::as_str), Some("children" | "query" | "reveal"))
        && count(v, "total_nodes") && count(v, "total_matches") && v.get("truncated").and_then(Value::as_bool).is_some()
        && v.get("parents").and_then(Value::as_arr).is_some_and(|parents| parents.iter().all(|p| {
            p.get("path").and_then(Value::as_str).is_some() && count(p, "offset") && count(p, "total")
                && p.get("nodes").and_then(Value::as_arr).is_some_and(|nodes| nodes.iter().all(node))
        }))
}

pub(super) fn tree_rects(v: &Value) -> Vec<(String, Rect)> {
    let Some(window) = window_index(v) else { return Vec::new() };
    v.get("parents").and_then(Value::as_arr).into_iter().flatten()
        .filter_map(|p| p.get("nodes")?.as_arr()).flatten().filter_map(|n| {
            let rect = read_rect(window, n.get("rect")?.as_arr()?).ok()?;
            let path = n.get("path")?.as_str()?;
            (rect.w > 0. && rect.h > 0.).then(|| (path.to_owned(), rect))
        }).collect()
}

pub(super) fn applied(v: &Value, prop: &str) -> Result<(String, String), String> {
    let change = |key| -> Result<String, String> {
        let map = v.get(key).ok_or("missing apply delta")?;
        let Value::Obj(fields) = map else { return Err("invalid apply delta".into()) };
        if fields.iter().any(|(_, v)| !matches!(v, Value::Str(_) | Value::Null)) { return Err("invalid reflected value".into()) }
        // Composite properties can change several reflected leaves. Preserve those exact deltas.
        Ok(match fields.as_slice() {
            [(name, json::Value::Str(s))] if name == prop => s.clone(),
            _ => map.to_json(),
        })
    };
    Ok((change("old")?, change("new")?))
}
