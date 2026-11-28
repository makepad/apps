//! AI tools for a live shared App. Consent and execution belong to its owner session.
use super::{patch, session::validate_design_inputs, DesignStateView};
use crate::tools::{only_keys, optional};
use crate::tweak::{parse_json, widget_name, wire, BakeScope, DesignNotice, DesignPlace, DesignReplyKind,
    DesignRequest, Notice, Request, StepDir};
use makepad_ai_services::wire::{Risk, ServiceManifest, ToolDef, ToolResult, MAX_RESULT_BYTES};
use makepad_strict_json::{self as json, Value};

const TOOLS: &[(&str, &str, &str, Risk)] = &[
    ("app_design_state", "Read structural editing status. Supply a widget path to locate its source and Rust references.",
        r#"{"type":"object","required":["binding","item"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"}}}"#, Risk::Read),
    ("app_design_palette", "List widget templates by name or group. Pass next_offset as offset to read another page.",
        r#"{"type":"object","required":["binding","item"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}}}"#, Risk::Read),
    ("app_design_insert", "Insert a widget before, inside or after the widget at path. Props map property names to literal Splash strings.",
        r#"{"type":"object","required":["binding","item","path","place","ty"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"place":{"type":"string","enum":["before","inside","after"]},"ty":{"type":"string"},"name":{"type":"string"},"props":{"type":"object","additionalProperties":{"type":"string"}}}}"#, Risk::Act),
    ("app_design_move", "Move a widget using either target and place together, or a direction relative to its current position.",
        r#"{"type":"object","required":["binding","item","path"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"target":{"type":"string"},"place":{"type":"string","enum":["before","inside","after"]},"direction":{"type":"string","enum":["up","down","out"]}}}"#, Risk::Act),
    ("app_design_delete", "Delete a widget. Inspect any reported Rust references before forcing the edit.",
        r#"{"type":"object","required":["binding","item","path"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"force_rust_refs":{"type":"boolean"}}}"#, Risk::Act),
    ("app_design_duplicate", "Duplicate a widget beside the original, optionally naming the copy.",
        r#"{"type":"object","required":["binding","item","path"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"name":{"type":"string"}}}"#, Risk::Act),
    ("app_design_wrap", "Wrap a widget in a new container of the specified type.",
        r#"{"type":"object","required":["binding","item","path","ty"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"ty":{"type":"string"},"name":{"type":"string"}}}"#, Risk::Act),
    ("app_design_rename", "Rename a widget. Inspect any reported Rust references before forcing the edit.",
        r#"{"type":"object","required":["binding","item","path","name"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"path":{"type":"string"},"name":{"type":"string"},"force_rust_refs":{"type":"boolean"}}}"#, Risk::Act),
    ("app_design_bake", "Bake live property edits, theme edits, or both into the working preview. Optionally filter by widget path and property.",
        r#"{"type":"object","required":["binding","item","scope"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"scope":{"type":"string","enum":["props","theme","all"]},"path":{"type":"string"},"prop":{"type":"string"}}}"#, Risk::Act),
    ("app_design_history", "Undo or redo a structural edit, or reset the working preview.",
        r#"{"type":"object","required":["binding","item","op"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"op":{"type":"string","enum":["undo","redo","reset"]}}}"#, Risk::Act),
    ("app_design_patch", "Read a unified diff (optional file, byte offset and limit), write a .patch under local/design (optional file), or verify the preview (optional widget path). Continue a diff with next_offset.",
        r#"{"type":"object","required":["binding","item","op"],"additionalProperties":false,"properties":{"binding":{"type":"string"},"item":{"type":"string"},"op":{"type":"string","enum":["diff","write_patch","verify"]},"file":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}}}"#, Risk::Act),
];

pub(crate) fn register(mut manifest: ServiceManifest) -> ServiceManifest {
    for &(name, description, parameters, risk) in TOOLS {
        manifest = manifest.with_tool(ToolDef::new(name, description, parameters, risk));
    }
    manifest
}

pub(crate) fn recognizes(name: &str) -> bool {
    TOOLS.iter().any(|&(tool, ..)| tool == name)
}

pub(crate) fn parse(name: &str, args: &Value) -> Result<Request, String> {
    use DesignRequest::*;
    let string = |key| optional(args, key, |v| v.as_str().map(str::to_owned));
    let text = |key| string(key)?.ok_or_else(|| format!("expected text for {key}"));
    let place = || match text("place")?.as_str() {
        "before" => Ok(DesignPlace::Before),
        "inside" => Ok(DesignPlace::Inside),
        "after" => Ok(DesignPlace::After),
        _ => Err(String::from("place must be before, inside or after")),
    };
    let offset = || optional(args, "offset", |v| u32::try_from(v.as_u64()?).ok()).map(|n| n.unwrap_or(0));
    let limit = || optional(args, "limit", |v| v.as_u64().filter(|n| *n > 0));
    let force = || optional(args, "force_rust_refs", Value::as_bool).map(|v| v.unwrap_or(false));
    let req = match name {
        "app_design_state" => {
            only_keys(args, &["binding", "item", "path"])?;
            State { path: string("path")? }
        }
        "app_design_palette" => {
            only_keys(args, &["binding", "item", "query", "offset", "limit"])?;
            let max = u64::from(wire::MAX_PALETTE_LIMIT);
            Palette { query: string("query")?, offset: offset()?, limit: limit()?.unwrap_or(max).min(max) as u16 }
        }
        "app_design_insert" => {
            only_keys(args, &["binding", "item", "path", "place", "ty", "name", "props"])?;
            let mut props = Vec::new();
            if let Some(value) = args.get("props") {
                let Value::Obj(entries) = value else { return Err("props must be an object of Splash literals".into()) };
                for (k, v) in entries {
                    props.push((k.clone(), v.as_str().ok_or_else(|| format!("expected Splash text for property {k}"))?.into()));
                }
            }
            Insert { path: text("path")?, place: place()?, ty: text("ty")?, name: string("name")?, props }
        }
        "app_design_move" => {
            only_keys(args, &["binding", "item", "path", "target", "place", "direction"])?;
            let path = text("path")?;
            match (string("target")?, args.get("place"), string("direction")?) {
                (Some(target), Some(_), None) => Move { path, target, place: place()? },
                (None, None, Some(direction)) => Step { path, dir: match direction.as_str() {
                    "up" => StepDir::Up, "down" => StepDir::Down, "out" => StepDir::Out,
                    _ => return Err("direction must be up, down or out".into()),
                } },
                _ => return Err("move requires either target and place together, or direction".into()),
            }
        }
        "app_design_delete" => {
            only_keys(args, &["binding", "item", "path", "force_rust_refs"])?;
            Delete { path: text("path")?, force_rust_refs: force()? }
        }
        "app_design_duplicate" => {
            only_keys(args, &["binding", "item", "path", "name"])?;
            Duplicate { path: text("path")?, name: string("name")? }
        }
        "app_design_wrap" => {
            only_keys(args, &["binding", "item", "path", "ty", "name"])?;
            Wrap { path: text("path")?, ty: text("ty")?, name: string("name")? }
        }
        "app_design_rename" => {
            only_keys(args, &["binding", "item", "path", "name", "force_rust_refs"])?;
            Rename { path: text("path")?, name: text("name")?, force_rust_refs: force()? }
        }
        "app_design_bake" => {
            only_keys(args, &["binding", "item", "scope", "path", "prop"])?;
            let scope = match text("scope")?.as_str() {
                "props" => BakeScope::Props, "theme" => BakeScope::Theme, "all" => BakeScope::All,
                _ => return Err("scope must be props, theme or all".into()),
            };
            Bake { scope, path: string("path")?, prop: string("prop")? }
        }
        "app_design_history" => {
            only_keys(args, &["binding", "item", "op"])?;
            match text("op")?.as_str() {
                "undo" => Undo, "redo" => Redo, "reset" => Reset,
                _ => return Err("history op must be undo, redo or reset".into()),
            }
        }
        "app_design_patch" => match text("op")?.as_str() {
            "diff" => {
                only_keys(args, &["binding", "item", "op", "file", "offset", "limit"])?;
                let limit = limit()?.unwrap_or(12 * 1024).min(MAX_RESULT_BYTES as u64).min(wire::MAX_DIFF_LIMIT.into());
                Diff { file: string("file")?, offset: offset()?, limit: limit as u32 }
            }
            "write_patch" => {
                only_keys(args, &["binding", "item", "op", "file"])?;
                WritePatch { file: string("file")? }
            }
            "verify" => {
                only_keys(args, &["binding", "item", "op", "path"])?;
                Verify { path: string("path")? }
            }
            _ => return Err("patch op must be diff, write_patch or verify".into()),
        },
        _ => return Err("unknown design tool".into()),
    };
    validate_design_inputs(&req)?;
    let request = Request::Design(req);
    request.validate()?;
    Ok(request)
}

pub(crate) enum DesignPhase {
    Read(DesignReplyKind),
    Mutation(&'static str),
    Bake { skipped: Vec<Value> },
}

impl DesignPhase {
    pub(crate) fn for_request(req: &DesignRequest) -> Self {
        use DesignRequest::*;
        match req {
            State { .. } => Self::Read(DesignReplyKind::State),
            Palette { .. } => Self::Read(DesignReplyKind::Palette),
            Diff { .. } => Self::Read(DesignReplyKind::Diff),
            WritePatch { .. } => Self::Read(DesignReplyKind::WritePatch),
            Verify { .. } => Self::Read(DesignReplyKind::Verify),
            Insert { .. } => Self::Mutation("insert"),
            Move { .. } => Self::Mutation("move"),
            Step { dir, .. } => Self::Mutation(match dir { StepDir::Up => "up", StepDir::Down => "down", StepDir::Out => "out" }),
            Delete { .. } => Self::Mutation("delete"),
            Duplicate { .. } => Self::Mutation("duplicate"),
            Wrap { .. } => Self::Mutation("wrap"),
            Rename { .. } => Self::Mutation("rename"),
            Undo => Self::Mutation("undo"), Redo => Self::Mutation("redo"), Reset => Self::Mutation("reset"),
            Bake { .. } => Self::Bake { skipped: Vec::new() },
            _ => unreachable!("no AI tool issues a drag request"),
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Read(DesignReplyKind::State) => "state",
            Self::Read(DesignReplyKind::Palette) => "palette",
            Self::Read(DesignReplyKind::Diff) => "diff",
            Self::Read(DesignReplyKind::WritePatch) => "write patch",
            Self::Read(DesignReplyKind::Verify) => "verify",
            Self::Read(DesignReplyKind::DropTarget) => unreachable!("no AI drop target tool"),
            Self::Mutation(op) => op,
            Self::Bake { .. } => "bake",
        }
    }

    /// A partial bake reports skipped edits privately, before its signed mutation arrives.
    pub(crate) fn answer(&mut self, id: &str, notice: &Notice, view: &DesignStateView) -> Option<ToolResult> {
        let label = self.label();
        let body = match notice {
            Notice::Refused(why) => {
                return Some(if why.len() <= MAX_RESULT_BYTES { ToolResult::refused(id, why) }
                    else { ToolResult::failed(id, "Design refusal exceeds the tool result limit.") })
            }
            Notice::Design(DesignNotice::Reply { kind, json }) => self.reply(*kind, json, view).transpose()?,
            Notice::Design(DesignNotice::Mutated { op, path, focus_path, seq, .. }) => {
                self.mutated(op, path, focus_path.as_deref(), *seq, view).map(|v| v.to_json())
            }
            _ => return None,
        };
        Some(match body {
            Ok(json) if json.len() <= MAX_RESULT_BYTES => ToolResult::ok(id, json, format!("design {label}")),
            Ok(_) => ToolResult::failed(id, "Design operation completed, but the result exceeds the tool result limit."),
            Err(why) => ToolResult::failed(id, why),
        })
    }

    fn reply(&mut self, kind: DesignReplyKind, raw: &str, view: &DesignStateView) -> Result<Option<String>, String> {
        if let Self::Read(wanted) = self {
            if kind != *wanted { return Err("unexpected design reply".into()) }
            let page = match kind {
                DesignReplyKind::Palette => palette_page(&parse_json(raw)?)?,
                DesignReplyKind::Diff => diff_page(&parse_json(raw)?)?,
                _ => raw.into(),
            };
            return Ok(Some(page))
        }
        if let (Self::Bake { skipped }, DesignReplyKind::State) = (self, kind) {
            let report = parse_json(raw)?;
            let rows = report.get("skipped").and_then(Value::as_arr).ok_or("invalid bake report")?;
            if report.get("baked").and_then(Value::as_bool) == Some(true) {
                *skipped = rows.to_vec();
                return Ok(None)
            }
            return Ok(Some(json::obj(vec![("op", json::s("bake")), ("baked", Value::Bool(false)),
                ("skipped", Value::Arr(rows.to_vec())), ("dirty_files", Value::Int(view.dirty_files as i64))]).to_json()))
        }
        Err("unexpected design reply".into())
    }

    fn mutated(&mut self, op: &str, path: &str, focus: Option<&str>, seq: u64, view: &DesignStateView) -> Result<Value, String> {
        if matches!(self, Self::Read(_)) || self.label() != op {
            return Err("unexpected design mutation".into())
        }
        // As with other App edits, preserve a u64 sequence exactly for JSON consumers.
        let mut fields = vec![("op", json::s(op)), ("seq", json::s(seq.to_string()))];
        match op {
            "undo" | "redo" | "reset" => fields.extend([("undo", Value::Int(view.undo as i64)),
                ("redo", Value::Int(view.redo as i64)), ("dirty_files", Value::Int(view.dirty_files as i64))]),
            "bake" => {
                let Self::Bake { skipped } = self else { unreachable!() };
                fields.extend([("baked", Value::Bool(true)), ("path", json::s(path)),
                    ("skipped", Value::Arr(std::mem::take(skipped))), ("dirty_files", Value::Int(view.dirty_files as i64))]);
            }
            _ => {
                fields.push((if op == "rename" { "old_path" } else { "path" }, json::s(path)));
                if op != "delete" { fields.push(("focus_path", focus.map_or(Value::Null, json::s))); }
                if matches!(op, "insert" | "duplicate" | "wrap" | "rename") {
                    fields.push(("name", focus.map(widget_name).map_or(Value::Null, json::s)));
                }
            }
        }
        Ok(json::obj(fields))
    }
}

fn count(body: &Value, key: &str) -> Result<i64, String> {
    body.get(key).and_then(Value::as_i64).filter(|n| *n >= 0).ok_or_else(|| format!("invalid design page {key}"))
}

fn next_offset(end: i64, total: i64) -> Value {
    if end < total { Value::Int(end) } else { Value::Null }
}

fn palette_page(raw: &Value) -> Result<String, String> {
    let total = count(raw, "total")?;
    let offset = count(raw, "offset")?;
    let rows = raw.get("palette").and_then(Value::as_arr).ok_or("invalid design palette")?;
    let envelope = |next, items| json::obj(vec![("total", Value::Int(total)), ("offset", Value::Int(offset)),
        ("next_offset", next), ("items", Value::Arr(items))]);
    // Account for the exact candidate cursor and each serialized row once, including commas.
    let fixed = envelope(Value::Null, vec![]).to_json().len() - "null".len();
    let mut bytes = fixed;
    let mut emitted = 0;
    for row in rows {
        let end = offset.checked_add(emitted + 1).filter(|end| *end <= total).ok_or("invalid palette count")?;
        bytes += row.to_json().len() + usize::from(emitted != 0);
        if bytes + next_offset(end, total).to_json().len() > MAX_RESULT_BYTES {
            if emitted == 0 { return Err(format!("palette entry at offset {offset} exceeds the {MAX_RESULT_BYTES}-byte page limit")) }
            break
        }
        emitted += 1;
    }
    if emitted == 0 && offset < total { return Err("incomplete design palette".into()) }
    Ok(envelope(next_offset(offset + emitted, total), rows[..emitted as usize].to_vec()).to_json())
}

fn diff_page(raw: &Value) -> Result<String, String> {
    let total = count(raw, "total_bytes")?;
    let requested = count(raw, "offset")?;
    let file = raw.get("file").ok_or("missing diff file")?;
    let patch = raw.get("patch").and_then(Value::as_str).ok_or("missing diff text")?;
    // The owner rounds a start inside a UTF-8 character up to its next boundary.
    let end = match raw.get("next_offset") {
        Some(Value::Null) => total,
        Some(_) => count(raw, "next_offset")?,
        None => return Err("missing diff cursor".into()),
    };
    let start = end - patch.len() as i64;
    if start < requested.min(total) || end > total || (patch.is_empty() && end < total) {
        return Err("invalid diff range".into())
    }
    let offset = start.max(requested);
    let mut page = patch;
    loop {
        let json = json::obj(vec![("file", file.clone()), ("total_bytes", Value::Int(total)), ("offset", Value::Int(offset)),
            ("next_offset", next_offset(offset + page.len() as i64, total)), ("patch", json::s(page))]).to_json();
        if json.len() <= MAX_RESULT_BYTES { return Ok(json) }
        let smaller = patch::page(page, 0, (page.len() / 2) as u32).0;
        if smaller.is_empty() || smaller.len() == page.len() { return Err("diff page header exceeds the tool result limit".into()) }
        page = smaller;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_palette_entry_fails_without_repeating_its_offset() {
        let raw = json::obj(vec![("total", Value::Int(2)), ("offset", Value::Int(1)), ("palette", Value::Arr(vec![
            json::obj(vec![("name", json::s("View")), ("group", json::s("Layout")), ("body", json::s("x".repeat(16 * 1024)))]),
        ]))]);
        let kind = DesignReplyKind::Palette;
        let notice = Notice::Design(DesignNotice::Reply { kind, json: raw.to_json() });
        let result = DesignPhase::Read(kind).answer("palette", &notice, &Default::default()).unwrap();
        assert_eq!(result.outcome, makepad_ai_services::wire::ToolOutcome::Failed);
        assert_eq!(result.text, format!("palette entry at offset 1 exceeds the {MAX_RESULT_BYTES}-byte page limit"));
    }
}
