//! Working tree changes shared by the source pane and the studio tool worker.
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType, tree::{TreeDraft, TreeRole, TreeFile}};
use makepad_studio_api::document;
use makepad_app_shell::tool_host::Change;
use std::sync::Arc;
use super::cut::open_doc::TreeDoc;

pub enum Operation {
    Undo { redo: bool },
    Import { reference: AssetRevisionRef, path: String, op: makepad_story_deck::ops::DeckOp },
    Document(Change),
    Deck(makepad_story_deck::ops::DeckOp),
    /// Remove a file and the slides that reference it in one undo step.
    DeleteFile { path: String },
}

pub fn apply(tree: &TreeDraft, change: &Change) -> Result<Arc<TreeDraft>, String> {
    apply_all(tree, std::slice::from_ref(change))
}

pub fn apply_all(tree: &TreeDraft, changes: &[Change]) -> Result<Arc<TreeDraft>, String> {
    let mut next = Arc::new(tree.clone());
    for change in changes { next = apply_one(&next, change)?; }
    makepad_asset_client::tree::validate_draft(&next)?;
    Ok(next)
}

fn apply_one(tree: &TreeDraft, change: &Change) -> Result<Arc<TreeDraft>, String> {
    let mut next = tree.clone();
    match change {
        Change::Import { reference, path } => { import_file(&mut next, *reference, path)?; }
        Change::Write { path, text, expected } => {
            let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`; use doc_create"))?;
            if matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio) { return Err(format!("{path}: binary media is imported, not written as text")); }
            if file.text()? == text.as_str() { return Ok(Arc::new(next)); }
            if expected.as_deref().is_some_and(|expected| file.text().ok() != Some(expected)) { return Err(format!("{path}: changed while this draft was pending; choose which version to keep in the source pane")); }
            let text = if path.ends_with(".splash") { document::write(file.text()?, text)? } else { text.clone() };
            let kind = source_kind(path, &text, file.kind)?;
            next.write(path, kind, text.into_bytes().into())?;
        }
        Change::Patch { path, edits } => {
            let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`"))?;
            if !path.ends_with(".splash") { return Err("doc_patch edits parsed Splash spans; use doc_write for text files".into()); }
            let text = super::cut::patch::Patcher::document(file.text()?, edits)?;
            let kind = source_kind(path, &text, file.kind)?;
            next.write(path, kind, text.into_bytes().into())?;
        }
        Change::Create { path, from: Some(from), text: None } => return makepad_asset_client::tree::rename_draft(tree, from, path, true).map(Arc::new),
        Change::Create { path, from: None, text } => {
            if tree.files.contains_key(path) { return Err(format!("file `{path}` already exists")); }
            let text = text.as_deref().unwrap_or("");
            let kind = source_kind(path, text, if path.ends_with(".splash") { AssetKind::Motion } else { AssetKind::Data })?;
            next.write(path, kind, text.as_bytes().into())?;
        }
        Change::Create { .. } => return Err("give either `from` or `text`, not both".into()),
        Change::Rename { from, to } => return makepad_asset_client::tree::rename_draft(tree, from, to, false).map(Arc::new),
        Change::Delete { path } => {
            if next.root.1 == *path { return Err("the root document cannot be deleted".into()); }
            if next.files.remove(path).is_none() && !next.optional.contains(path) { return Err(format!("no file `{path}`")); }
        }
    }
    Ok(Arc::new(next))
}

fn source_kind(path: &str, text: &str, fallback: AssetKind) -> Result<AssetKind, String> {
    use makepad_widgets::makepad_script::syntax::{self, Kind};
    if !path.ends_with(".splash") { return Ok(fallback); }
    let syntax = syntax::parse(text).map_err(|e| format!("{path}: {e}"))?;
    let Some(node) = syntax.statements().last() else { return Ok(fallback) };
    let Some((Some(ty), _)) = node.object() else { return Ok(fallback) };
    Ok(match ty.rsplit('.').next() {
        Some("Flow") if node.field("kind").is_some_and(|field| matches!(&field.value.kind, Kind::Name(kind) if kind == "cut")) => AssetKind::Cut,
        Some("Deck") => AssetKind::Deck,
        Some("Motion") => AssetKind::Motion,
        Some("Score") => AssetKind::Score,
        _ => fallback,
    })
}

