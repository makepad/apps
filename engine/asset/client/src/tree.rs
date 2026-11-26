//! Tree editing is a worker operation. Content is published first; a single
//! guarded manifest publication makes the complete edit visible atomically.

pub use makepad_asset_data::tree::*;
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType};
use crate::{AssetClient, PublishBundle, PublishBundleFile, PublishExpectedHead, PublishRights, TierPreference};
use makepad_script::syntax::{self, edit::Replace};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone)]
pub struct TreeContent {
    pub kind: AssetKind,
    pub bytes: Arc<[u8]>,
    pub rights: PublishRights,
}

/// The same edit engine works against the client or an in-memory store.
pub trait TreeStore {
    fn read(&mut self, reference: AssetRevisionRef) -> Result<TreeContent, String>;
    fn publish(&mut self, bundle: &PublishBundle) -> Result<AssetRevisionRef, String>;
}

impl TreeStore for AssetClient {
    fn read(&mut self, reference: AssetRevisionRef) -> Result<TreeContent, String> {
        let manifest = self.fetch_asset_manifest(&reference.revision).map_err(|e| e.to_string())?;
        if manifest.asset_id != reference.asset_id { return Err("tree entry identity does not match its revision".into()); }
        let role = match manifest.kind {
            AssetKind::Texture => FileRole::Texture,
            AssetKind::Audio => FileRole::Audio,
            AssetKind::Video => FileRole::Video,
            _ => FileRole::Source,
        };
        let file = crate::select_file(&manifest, role, TierPreference::Exact(makepad_asset_data::DeviceTier::Any), 0).map_err(|e| e.to_string())?;
        let bytes = self.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|e| e.to_string())?;
        Ok(TreeContent { kind: manifest.kind, bytes: bytes.into(), rights: PublishRights::from_manifest(&manifest.rights) })
    }

    fn publish(&mut self, bundle: &PublishBundle) -> Result<AssetRevisionRef, String> {
        let published = self.publish_bundle(bundle).map_err(|e| e.to_string())?;
        Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
    }
}

/// Fetch an immutable tree on a worker, verifying its dependency list before
/// handing its already-fetched bytes to synchronous document loaders.
pub fn load(client: &mut AssetClient, reference: AssetRevisionRef) -> Result<MemoryTree, String> {
    let manifest = client.fetch_asset_manifest(&reference.revision).map_err(|e| e.to_string())?;
    if manifest.asset_id != reference.asset_id || manifest.kind != AssetKind::Tree { return Err("expected a Tree revision".into()); }
    let content = TreeStore::read(client, reference)?;
    let tree = Tree::parse(std::str::from_utf8(&content.bytes).map_err(|_| "tree manifest is not UTF-8")?)?;
    if tree.dependencies() != manifest.dependencies { return Err("tree dependencies do not match its entries".into()); }
    let mut snapshot = MemoryTree { tree, ..Default::default() };
    for entry in snapshot.tree.entries.values() {
        let Some(reference) = entry.reference() else { continue };
        if matches!(entry, TreeEntry::Link { .. }) { continue; }
        if let std::collections::btree_map::Entry::Vacant(slot) = snapshot.files.entry(reference) {
            let content = TreeStore::read(client, reference)?;
            slot.insert((content.kind, content.bytes));
        }
    }
    Ok(snapshot)
}

/// A mutable checkout with an alias-scoped head precondition. The publication
/// template supplies identity, alias, thumbnail and rights; content and deps
/// are always derived from the tree. On failure the checkout is unchanged.
pub struct TreeEditor {
    pub tree: Tree,
    pub head: Option<AssetRevisionRef>,
    publication: PublishBundle,
}

