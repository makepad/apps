//! A project revision: a sorted map of logical file paths to exact content.

use crate::{AssetAlias, AssetKind, AssetRevisionRef};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEntry {
    File(AssetRevisionRef),
    /// `optional <path>` declares absent media; `optional <path> <asset@revision>`
    /// fills it. Absence has no dependency edge and is distinct from missing
    /// bytes for a pinned revision, which remains an error.
    Optional(Option<AssetRevisionRef>),
    /// Reserved cross-tree reference. Kept pinned for dependency collection;
    /// loading links is deliberately not implemented yet.
    Link { alias: AssetAlias, path: String, reference: AssetRevisionRef },
}

impl TreeEntry {
    pub fn reference(&self) -> Option<AssetRevisionRef> {
        match self {
            Self::File(reference) | Self::Link { reference, .. } => Some(*reference),
            Self::Optional(reference) => *reference,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRole { Deck, Cut, Score, Desk }

impl TreeRole {
    pub fn as_str(self) -> &'static str {
        match self { Self::Deck => "deck", Self::Cut => "cut", Self::Score => "score", Self::Desk => "desk" }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub name: String,
    pub root: Option<(TreeRole, String)>,
    pub entries: BTreeMap<String, TreeEntry>,
}

/// Logical paths are portable and case-sensitive. Directories are implicit;
/// only the final segment has an extension (including `*.deck.splash`).
pub fn validate_path(path: &str) -> Result<(), String> {
    fn word(s: &str) -> bool {
        !s.is_empty() && s.as_bytes()[0].is_ascii_lowercase()
            && s.split('_').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
    }
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        let valid = if segments.peek().is_none() {
            let parts: Vec<_> = segment.split('.').collect();
            parts.len() >= 2 && parts.iter().all(|part| word(part))
        } else { word(segment) };
        if !valid { return Err(format!("invalid tree path `{path}`: use relative snake_case folders and a file extension")); }
    }
    Ok(())
}

/// Unadorned references start at the tree root; `./` and `../` are relative
/// to the referring file. A reference can omit the final extension.
pub fn reference_path(from: &str, path: &str) -> Result<String, String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(format!("invalid tree reference `{path}`"));
    }
    let relative = path.starts_with("./") || path.starts_with("../");
    let mut parts: Vec<&str> = if relative { from.rsplit_once('/').map_or(Vec::new(), |(dir, _)| dir.split('/').collect()) } else { Vec::new() };
    for part in path.split('/') {
        match part {
            "." if relative => {},
            ".." if relative => { if parts.pop().is_none() { return Err(format!("tree reference `{path}` escapes the tree")); } },
            "" | "." | ".." => return Err(format!("invalid tree reference `{path}`")),
            part => parts.push(part),
        }
    }
    let path = parts.join("/");
    if path.rsplit('/').next().is_some_and(|name| name.contains('.')) { validate_path(&path)?; }
    else { validate_path(&format!("{path}.splash"))?; }
    Ok(path)
}

impl Tree {
    /// Validate the source blob against the store manifest. GC only needs the
    /// manifest's ordinary dependency edges after this check.
    pub fn check_manifest(manifest: &crate::AssetManifest, bytes: &[u8]) -> Result<Self, String> {
        if manifest.kind != AssetKind::Tree { return Err("expected a Tree manifest".into()); }
        manifest.validate().map_err(|e| format!("invalid tree asset: {e:?}"))?;
        let file = &manifest.files[0];
        if file.byte_len != bytes.len() as u64 || file.blob != crate::BlobId::hash_of(bytes) { return Err("tree manifest blob identity mismatch".into()); }
        let tree = Self::parse(std::str::from_utf8(bytes).map_err(|_| "tree manifest is not UTF-8")?)?;
        if tree.dependencies() != manifest.dependencies { return Err("tree dependencies do not equal its entries".into()); }
        Ok(tree)
    }

