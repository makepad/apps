//! The document tools over a product's [`Documents`]: the open tree (or
//! its one root file), read in pages, written, patched and reorganised as
//! one undo step each, with its history.

use super::{done, fail, is_binary, kind_name, text, LibraryJob, ProductTools};
use makepad_app_shell::tool_host::{Change, Ctx, Documents, History, Outcome, ToolHost, ToolProduct};
use makepad_asset_data::tree::TreeDraft;
use makepad_studio_api::{self as api, diff, DocChange, ToolError, ToolOutput};
use makepad_strict_json::{self as json, Value as Json};
use std::collections::HashMap;

/// A file's text and the revision it is at: a file of the open tree, or
/// the root file of a document without one.
pub fn document_state<P: Documents>(app: &P::App, path: &str) -> Result<(String, u64), ToolError> {
    if let Some(open) = P::open_tree(app) {
        return open.tree.text(path).map(|text| (text.into(), open.revision)).map_err(ToolError::refused);
    }
    if path != P::root_path(app) {
        return Err(ToolError::not_found(format!("no file `{path}`; call tree_list")));
    }
    P::doc_state(app)
}

/// The file the tools name as the document's root.
pub fn root_path<P: Documents>(app: &P::App) -> String {
    match P::open_tree(app) {
        Some(open) => open.path.into(),
        None => P::root_path(app),
    }
}

/// The change feed entry and the answer of a tree change: every file it
/// changed, with its diff.
pub fn tree_output<P: ToolProduct>(host: &mut ToolHost<P>, tool: &str, origin: &str, revision: u64, before: Option<&TreeDraft>, after: &TreeDraft) -> ToolOutput {
    let paths: std::collections::BTreeSet<_> = before.into_iter().flat_map(|tree| tree.files.keys()).chain(after.files.keys()).collect();
    let mut changes = Vec::new();
    let mut patches = Vec::new();
    for path in paths {
        let old = before.and_then(|tree| tree.files.get(path));
        let new = after.files.get(path);
        if old.map(|file| (file.kind, &file.bytes)) == new.map(|file| (file.kind, &file.bytes)) {
            continue;
        }
        let old_text = old.filter(|file| !is_binary(file.kind)).and_then(|file| file.text().ok()).unwrap_or("");
        let new_text = new.filter(|file| !is_binary(file.kind)).and_then(|file| file.text().ok()).unwrap_or("");
        patches.push(format!("{path}\n{}", diff::unified(old_text, new_text, 2)));
        changes.push(DocChange { key: path.clone(), label: tool.into(), before: old_text.into(), after: new_text.into() });
    }
    host.log.push(revision, tool, origin, tool.replace('_', " "), patches.join("\n"));
    let mut output = ToolOutput::json(json::obj(vec![("path", json::s(after.root.1.clone())), ("revision", Json::Int(revision as i64)), ("changed", Json::Arr(changes.iter().map(|change| json::s(change.key.clone())).collect())), ("diagnostics", Json::Arr(Vec::new()))]));
    output.changes = changes;
    output
}