impl TreeEditor {
    /// Publish every changed working file, then expose them with ONE guarded
    /// manifest publication. The previous head stays visible on any failure.
    pub fn save_draft(&mut self, store: &mut impl TreeStore, draft: &TreeDraft) -> Result<MemoryTree, String> {
        validate_draft(draft)?;
        let mut tree = Tree { name: draft.name.clone(), root: Some(draft.root.clone()), entries: draft.optional.iter().map(|path| (path.clone(), TreeEntry::Optional(None))).collect() };
        let mut files = BTreeMap::new();
        for (path, file) in &draft.files {
            let reference = match file.reference {
                Some(reference) => reference,
                None => {
                    let previous = file.base.or_else(|| self.tree.entries.get(path).and_then(TreeEntry::reference));
                    let rights = match previous { Some(reference) => store.read(reference)?.rights, None => self.publication.rights.clone() };
                    self.publish_file(store, path, &TreeContent { kind: file.kind, bytes: file.bytes.clone(), rights }, previous)?
                }
            };
            tree.entries.insert(path.clone(), if draft.optional.contains(path) { TreeEntry::Optional(Some(reference)) } else { TreeEntry::File(reference) });
            files.insert(reference, (file.kind, file.bytes.clone()));
        }
        self.commit(store, tree.clone())?;
        Ok(MemoryTree { tree, files })
    }
    /// Reserve the stable identity before the first guarded publication.
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "web")))]
    pub fn create(client: &mut AssetClient, tree: Tree, mut publication: PublishBundle) -> Result<Self, String> {
        tree.validate()?;
        if publication.alias.is_none() { return Err("a tree requires an alias".into()); }
        publication.asset_id = Some(client.api().register_asset(&publication.namespace, None).map_err(|e| e.to_string())?);
        Self::new(tree, publication, None)
    }

    pub fn new(tree: Tree, mut publication: PublishBundle, head: Option<AssetRevisionRef>) -> Result<Self, String> {
        tree.validate()?;
        if publication.asset_id.is_none() || publication.alias.is_none() { return Err("a tree edit requires a registered asset id and alias".into()); }
        if head.is_some_and(|head| Some(head.asset_id) != publication.asset_id) { return Err("tree head has a different asset identity".into()); }
        publication.kind = AssetKind::Tree;
        Ok(Self { tree, head, publication })
    }

    /// Publish the initial manifest or the current checkout once.
    pub fn publish(&mut self, store: &mut impl TreeStore) -> Result<AssetRevisionRef, String> {
        let contents = self.contents(store)?;
        validate_documents(&contents, &self.tree.optional_paths())?;
        self.commit(store, self.tree.clone())
    }

    /// Write a Splash/text document, arbitrary Data, or a PNG/JPEG texture.
    /// Kinds requiring measured media metadata use their typed publisher;
    /// its exact revision can then be included in `tree.entries` and published.
    pub fn write(&mut self, store: &mut impl TreeStore, path: &str, bytes: &[u8], kind: AssetKind) -> Result<AssetRevisionRef, String> {
        validate_path(path)?;
        if matches!(self.tree.entries.get(path), Some(TreeEntry::Link { .. })) { return Err("cannot write a reserved link".into()); }
        let mut contents = self.contents(store)?;
        let rights = contents.get(path).map_or_else(|| self.publication.rights.clone(), |c| c.rights.clone());
        let content = TreeContent { kind, bytes: bytes.into(), rights };
        contents.insert(path.into(), content.clone());
        validate_documents(&contents, &self.tree.optional_paths())?;
        let mut tree = self.tree.clone();
        let previous = tree.entries.get(path).and_then(TreeEntry::reference);
        let reference = self.publish_file(store, path, &content, previous)?;
        let entry = if matches!(tree.entries.get(path), Some(TreeEntry::Optional(_))) { TreeEntry::Optional(Some(reference)) } else { TreeEntry::File(reference) };
        tree.entries.insert(path.into(), entry);
        self.commit(store, tree)
    }

    /// Rename the entry and structurally rewrite references in every Splash
    /// document. A moved file's relative references retain their targets.
    pub fn rename(&mut self, store: &mut impl TreeStore, from: &str, to: &str) -> Result<AssetRevisionRef, String> {
        self.destination(from, to)?;
        let mut contents = self.contents(store)?;
        let mut changed = BTreeMap::new();
        for (path, content) in &contents {
            if path.ends_with(".splash") {
                let text = std::str::from_utf8(&content.bytes).map_err(|_| format!("{path}: not UTF-8"))?;
                let renamed = rewrite_references(&self.tree, path, text, from, to, path == from)?;
                if renamed.as_bytes() != content.bytes.as_ref() {
                    let mut content = content.clone(); content.bytes = renamed.into_bytes().into();
                    changed.insert(path.clone(), content);
                }
            }
        }
        contents.extend(changed.clone());
        if let Some(content) = contents.remove(from) { contents.insert(to.into(), content); }
        let mut optional = self.tree.optional_paths();
        if optional.remove(from) { optional.insert(to.into()); }
        validate_documents(&contents, &optional)?;
        let mut tree = self.tree.clone();
        for (path, content) in changed {
            let previous = tree.entries.get(&path).and_then(TreeEntry::reference);
            let reference = self.publish_file(store, &path, &content, previous)?;
            let entry = if matches!(tree.entries.get(&path), Some(TreeEntry::Optional(_))) { TreeEntry::Optional(Some(reference)) } else { TreeEntry::File(reference) };
            tree.entries.insert(path, entry);
        }
        let entry = tree.entries.remove(from).ok_or_else(|| format!("no tree entry `{from}`"))?;
        tree.entries.insert(to.into(), entry);
        if let Some((_, root)) = &mut tree.root { if root == from { *root = to.into(); } }
        self.commit(store, tree)
    }

    pub fn delete(&mut self, store: &mut impl TreeStore, path: &str) -> Result<AssetRevisionRef, String> {
        validate_path(path)?;
        let mut tree = self.tree.clone();
        let entry = tree.entries.get_mut(path).ok_or_else(|| format!("no tree entry `{path}`"))?;
        if matches!(entry, TreeEntry::Optional(_)) { *entry = TreeEntry::Optional(None); } else { tree.entries.remove(path); }
        tree.validate()?;
        let mut contents = self.contents(store)?;
        contents.remove(path);
        validate_documents(&contents, &tree.optional_paths())?;
        self.commit(store, tree)
    }

    pub fn copy(&mut self, store: &mut impl TreeStore, from: &str, to: &str) -> Result<AssetRevisionRef, String> {
        self.destination(from, to)?;
        let mut tree = self.tree.clone();
        let entry = tree.entries[from].clone();
        let mut contents = self.contents(store)?;
        let mut changed = None;
        if let Some(mut content) = contents.get(from).cloned() {
            if from.ends_with(".splash") {
                let text = std::str::from_utf8(&content.bytes).map_err(|_| format!("{from}: not UTF-8"))?;
                let copied = rewrite_references(&self.tree, from, text, "", "", true)?;
                if copied.as_bytes() != content.bytes.as_ref() { content.bytes = copied.into_bytes().into(); changed = Some(content.clone()); }
            }
            contents.insert(to.into(), content);
        }
        let mut optional = tree.optional_paths();
        if matches!(entry, TreeEntry::Optional(_)) { optional.insert(to.into()); }
        validate_documents(&contents, &optional)?;
        let entry = if let Some(content) = changed {
            let reference = self.publish_file(store, to, &content, None)?;
            if matches!(entry, TreeEntry::Optional(_)) { TreeEntry::Optional(Some(reference)) } else { TreeEntry::File(reference) }
        } else { entry };
        tree.entries.insert(to.into(), entry);
        self.commit(store, tree)
    }

    fn destination(&self, from: &str, to: &str) -> Result<(), String> {
        validate_path(from)?; validate_path(to)?;
        if !self.tree.entries.contains_key(from) { return Err(format!("no tree entry `{from}`")); }
        if self.tree.entries.contains_key(to) { return Err(format!("tree entry `{to}` already exists")); }
        Ok(())
    }

    fn contents(&self, store: &mut impl TreeStore) -> Result<BTreeMap<String, TreeContent>, String> {
        let mut cache = BTreeMap::new();
        let mut out = BTreeMap::new();
        for (path, entry) in &self.tree.entries {
            if let TreeEntry::File(reference) | TreeEntry::Optional(Some(reference)) = entry {
                if !cache.contains_key(reference) { cache.insert(*reference, store.read(*reference)?); }
                out.insert(path.clone(), cache[reference].clone());
            }
        }
        Ok(out)
    }

    fn publish_file(&self, store: &mut impl TreeStore, path: &str, content: &TreeContent, previous: Option<AssetRevisionRef>) -> Result<AssetRevisionRef, String> {
        let file = content_file(path, &content.bytes, content.kind)?;
        let mut bundle = PublishBundle::new(&self.publication.namespace, content.kind, path, vec![file], self.publication.thumbnail.clone(), content.rights.clone());
        bundle.asset_id = previous.map(|r| r.asset_id);
        store.publish(&bundle)
    }

    fn commit(&mut self, store: &mut impl TreeStore, tree: Tree) -> Result<AssetRevisionRef, String> {
        let mut publication = self.publication.clone();
        publication.files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, tree.serialize()?.into_bytes(), None)];
        publication.dependencies = tree.dependencies();
        publication.expected_head = self.head.map_or(PublishExpectedHead::Absent, PublishExpectedHead::Exact);
        let head = store.publish(&publication)?;
        self.tree = tree;
        self.head = Some(head);
        Ok(head)
    }
}