    pub fn parse(source: &str) -> Result<Self, String> {
        let mut tree = Self::default();
        let mut named = false;
        for (line, text) in source.lines().enumerate() {
            let text = text.trim();
            if text.is_empty() { continue; }
            let parse = || -> Result<_, String> {
                let fields: Vec<_> = text.split_whitespace().collect();
                match fields.as_slice() {
                    ["file", path, reference] => Ok(Some(((*path).to_string(), TreeEntry::File(parse_ref(reference)?)))),
                    ["optional", path] => Ok(Some(((*path).to_string(), TreeEntry::Optional(None)))),
                    ["optional", path, reference] => Ok(Some(((*path).to_string(), TreeEntry::Optional(Some(parse_ref(reference)?))))),
                    ["link", path, alias, target, reference] => {
                        validate_path(target)?;
                        Ok(Some(((*path).to_string(), TreeEntry::Link {
                            alias: alias.parse().map_err(|e| format!("invalid tree alias: {e:?}"))?,
                            path: (*target).into(), reference: parse_ref(reference)?,
                        })))
                    },
                    _ => Err("expected `file <path> <asset@revision>`, `optional <path> [<asset@revision>]` or `link <path> <alias> <path> <asset@revision>`".into()),
                }
            };
            if let Some(name) = text.strip_prefix("tree ") {
                if named { return Err(format!("line {}: duplicate tree name", line + 1)); }
                tree.name = unquote(name).map_err(|e| format!("line {}: {e}", line + 1))?;
                named = true;
                continue;
            }
            let root = text.split_once(char::is_whitespace).and_then(|(role, path)| Some((match role {
                "deck" => TreeRole::Deck, "cut" => TreeRole::Cut, "score" => TreeRole::Score, "desk" => TreeRole::Desk, _ => return None,
            }, path.trim().to_string())));
            if let Some(root) = root {
                if tree.root.replace(root).is_some() { return Err(format!("line {}: duplicate root", line + 1)); }
                continue;
            }
            if let Some((path, entry)) = parse().map_err(|e| format!("line {}: {e}", line + 1))? {
                validate_path(&path).map_err(|e| format!("line {}: {e}", line + 1))?;
                if tree.entries.insert(path.clone(), entry).is_some() { return Err(format!("line {}: duplicate path `{path}`", line + 1)); }
            }
        }
        tree.validate()?;
        Ok(tree)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.chars().any(char::is_control) { return Err("a tree name cannot contain control characters".into()); }
        for (path, entry) in &self.entries {
            validate_path(path)?;
            if let TreeEntry::Link { path, .. } = entry { validate_path(path)?; }
            for (end, _) in path.match_indices('/') {
                if self.entries.contains_key(&path[..end]) { return Err(format!("`{path}` uses a file as a folder")); }
            }
        }
        if let Some((_, path)) = &self.root {
            validate_path(path)?;
            if !matches!(self.entries.get(path), Some(TreeEntry::File(_))) { return Err(format!("tree root `{path}` is not a file")); }
        }
        Ok(())
    }

    pub fn serialize(&self) -> Result<String, String> {
        self.validate()?;
        let mut out = String::new();
        out.push_str(&format!("tree \"{}\"\n", self.name.replace('\\', "\\\\").replace('"', "\\\"")));
        if let Some((role, path)) = &self.root { out.push_str(&format!("{} {path}\n", role.as_str())); }
        for (path, entry) in &self.entries {
            match entry {
                TreeEntry::File(reference) => out.push_str(&format!("file {path} {reference}\n")),
                TreeEntry::Optional(None) => out.push_str(&format!("optional {path}\n")),
                TreeEntry::Optional(Some(reference)) => out.push_str(&format!("optional {path} {reference}\n")),
                TreeEntry::Link { alias, path: target, reference } => out.push_str(&format!("link {path} {alias} {target} {reference}\n")),
            }
        }
        Ok(out)
    }

    pub fn dependencies(&self) -> Vec<AssetRevisionRef> {
        let mut deps: Vec<_> = self.entries.values().filter_map(TreeEntry::reference).collect();
        deps.sort(); deps.dedup(); deps
    }

    pub fn optional_paths(&self) -> BTreeSet<String> {
        self.entries.iter().filter(|(_, entry)| matches!(entry, TreeEntry::Optional(_))).map(|(path, _)| path.clone()).collect()
    }

