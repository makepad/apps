//! Path-based documents loaded against one immutable project snapshot.

pub use makepad_asset_data::tree::{MemoryTree, TreeContext, TreeFile, TreeLookup, TreeResolver};
use crate::modules::{ModuleResolver, ModuleSource, Modules};
use makepad_script::*;
use std::sync::Arc;

/// Read a local project for offline tools. Handles are content-addressed within
/// this snapshot, never followed in a store. Publishing uses the tree editor.
///
/// The files a folder holds as a tree, by logical path: everything but hidden
/// files and what the folder's own `.gitignore` excludes (a person's local
/// copies, script caches, stand-in outputs a checkout may hold). The one
/// reader of folder trees (bundled examples, tests, the player bundle, a
/// player in the browser), read through [`makepad_asset_data::source`].
pub fn folder_files(dir: &std::path::Path) -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
    use makepad_asset_data::source::{self, EntryKind};
    let ignored = Ignored::read(dir);
    fn visit(dir: &std::path::Path, base: &std::path::Path, ignored: &Ignored, out: &mut std::collections::BTreeMap<String, Vec<u8>>) -> Result<(), String> {
        let mut entries = source::read_dir(dir).map_err(|e| e.to_string())?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            let path = dir.join(&entry.name);
            let logical = path.strip_prefix(base).map_err(|e| e.to_string())?.to_str().ok_or("tree path is not UTF-8")?.replace('\\', "/");
            if entry.name.starts_with('.') || ignored.matches(&logical, entry.is_dir()) { continue; }
            if entry.kind == EntryKind::Symlink { return Err(format!("{}: a tree cannot contain symlinks", path.display())); }
            if entry.is_dir() { visit(&path, base, ignored, out)?; continue; }
            if !entry.is_file() { continue; }
            makepad_asset_data::tree::validate_path(&logical)?;
            out.insert(logical, source::read(&path).map_err(|e| e.to_string())?);
        }
        Ok(())
    }
    let mut files = std::collections::BTreeMap::new();
    visit(dir, dir, &ignored, &mut files)?;
    Ok(files)
}

/// The `.gitignore` patterns the examples use: `name`, `name/` (a folder),
/// `*suffix`, matched on the last path component (as git does for patterns
/// without a slash).
struct Ignored(Vec<String>);