pub fn load_deck(tree: &Arc<TreeDraft>) -> Result<Arc<makepad_story_deck::DeckDoc>, String> {
    if tree.root.0 != TreeRole::Deck { return Err("this tree is not a deck".into()); }
    load(tree, &tree.root.1)?.into_deck()
}

/// The deck file `path` of `tree` after a change of `before`, whose deck was `prior`:
/// only the slides whose graphic file changed (or reads a changed file) are
/// evaluated again ([`makepad_story_deck::DeckDoc::reload_tree`]).
pub fn reload_deck(tree: &Arc<TreeDraft>, path: &str, before: &TreeDraft, prior: &makepad_story_deck::DeckDoc) -> Result<Arc<makepad_story_deck::DeckDoc>, String> {
    if tree.root.0 != TreeRole::Deck { return Err("this tree is not a deck".into()); }
    makepad_asset_client::tree::validate_draft(tree)?;
    let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`"))?;
    if file.kind != AssetKind::Deck { return Err(format!("{path}: expected a deck document")); }
    makepad_story_deck::DeckDoc::reload_tree(file.text()?, tree.context(path), prior, &changed_files(before, tree))
        .map(Arc::new).map_err(|diags| diags.iter().map(|diag| format!("{path}:{diag}")).collect::<Vec<_>>().join("\n"))
}

/// The paths whose content differs between two trees (written, created, removed).
pub fn changed_files(before: &TreeDraft, after: &TreeDraft) -> Vec<String> {
    let same = |a: &TreeFile, b: &TreeFile| a.kind == b.kind && (Arc::ptr_eq(&a.bytes, &b.bytes) || a.bytes == b.bytes);
    let mut changed: Vec<String> = after.files.iter().filter(|(path, file)| before.files.get(*path).is_none_or(|was| !same(was, file))).map(|(path, _)| path.clone()).collect();
    changed.extend(before.files.keys().filter(|path| !after.files.contains_key(*path)).cloned());
    changed
}

/// The deck file `path` of a deck tree (its root or another
/// `<name>.deck.splash`).
pub fn load_deck_at(tree: &Arc<TreeDraft>, path: &str) -> Result<Arc<makepad_story_deck::DeckDoc>, String> {
    if tree.root.0 != TreeRole::Deck { return Err("this tree is not a deck".into()); }
    load(tree, path)?.into_deck()
}

/// The deck files of a deck tree: the root first, then the others by path.
/// A tree may hold several talks; links jump between them.
pub fn deck_files(tree: &TreeDraft) -> Vec<String> {
    if tree.root.0 != TreeRole::Deck { return Vec::new(); }
    std::iter::once(tree.root.1.clone())
        .chain(tree.files.iter().filter(|(path, file)| file.kind == AssetKind::Deck && **path != tree.root.1).map(|(path, _)| path.clone()))
        .collect()
}

/// The deck file an edit of `path` changes: `path` when it is a deck file,
/// else the tree's root deck.
pub fn deck_path(tree: &TreeDraft, path: &str) -> String {
    if tree.files.get(path).is_some_and(|file| file.kind == AssetKind::Deck) { path.to_string() } else { tree.root.1.clone() }
}

/// Every deck file of the tree, loaded, with each one's link problems
/// (a target that does not load, a `link_slide` it lacks).
pub fn check_deck_links(tree: &Arc<TreeDraft>) -> Vec<String> {
    let decks: Vec<(String, Result<Arc<makepad_story_deck::DeckDoc>, String>)> = deck_files(tree).into_iter().map(|path| { let doc = load_deck_at(tree, &path); (path, doc) }).collect();
    let mut out = Vec::new();
    for (path, doc) in &decks {
        let Ok(doc) = doc else { continue };
        let lookup = |target: &str| decks.iter().find(|(p, _)| p == target).and_then(|(_, doc)| doc.as_deref().ok());
        for diag in makepad_story_deck::check_links(doc, lookup) {
            out.push(format!("{path}:{diag}"));
        }
    }
    out
}

pub fn load(tree: &Arc<TreeDraft>, path: &str) -> Result<TreeDoc, String> {
    load_with(tree, path, false)
}

/// A deck with only its first slide's graphic loaded (the others are
/// pending until [`load`]); any other document loads whole.
pub fn load_first(tree: &Arc<TreeDraft>, path: &str) -> Result<TreeDoc, String> {
    load_with(tree, path, true)
}

fn load_with(tree: &Arc<TreeDraft>, path: &str, first: bool) -> Result<TreeDoc, String> {
    makepad_asset_client::tree::validate_draft(tree)?;
    let file = tree.files.get(path).ok_or_else(|| format!("no file `{path}`"))?;
    let deck = if first { makepad_story_deck::DeckDoc::load_tree_first } else { makepad_story_deck::DeckDoc::load_tree };
    match file.kind {
        AssetKind::Cut => makepad_story_cut::CutDoc::load_tree(file.text()?, tree.context(path))
            .map(|doc| TreeDoc::Cut(Arc::new(doc))).map_err(super::cut::patch::diagnostics),
        AssetKind::Deck => deck(file.text()?, tree.context(path))
            .map(|doc| TreeDoc::Deck(Arc::new(doc))).map_err(|diags| diags.iter().map(|diag| format!("{path}:{diag}")).collect::<Vec<_>>().join("\n")),
        _ => Err(format!("{path}: expected a cut or deck document")),
    }
}

pub fn media_stem(tree: &TreeDraft, title: &str, folder: &str) -> String {
    let words: String = title.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '_' }).collect();
    let mut name = words.split('_').filter(|word| !word.is_empty()).collect::<Vec<_>>().join("_");
    if name.is_empty() { name = "imported_media".into(); }
    else if !name.starts_with(|ch: char| ch.is_ascii_lowercase()) { name.insert_str(0, "media_"); }
    (1..).map(|n| format!("{folder}/{name}{}", if n == 1 { String::new() } else { format!("_{n}") }))
        .find(|stem| !tree.files.keys().any(|path| path.starts_with(&format!("{stem}.")))).unwrap()
}

/// Shared file navigation for CUT and DECK. Build only when the revision,
/// selection or folder folds change; file contents are shared by snapshots.
pub fn file_rows(tree: &TreeDraft, saved: Option<&TreeDraft>, folded: &std::collections::HashSet<String>) -> Vec<super::structure::TreeRow> {
    use super::structure::TreeRow;
    use makepad_motion::structure::NodeKind;
    let mut rows = Vec::new();
    let mut folders = std::collections::HashSet::new();
    for (path, file) in tree.files.get_key_value(&tree.root.1).into_iter().chain(tree.files.iter().filter(|(path, _)| path.as_str() != tree.root.1)) {
        let mut hidden = false;
        for (end, _) in path.match_indices('/') {
            let folder = &path[..end];
            let key = format!("{folder}/");
            if folders.insert(key.clone()) {
                rows.push(TreeRow { path: key.clone(), depth: folder.matches('/').count(), label: folder.rsplit('/').next().unwrap_or(folder).into(), detail: String::new(), kind: NodeKind::Scene, on: true, open: Some(!folded.contains(&key)) });
            }
            if folded.contains(&key) { hidden = true; break; }
        }
        if hidden { continue; }
        let dirty = saved.and_then(|tree| tree.files.get(path)).is_none_or(|saved| saved.bytes != file.bytes);
        rows.push(TreeRow { path: path.clone(), depth: path.matches('/').count(), label: path.rsplit('/').next().unwrap_or(path).into(), detail: if dirty { "modified".into() } else { String::new() }, kind: NodeKind::Code, on: true, open: None });
    }
    rows
}

/// Import once, retain the library revision, then use the same CUT patcher as
/// every other edit. The file and timeline change enter history together.
pub fn prepare_cut(patcher: &mut super::cut::patch::Patcher, tree: &Arc<TreeDraft>, path: &str, op: &super::cut::ops::CutOp) -> Result<Arc<TreeDraft>, String> {
    use super::cut::ops::CutOp;
    use makepad_story_cut::{AssetRef, MediaRef};
    let mut assets = Vec::new();
    let media_asset = |media: &MediaRef| match media {
        MediaRef::Asset(asset) => Some(asset.clone()),
        MediaRef::Run(run) => Some(AssetRef { id: run.render_id.clone(), rev: run.render.clone() }),
        MediaRef::MissingOptional(_) => None,
    };
    match op {
        CutOp::Add { media, .. } => assets.extend(media_asset(&media.src)),
        CutOp::Param { value, .. } => { if let Some(value) = super::cut::ops::media_param(value) { assets.extend(media_asset(&value)); } }
        CutOp::SetLines { lines, .. } => assets.extend(lines.iter().filter_map(|line| line.take.clone())),
        _ => {}
    };
    let mut tree = tree.clone();
    for asset in assets {
        let reference = AssetRevisionRef { asset_id: asset.id.parse().map_err(|_| "invalid media reference")?, revision: asset.rev.parse().map_err(|_| "invalid media revision")? };
        if !tree.files.values().any(|file| file.reference == Some(reference)) {
            let mut connection = None;
            let client = makepad_asset_connection::connected(&mut connection, &makepad_asset_connection::origin())?;
            let annotation = client.get_annotation(&reference.asset_id).map_err(|e| e.to_string())?;
            let stem = media_stem(&tree, &annotation.title, "media");
            let mut next = (*tree).clone();
            import_file(&mut next, reference, &stem)?;
            tree = Arc::new(next);
        }
    }
    patcher.tree = Some((tree.clone(), path.into()));
    Ok(tree)
}

pub fn apply_cut(patcher: &mut super::cut::patch::Patcher, tree: &Arc<TreeDraft>, path: &str, op: &super::cut::ops::CutOp) -> Result<(String, Arc<makepad_story_cut::CutDoc>, Arc<TreeDraft>), String> {
    let tree = prepare_cut(patcher, tree, path, op)?;
    let (text, _) = super::cut::ops::apply(patcher, tree.text(path)?, op)?;
    let tree = apply(&tree, &Change::Write { path: path.into(), text: text.clone(), expected: None })?;
    let doc = load(&tree, path)?.into_cut()?;
    Ok((text, doc, tree))
}

/// A library drop retains the existing media revision. Reads run on the studio
/// worker; adding its entry and the slide is one working-tree transaction.
pub fn import_file(tree: &mut TreeDraft, reference: AssetRevisionRef, stem: &str) -> Result<String, String> {
    use makepad_asset_client::{tree::TreeStore, TierPreference};
    let mut connection = None;
    let client = makepad_asset_connection::connected(&mut connection, &makepad_asset_connection::origin())?;
    let manifest = client.fetch_asset_manifest(&reference.revision).map_err(|e| e.to_string())?;
    let role = match manifest.kind { AssetKind::Texture => FileRole::Texture, AssetKind::Video => FileRole::Video, AssetKind::Audio => FileRole::Audio, _ => FileRole::Source };
    let file = makepad_asset_client::select_file(&manifest, role, TierPreference::Exact(makepad_asset_data::DeviceTier::Any), 0).map_err(|e| e.to_string())?;
    let extension = match file.media { MediaType::Png => "png", MediaType::Jpeg => "jpg", MediaType::Mp4 => "mp4", MediaType::Wav => "wav", MediaType::Ogg => "ogg", MediaType::Mp3 => "mp3", MediaType::Text => "splash", _ => return Err("this library file is not a document or media file".into()) };
    let path = format!("{stem}.{extension}");
    makepad_asset_data::tree::validate_path(&path)?;
    if tree.files.contains_key(&path) { return Err(format!("file `{path}` already exists")); }
    let content = TreeStore::read(client, reference)?;
    tree.files.insert(path.clone(), TreeFile { path: path.clone(), reference: Some(reference), base: Some(reference), kind: content.kind, bytes: content.bytes });
    Ok(path)
}