pub fn tree_list<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let app = &*ctx.app;
    let prefix = text(args, "path").unwrap_or("");
    let mut files = Vec::new();
    if let Some(open) = P::open_tree(app) {
        let (tree, root) = (open.tree, open.path);
        let mut uses = P::file_uses(app);
        // Graphics a slide's inline cut uses (`Graphic{src: graphic("slides/x")}`):
        // each file says which slides show it.
        let mut graphic_uses: HashMap<String, Vec<Json>> = HashMap::new();
        if let Ok(Json::Arr(rows)) = tree.text(root).map_err(|_| ()).and_then(|source| api::document::outline(source).map_err(|_| ())) {
            for row in rows.iter().filter(|row| row.get("object").and_then(Json::as_str) == Some("Slide")) {
                let Some(name) = row.get("name").and_then(Json::as_str) else { continue };
                for graphic in row.get("graphics").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(Json::as_str) {
                    let file = if tree.files.contains_key(graphic) { graphic.to_string() } else { format!("{graphic}.splash") };
                    graphic_uses.entry(file).or_default().push(json::s(name));
                }
            }
        }
        let folder = format!("{}/", prefix.trim_end_matches('/'));
        for (path, file) in &tree.files {
            if !prefix.is_empty() && path != prefix && !path.starts_with(&folder) {
                continue;
            }
            let mut slides = uses.remove(path.as_str()).unwrap_or_default();
            if let Some(names) = graphic_uses.remove(path.as_str()) {
                slides.extend(names.into_iter().map(|name| json::obj(vec![("name", name), ("as", json::s("graphic"))])));
            }
            files.push(json::obj(vec![("path", json::s(path.clone())), ("kind", json::s(kind_name(file.kind))), ("bytes", Json::Int(file.bytes.len() as i64)), ("slides", Json::Arr(slides))]));
        }
    } else {
        let path = P::root_path(app);
        if prefix.is_empty() || path == prefix {
            match P::doc_state(app) {
                Ok((source, _)) => files.push(json::obj(vec![("path", json::s(path)), ("bytes", Json::Int(source.len() as i64))])),
                Err(error) => return fail(error),
            }
        }
    }
    done(json::obj(vec![("root", json::s(root_path::<P>(app))), ("files", Json::Arr(files)), ("diagnostics", Json::Arr(Vec::new())), ("selection", P::selection(app))]))
}

pub fn tree_search<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    const MOST: usize = 60;
    let app = &*ctx.app;
    let Some(query) = text(args, "text").map(str::to_lowercase).filter(|query| !query.is_empty()) else { return fail(ToolError::invalid("give the `text` to look for")) };
    let prefix = text(args, "path").unwrap_or("");
    let files: Vec<(String, String)> = match P::open_tree(app) {
        Some(open) => open.tree.files.iter().filter(|(path, file)| path.starts_with(prefix) && !is_binary(file.kind))
            .filter_map(|(path, file)| file.text().ok().map(|text| (path.clone(), text.to_string()))).collect(),
        None => match P::doc_state(app) {
            Ok((source, _)) => vec![(P::root_path(app), source)],
            Err(error) => return fail(error),
        },
    };
    let mut matches = Vec::new();
    let mut more = 0usize;
    for (path, source) in &files {
        for (index, line) in source.lines().enumerate() {
            if !line.to_lowercase().contains(&query) {
                continue;
            }
            if matches.len() == MOST {
                more += 1;
                continue;
            }
            let shown: String = line.trim().chars().take(160).collect();
            matches.push(json::obj(vec![("path", json::s(path.clone())), ("line", Json::Int(index as i64 + 1)), ("text", json::s(shown))]));
        }
    }
    let mut fields = vec![("matches", Json::Arr(matches))];
    if more > 0 {
        fields.push(("more", Json::Int(more as i64)));
    }
    done(json::obj(fields))
}

pub fn read<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let Some(path) = text(args, "path") else { return fail(ToolError::invalid("give a file `path` from tree_list")) };
    if let Some(open) = P::open_tree(ctx.app) {
        if let Some(file) = open.tree.files.get(path).filter(|file| is_binary(file.kind)) {
            return done(json::obj(vec![("path", json::s(path)), ("revision", Json::Int(open.revision as i64)), ("kind", json::s(kind_name(file.kind))), ("bytes", Json::Int(file.bytes.len() as i64)), ("binary", Json::Bool(true)), ("diagnostics", Json::Arr(Vec::new()))]));
        }
    }
    match document_state::<P>(ctx.app, path) {
        Ok((source, revision)) => {
            let summary = P::summary(ctx.app);
            let from_line = args.get("from_line").and_then(Json::as_u64).unwrap_or(1) as usize;
            P::library(ctx, LibraryJob::ReadDocument { path: path.into(), source, revision, summary, from_line })
        }
        Err(error) => fail(error),
    }
}