impl Ignored {
    fn read(dir: &std::path::Path) -> Self {
        let gitignore = dir.join(".gitignore");
        let text = if makepad_asset_data::source::is_file(&gitignore) { makepad_asset_data::source::read_to_string(&gitignore).unwrap_or_default() } else { String::new() };
        Self(text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_string).collect())
    }

    fn matches(&self, logical: &str, is_dir: bool) -> bool {
        let name = logical.rsplit('/').next().unwrap_or(logical);
        self.0.iter().any(|pattern| match pattern.strip_suffix('/') {
            Some(folder) => is_dir && name == folder,
            None => match pattern.strip_prefix('*') {
                Some(suffix) => name.ends_with(suffix),
                None => name == pattern,
            },
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_folder(dir: &std::path::Path, root: &str, role: makepad_asset_data::tree::TreeRole) -> Result<MemoryTree, String> {
    memory_tree(folder_files(dir)?, root, role)
}

/// The tree of `files` (logical path -> bytes, as [`folder_files`] reads a
/// folder), rooted at `root`.
pub fn memory_tree(files: std::collections::BTreeMap<String, Vec<u8>>, root: &str, role: makepad_asset_data::tree::TreeRole) -> Result<MemoryTree, String> {
    use makepad_asset_data::{tree::{Tree, TreeEntry}, AssetId, AssetRevisionId, AssetRevisionRef, BlobId};
    fn visit(files: std::collections::BTreeMap<String, Vec<u8>>, out: &mut MemoryTree) -> Result<(), String> {
        for (logical, bytes) in files {
            if logical.ends_with(".tree") {
                let declarations = Tree::parse(std::str::from_utf8(&bytes).map_err(|_| "tree descriptor is not UTF-8")?)?;
                if declarations.root.is_some() || declarations.entries.values().any(|entry| !matches!(entry, TreeEntry::Optional(None))) {
                    return Err(format!("{logical}: a folder descriptor only declares optional paths"));
                }
                for (path, _) in declarations.entries {
                    if matches!(out.tree.entries.get(&path), Some(TreeEntry::Optional(_))) { return Err(format!("duplicate optional path `{path}`")); }
                    let reference = out.tree.entries.get(&path).and_then(TreeEntry::reference);
                    out.tree.entries.insert(path, TreeEntry::Optional(reference));
                }
            }
            let kind = file_kind(&logical);
            let reference = AssetRevisionRef {
                asset_id: AssetId::from_bytes(BlobId::hash_of(logical.as_bytes()).as_bytes()[..16].try_into().unwrap()),
                revision: AssetRevisionId::hash_of(&bytes),
            };
            let entry = if matches!(out.tree.entries.get(&logical), Some(TreeEntry::Optional(_))) { TreeEntry::Optional(Some(reference)) } else { TreeEntry::File(reference) };
            out.tree.entries.insert(logical, entry);
            out.files.insert(reference, (kind, bytes.into()));
        }
        Ok(())
    }
    let mut snapshot = MemoryTree::default();
    visit(files, &mut snapshot)?;
    snapshot.tree.root = Some((role, root.into()));
    snapshot.tree.validate()?;
    Ok(snapshot)
}

/// File kinds used by local project tools and bundled tree readers.
pub fn file_kind(path: &str) -> makepad_asset_data::AssetKind {
    use makepad_asset_data::AssetKind;
    if path.ends_with(".cut.splash") { AssetKind::Cut }
    else if path.ends_with(".deck.splash") || path == "deck.splash" { AssetKind::Deck }
    else if path.ends_with(".splash") { AssetKind::Motion }
    else { match path.rsplit('.').next().unwrap_or("") {
        "png" | "jpg" | "jpeg" => AssetKind::Texture,
        "mp3" | "wav" | "ogg" | "m4a" => AssetKind::Audio,
        "mp4" | "webm" => AssetKind::Video,
        _ => AssetKind::Data,
    } }
}

struct TreeModules(TreeContext);

impl ModuleResolver for TreeModules {
    fn document_file(&self) -> Option<&str> { Some(&self.0.document) }
    fn path(&self, from: &str, path: &str) -> Result<ModuleSource, String> {
        let context = if from.is_empty() { self.0.clone() } else { self.0.at(from) };
        let file = context.resolve(path)?;
        if file.kind != makepad_asset_data::AssetKind::Motion { return Err(format!("{}: an imported module must be Motion source", file.path)); }
        Ok(ModuleSource::new(&file.path, &makepad_asset_data::BlobId::hash_of(&file.bytes).to_string(), file.text()?).with_file(&file.path))
    }

    fn validate(&self, source: &str, file: &str) -> Result<(), Vec<String>> {
        let context = if file.is_empty() { self.0.clone() } else { self.0.at(file) };
        validate(source, &context).map_err(|errors| errors.into_iter().map(|e| format!("{}:{}:{}: {}", context.document, e.line, e.col, e.message)).collect())
    }

    fn install(&self, vm: &mut ScriptVm, module: ScriptObject) {
        let context = self.0.clone();
        vm.add_method(module, id_lut!(graphic), script_args!(path = NIL), move |vm, args| {
            let value = vm.value(args, id!(path));
            let path = value.as_string().map(|s| vm.heap().string(s).to_string()).or_else(|| value.as_inline_string(str::to_string));
            let Some(path) = path else { return script_err_invalid_args!(vm.trap(), "graphic takes a tree path string"); };
            let caller = vm.bx.code_ref().ip_to_loc(vm.trap_ip()).map(|loc| loc.file().to_string());
            let from = caller.as_deref().filter(|f| f.ends_with(".splash") && !f.starts_with('<'));
            let file = match from.map_or_else(|| context.lookup(&path), |from| context.at(from).lookup(&path)) {
                Ok(TreeLookup::File(file)) => file,
                Ok(TreeLookup::MissingOptional(path)) => {
                    let path = vm.bx.heap_mut().new_string_from_str(&path);
                    return crate::layers::make(vm, "MissingMedia", &[(id_lut!(path), path.into())]).into();
                },
                Err(error) => return script_err_invalid_args!(vm.trap(), "{}", error),
            };
            let Some(reference) = file.reference else { return script_err_invalid_args!(vm.trap(), "{}: media must be imported before it can be drawn", file.path); };
            let id = vm.bx.heap_mut().new_string_from_str(&reference.asset_id.to_string());
            let rev = vm.bx.heap_mut().new_string_from_str(&reference.revision.to_string());
            crate::layers::make(vm, "Asset", &[(id_lut!(id), id.into()), (id_lut!(rev), rev.into())]).into()
        });
        vm.add_method(module, id_lut!(asset), script_args!(id = NIL, rev = NIL), |vm, _| {
            script_err_invalid_args!(vm.trap(), "asset() is forbidden inside a tree; use graphic(path)")
        });
    }
}

impl Modules {
    pub fn from_tree(context: TreeContext) -> Self { Self(Arc::new(TreeModules(context))) }
}

#[derive(Clone, Debug)]
pub struct PathDiagnostic {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

/// Inspect parsed calls and fields, including references in inactive branches.
/// The original objects still go to the compiler, without source rewriting.
pub fn validate(source: &str, context: &TreeContext) -> Result<(), Vec<PathDiagnostic>> {
    let syntax = syntax::parse(source).map_err(|e| {
        let (line, col) = syntax::line_col(source, e.at);
        vec![PathDiagnostic { line: line as u32, col: col as u32, message: e.message }]
    })?;
    let mut errors = Vec::new();
    syntax.walk(&mut |_, node| {
        let mut check = |at: usize, path: &str| {
            if let Err(message) = context.lookup(path) {
                let (line, col) = syntax.line_col(at);
                errors.push(PathDiagnostic { line: line as u32, col: col as u32, message });
            }
        };
        if let Some((_, fields)) = node.object() {
            for field in fields.iter().filter(|f| matches!(f.name.as_deref(), Some("src" | "graphic"))) {
                if let Some(path) = field.value.string() { check(field.value.span.start, path); }
            }
        }
        if let syntax::Kind::Call { callee, args, .. } = &node.kind {
            let callee = callee.ident().unwrap_or("");
            let name = ["mod.cut.", "mod.deck.", "mod.motion.", "mod.forms."].iter().find_map(|prefix| callee.strip_prefix(*prefix)).unwrap_or(callee);
            if matches!(name, "graphic" | "import") {
                if let [arg] = args.as_slice() {
                    if let Some(path) = arg.string() { check(arg.span.start, path); return; }
                }
                let (line, col) = syntax.line_col(node.span.start);
                errors.push(PathDiagnostic { line: line as u32, col: col as u32, message: format!("{name} takes one literal tree path") });
            } else if matches!(name, "asset" | "cut" | "run") {
                let (line, col) = syntax.line_col(node.span.start);
                errors.push(PathDiagnostic { line: line as u32, col: col as u32, message: format!("{name}() library references are forbidden in a tree document; use graphic(path)") });
            }
        }
    });
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