/// The file role and image dimensions come from the actual bytes, never from
/// a second metadata file alongside a tree.
pub fn content_file(path: &str, bytes: &[u8], kind: AssetKind) -> Result<PublishBundleFile, String> {
    validate_path(path)?;
    let (role, media, dims) = match kind {
        AssetKind::Texture => {
            let (media, dims) = if let Some(dims) = makepad_asset_data::image_dims::png_dims(bytes) { (MediaType::Png, dims) }
                else if let Some(dims) = makepad_asset_data::image_dims::jpeg_dims(bytes) { (MediaType::Jpeg, dims) }
                else { return Err(format!("{path}: unsupported texture encoding")); };
            (FileRole::Texture, media, Some(dims))
        },
        AssetKind::Deck | AssetKind::Cut | AssetKind::Motion | AssetKind::Score | AssetKind::Storyboard | AssetKind::Flow => {
            std::str::from_utf8(bytes).map_err(|_| format!("{path}: not UTF-8"))?;
            (FileRole::Source, MediaType::Text, None)
        },
        // A clip a cut plays (`Video{src: "media/x.mp4"}`): the MP4 itself, its
        // length read by the player.
        AssetKind::Video => {
            if bytes.get(4..8) != Some(b"ftyp".as_slice()) { return Err(format!("{path}: not an MP4 file")); }
            (FileRole::Video, MediaType::Mp4, None)
        },
        AssetKind::Data => (FileRole::Source, if std::str::from_utf8(bytes).is_ok() { MediaType::Text } else { MediaType::Bin }, None),
        _ => return Err(format!("{path}: this file kind needs its typed media publisher")),
    };
    Ok(PublishBundleFile::bytes(role, media, bytes.to_vec(), dims))
}