pub fn write<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (Some(path), Some(text)) = (text(args, "path"), text(args, "text")) else { return fail(ToolError::invalid("give `path` and `text`")) };
    // The file as it is now: a write that lands after another change to the
    // same file (the person's, or a parallel call's) is refused, not merged.
    let expected = match document_state::<P>(ctx.app, path) {
        Ok((source, _)) => P::open_tree(ctx.app).is_some().then_some(source),
        Err(_) if P::open_tree(ctx.app).is_some() => None,
        Err(error) => return fail(error),
    };
    P::change(ctx, args, Change::Write { path: path.into(), text: text.into(), expected })
}

pub fn patch<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let Some(path) = text(args, "path") else { return fail(ToolError::invalid("give `path`")) };
    let edits = match api::document::edits(args) {
        Ok(edits) => edits,
        Err(error) => return fail(ToolError::invalid(error)),
    };
    if P::open_tree(ctx.app).is_none() {
        if let Err(error) = document_state::<P>(ctx.app, path) {
            return fail(error);
        }
    }
    P::change(ctx, args, Change::Patch { path: path.into(), edits })
}

/// A file added, renamed or removed: only in a tree, never the root file
/// or a file with a source draft.
fn tree_change<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json, change: Change) -> Outcome<P::Wait> {
    let removed = match &change {
        Change::Rename { from, .. } => Some(from),
        Change::Delete { path } => Some(path),
        _ => None,
    };
    if matches!(&change, Change::Delete { path } if *path == root_path::<P>(ctx.app)) {
        return fail(ToolError::refused("open another document before deleting the selected cut file"));
    }
    if removed.is_some_and(|path| P::has_file_draft(ctx.app, path)) {
        return fail(ToolError::refused("apply or revert the file’s source draft before moving or deleting it"));
    }
    if P::open_tree(ctx.app).is_none() {
        return fail(ToolError::refused("this document has only its root file; open a tree to create, rename or delete files"));
    }
    P::change(ctx, args, change)
}

pub fn create<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let Some(path) = text(args, "path") else { return fail(ToolError::invalid("give `path`")) };
    tree_change(ctx, args, Change::Create { path: path.into(), from: text(args, "from").map(str::to_string), text: text(args, "text").map(str::to_string) })
}

pub fn rename<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let (Some(from), Some(to)) = (text(args, "from"), text(args, "to")) else { return fail(ToolError::invalid("give `from` and `to` paths")) };
    tree_change(ctx, args, Change::Rename { from: from.into(), to: to.into() })
}

pub fn delete<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let Some(path) = text(args, "path") else { return fail(ToolError::invalid("give `path`")) };
    tree_change(ctx, args, Change::Delete { path: path.into() })
}

pub fn history<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let path = text(args, "path");
    let snapshots = match P::history(ctx.app, path) {
        Ok(History::Steps(snapshots)) => snapshots,
        Ok(History::Answer(answer)) => return done(answer),
        Err(error) => return fail(error),
    };
    let root = root_path::<P>(ctx.app);
    let entries = snapshots.iter().enumerate().map(|(index, (source, tree))| {
        let paths: Vec<_> = tree.as_ref().map_or_else(|| vec![root.clone()], |tree| tree.files.keys().filter(|p| path.is_none_or(|path| p.as_str() == path)).cloned().collect());
        let text = path.and_then(|path| match tree {
            Some(tree) => tree.text(path).ok(),
            None => (path == root).then_some(source.as_str()),
        });
        json::obj(vec![("step", Json::Int(index as i64)), ("paths", Json::Arr(paths.into_iter().map(json::s).collect())), ("text", text.map(json::s).unwrap_or(Json::Null))])
    }).collect();
    let working = Json::Arr(entries);
    if let Some(head) = P::open_tree(ctx.app).and_then(|open| open.head) {
        return P::library(ctx, LibraryJob::TreeHistory { head, path: path.map(str::to_string), working });
    }
    done(json::obj(vec![("history", working), ("unit", json::s("tree revision"))]))
}