    pub fn resolve(&self, from: &str, path: &str) -> Result<(&str, &TreeEntry), String> {
        let path = reference_path(from, path)?;
        if let Some((path, entry)) = self.entries.get_key_value(&path) { return Ok((path, entry)); }
        if !path.rsplit('/').next().unwrap_or("").contains('.') {
            let prefix = format!("{path}.");
            let mut candidates = self.entries.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix));
            if let Some((path, entry)) = candidates.next() {
                if candidates.next().is_none() { return Ok((path, entry)); }
                return Err(format!("ambiguous tree path `{prefix}`: include the extension"));
            }
        }
        Err(format!("dangling tree path `{path}`"))
    }
}

fn parse_ref(text: &str) -> Result<AssetRevisionRef, String> {
    let (id, revision) = text.split_once('@').ok_or("a revision reference is asset@revision")?;
    Ok(AssetRevisionRef { asset_id: id.parse().map_err(|e| format!("invalid asset: {e:?}"))?, revision: revision.parse().map_err(|e| format!("invalid revision: {e:?}"))? })
}

fn unquote(text: &str) -> Result<String, String> {
    let inner = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')).ok_or("a tree name is quoted")?;
    let mut chars = inner.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() { Some(c @ ('\\' | '"')) => out.push(c), _ => return Err("invalid escape in tree name".into()) },
            '"' => return Err("unescaped quote in tree name".into()),
            c => out.push(c),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct TreeFile {
    pub path: String,
    /// None for a new or edited file which has not been published yet.
    pub reference: Option<AssetRevisionRef>,
    /// Content this draft was derived from, retained across edits and renames.
    pub base: Option<AssetRevisionRef>,
    pub kind: AssetKind,
    pub bytes: Arc<[u8]>,
}

impl TreeFile {
    pub fn text(&self) -> Result<&str, String> {
        std::str::from_utf8(&self.bytes).map_err(|_| format!("{}: not UTF-8", self.path))
    }
}

/// A resolver owns one immutable tree revision. It must never follow heads.
/// Loaders receive bytes already fetched on the host's worker.
pub trait TreeResolver: Send + Sync {
    fn lookup(&self, from: &str, path: &str) -> Result<TreeLookup, String>;
    fn missing_optional(&self) -> Vec<String>;

    /// Documents and imports require content. Media loaders use `lookup` so
    /// declared absence is distinct from a broken reference or failed fetch.
    fn resolve(&self, from: &str, path: &str) -> Result<TreeFile, String> {
        match self.lookup(from, path)? {
            TreeLookup::File(file) => Ok(file),
            TreeLookup::MissingOptional(path) => Err(format!("media not present: {path}")),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TreeLookup {
    File(TreeFile),
    MissingOptional(String),
}

#[derive(Clone)]
pub struct TreeContext {
    pub resolver: Arc<dyn TreeResolver>,
    pub document: String,
}

impl TreeContext {
    pub fn resolve(&self, path: &str) -> Result<TreeFile, String> { self.resolver.resolve(&self.document, path) }
    pub fn lookup(&self, path: &str) -> Result<TreeLookup, String> { self.resolver.lookup(&self.document, path) }
    pub fn at(&self, document: impl Into<String>) -> Self { Self { resolver: self.resolver.clone(), document: document.into() } }
}

impl std::fmt::Debug for TreeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("TreeContext").field("document", &self.document).finish() }
}
impl PartialEq for TreeContext {
    fn eq(&self, other: &Self) -> bool { self.document == other.document && Arc::ptr_eq(&self.resolver, &other.resolver) }
}

#[derive(Clone, Debug, Default)]
pub struct MemoryTree {
    pub tree: Tree,
    pub files: BTreeMap<AssetRevisionRef, (AssetKind, Arc<[u8]>)>,
}

impl TreeResolver for MemoryTree {
    fn missing_optional(&self) -> Vec<String> {
        self.tree.entries.iter().filter(|(_, entry)| matches!(entry, TreeEntry::Optional(None))).map(|(path, _)| path.clone()).collect()
    }
    fn lookup(&self, from: &str, path: &str) -> Result<TreeLookup, String> {
        let (path, entry) = self.tree.resolve(from, path)?;
        let reference = match entry {
            TreeEntry::File(reference) | TreeEntry::Optional(Some(reference)) => reference,
            TreeEntry::Optional(None) => return Ok(TreeLookup::MissingOptional(path.into())),
            TreeEntry::Link { .. } => return Err(format!("{path}: cross-tree links are reserved")),
        };
        let (kind, bytes) = self.files.get(reference).ok_or_else(|| format!("{path}: content revision is unavailable"))?;
        Ok(TreeLookup::File(TreeFile { path: path.into(), reference: Some(*reference), base: Some(*reference), kind: *kind, bytes: bytes.clone() }))
    }
}

/// An editor's immutable working snapshot. Bytes are shared between snapshots;
/// unpublished documents have paths and bytes, never invented store revisions.
#[derive(Clone, Debug)]
pub struct TreeDraft {
    pub name: String,
    pub root: (TreeRole, String),
    pub files: BTreeMap<String, TreeFile>,
    /// Declarations persist when content is filled, edited, or removed.
    pub optional: BTreeSet<String>,
}

impl PartialEq for TreeDraft {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.root == other.root && self.optional == other.optional && self.files.len() == other.files.len()
            && self.files.iter().all(|(path, file)| other.files.get(path).is_some_and(|other| file.kind == other.kind && (Arc::ptr_eq(&file.bytes, &other.bytes) || file.bytes == other.bytes)))
    }
}

impl TreeDraft {
    pub fn from_memory(memory: &MemoryTree) -> Result<Self, String> {
        let root = memory.tree.root.clone().ok_or("the tree has no root document")?;
        let files = memory.tree.entries.iter().filter(|(_, entry)| !matches!(entry, TreeEntry::Optional(None))).map(|(path, _)| memory.resolve("", path).map(|file| (path.clone(), file))).collect::<Result<_, _>>()?;
        Ok(Self { name: memory.tree.name.clone(), root, files, optional: memory.tree.optional_paths() })
    }

    pub fn new(name: String, role: TreeRole, path: String, kind: AssetKind, text: String) -> Result<Self, String> {
        let mut draft = Self { name, root: (role, path.clone()), files: BTreeMap::new(), optional: BTreeSet::new() };
        draft.write(&path, kind, text.into_bytes().into())?;
        Ok(draft)
    }

    pub fn write(&mut self, path: &str, kind: AssetKind, bytes: Arc<[u8]>) -> Result<(), String> {
        validate_path(path)?;
        if self.files.get(path).is_some_and(|file| file.kind == kind && file.bytes == bytes) { return Ok(()); }
        let base = self.files.get(path).and_then(|file| file.base);
        self.files.insert(path.into(), TreeFile { path: path.into(), reference: None, base, kind, bytes });
        Ok(())
    }

    pub fn context(self: &Arc<Self>, path: &str) -> TreeContext {
        TreeContext { resolver: self.clone(), document: path.into() }
    }

    pub fn text(&self, path: &str) -> Result<&str, String> {
        self.files.get(path).ok_or_else(|| format!("no file `{path}`"))?.text()
    }
}

impl TreeResolver for TreeDraft {
    fn missing_optional(&self) -> Vec<String> {
        self.optional.iter().filter(|path| !self.files.contains_key(*path)).cloned().collect()
    }
    fn lookup(&self, from: &str, path: &str) -> Result<TreeLookup, String> {
        let path = reference_path(from, path)?;
        if let Some(file) = self.files.get(&path) { return Ok(TreeLookup::File(file.clone())); }
        if self.optional.contains(&path) { return Ok(TreeLookup::MissingOptional(path)); }
        if !path.rsplit('/').next().unwrap_or("").contains('.') {
            let prefix = format!("{path}.");
            let candidates: BTreeSet<_> = self.files.range(prefix.clone()..).map(|(name, _)| name).take_while(|name| name.starts_with(&prefix))
                .chain(self.optional.range(prefix.clone()..).take_while(|name| name.starts_with(&prefix))).collect();
            if let Some(path) = candidates.first() {
                if candidates.len() == 1 { return self.lookup("", path); }
                return Err(format!("ambiguous tree path `{path}`: include the extension"));
            }
        }
        Err(format!("dangling tree path `{path}`"))
    }
}