/// Literal path arguments and `src` fields, from parsed nodes only.
pub fn references(tree: &syntax::Syntax) -> Result<Vec<(std::ops::Range<usize>, String)>, String> {
    let mut refs = Vec::new();
    let mut errors = Vec::new();
    tree.walk(&mut |_, node| {
        if let syntax::Kind::Call { callee, args, .. } = &node.kind {
            let callee = callee.ident().unwrap_or("");
            let name = ["mod.cut.", "mod.deck.", "mod.motion.", "mod.forms."].iter().find_map(|prefix| callee.strip_prefix(*prefix)).unwrap_or(callee);
            if matches!(name, "asset" | "run" | "cut") {
                let (line, col) = tree.line_col(node.span.start);
                errors.push(format!("{line}:{col}: `{name}()` library references are forbidden in a tree document"));
            }
            if matches!(name, "graphic" | "import") {
                if let [arg] = args.as_slice() {
                    if let Some(path) = arg.string() { refs.push((arg.span.clone(), path.into())); return; }
                }
                let (line, col) = tree.line_col(node.span.start);
                errors.push(format!("{line}:{col}: `{name}` takes one literal tree path"));
            }
        }
        if let Some((ty, fields)) = node.object() {
            // A deck's `Slide{link: "talk.deck.splash"}` names another deck of the tree.
            let slide = ty.as_deref().is_some_and(|ty| ty.rsplit('.').next() == Some("Slide"));
            for field in fields.iter().filter(|field| matches!(field.name.as_deref(), Some("src" | "graphic")) || (slide && field.name.as_deref() == Some("link"))) {
                if let Some(path) = field.value.string() { refs.push((field.value.span.clone(), path.into())); }
            }
        }
    });
    if errors.is_empty() { Ok(refs) } else { Err(errors.join("\n")) }
}

pub fn validate_documents(contents: &BTreeMap<String, TreeContent>, optional: &BTreeSet<String>) -> Result<(), String> {
    let paths: BTreeSet<_> = contents.keys().chain(optional.iter()).cloned().collect();
    for path in &paths {
        validate_path(path)?;
        for (end, _) in path.match_indices('/') {
            if paths.contains(&path[..end]) { return Err(format!("`{path}` uses a file as a folder")); }
        }
    }
    for (path, content) in contents {
        validate_path(path)?;
        if matches!(content.kind, AssetKind::Deck | AssetKind::Cut | AssetKind::Motion | AssetKind::Score | AssetKind::Storyboard | AssetKind::Flow) && !path.ends_with(".splash") {
            return Err(format!("{path}: a Splash document requires a .splash extension"));
        }
        if !path.ends_with(".splash") { continue; }
        let text = std::str::from_utf8(&content.bytes).map_err(|_| format!("{path}: not UTF-8"))?;
        let syntax = syntax::parse(text).map_err(|e| format!("{path}: {e}"))?;
        for (span, reference) in references(&syntax).map_err(|e| format!("{path}:{e}"))? {
            let (line, col) = syntax.line_col(span.start);
            let canonical = reference_path(path, &reference).map_err(|e| format!("{path}:{line}:{col}: {e}"))?;
            let count = if paths.contains(&canonical) { 1 } else if canonical.rsplit('/').next().is_some_and(|name| name.contains('.')) { 0 } else {
                let prefix = format!("{canonical}.");
                paths.range(prefix.clone()..).take_while(|p| p.starts_with(&prefix)).take(2).count()
            };
            if count != 1 {
                return Err(format!("{path}:{line}:{col}: {} path `{reference}`", if count == 0 { "dangling" } else { "ambiguous" }));
            }
        }
    }
    Ok(())
}

fn rewrite_references(tree: &Tree, path: &str, text: &str, from: &str, to: &str, moved: bool) -> Result<String, String> {
    rewrite_paths(path, text, from, to, moved, |reference| tree.resolve(path, reference).map(|(path, _)| path.to_string()))
}

fn rewrite_paths(path: &str, text: &str, from: &str, to: &str, moved: bool, resolve: impl Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let syntax = syntax::parse(text).map_err(|e| format!("{path}: {e}"))?;
    let mut edits = Vec::new();
    for (span, reference) in references(&syntax)? {
        let target = resolve(&reference)?;
        if target == from || (moved && reference.starts_with('.')) {
            let target = if target == from { to } else { &target };
            edits.push(Replace::new(span, syntax::write::quote(target)));
        }
    }
    syntax.apply(&edits).map(|tree| tree.print()).map_err(|e| e.to_string())
}

pub fn validate_draft(draft: &TreeDraft) -> Result<(), String> {
    if !draft.files.contains_key(&draft.root.1) { return Err(format!("tree root `{}` is missing", draft.root.1)); }
    let contents = draft.files.iter().map(|(path, file)| (path.clone(), TreeContent {
        kind: file.kind, bytes: file.bytes.clone(), rights: PublishRights::generated_cc0(),
    })).collect();
    if draft.optional.contains(&draft.root.1) { return Err("the root cannot be optional".into()); }
    validate_documents(&contents, &draft.optional)
}

/// Rename/copy preserve references through the same parsed spans as store edits.
/// The caller installs the result only after its document loader accepts it.
pub fn rename_draft(draft: &TreeDraft, from: &str, to: &str, copy: bool) -> Result<TreeDraft, String> {
    validate_path(from)?;
    validate_path(to)?;
    let file = draft.files.get(from);
    if file.is_none() && !draft.optional.contains(from) { return Err(format!("no file `{from}`")); }
    if draft.files.contains_key(to) || draft.optional.contains(to) { return Err(format!("file `{to}` already exists")); }
    let mut next = draft.clone();
    let mut moved = file.cloned();
    if let Some(file) = &mut moved { file.path = to.into(); }
    if draft.optional.contains(from) { next.optional.insert(to.into()); }
    for (path, content) in &draft.files {
        if !path.ends_with(".splash") || (copy && path != from) { continue; }
        let text = rewrite_paths(path, content.text()?, if copy { "" } else { from }, to, path == from, |reference| draft.lookup(path, reference).map(|entry| match entry { TreeLookup::File(file) => file.path, TreeLookup::MissingOptional(path) => path }))?;
        if path == from {
            if let Some(file) = &mut moved {
                if text.as_bytes() != file.bytes.as_ref() { file.bytes = text.into_bytes().into(); file.reference = None; }
            }
        } else {
            next.write(path, content.kind, text.into_bytes().into())?;
        }
    }
    if let Some(file) = moved { next.files.insert(to.into(), file); }
    if !copy {
        next.files.remove(from);
        next.optional.remove(from);
        if next.root.1 == from { next.root.1 = to.into(); }
    }
    validate_draft(&next)?;
    Ok(next)
}
