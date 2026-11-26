//! Immutable crash-copy contents selected together by one atomically replaced map.
//!
//! Each content file is named by the hash of what Fab wrote in it. One that is not (a file
//! changed outside Fab) is the person's: it is read as it reads, and nothing here removes or
//! writes over it. A working copy that does not read back is kept whole in `kept-aside/`.

use crate::AUTOSAVE_FILE;
use fab_model::program::{Assets, FacetAssets, SCRIPT_EXTENSION, STROKES_EXTENSION};
use fab_model::picture::{Picture, Pictures, PICTURE_EXTENSION};
use fab_model::{project, Docs, ElecAssets, ElecMemory, FabModel, Head};
use makepad_csg_sdf::mesh_field::MeshField;
use fab_paint_field::{Facets, Strokes};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

enum Content {
    Text(String),
    Strokes(Arc<Strokes>),
    Mesh(Arc<MeshField>),
    Facets(Arc<Facets>),
    Picture(Arc<Picture>),
    /// An electronics document ([`FabModel::docs`]).
    Doc(Arc<str>),
    /// A binary electronics file ([`FabModel::elec_assets`]).
    Asset(Arc<[u8]>),
    /// A device memory ([`FabModel::elec_memory`]).
    Memory(Arc<[u8]>),
}

/// An electronics document's extension in the working copy (`scripts/<hash>.elec.splash`); its
/// map name is `elec/<file>`.
const DOC_EXTENSION: &str = "elec.splash";

/// Binary electronics content uses its own extension, even if its bytes match a document's.
const ASSET_EXTENSION: &str = "elec.bin";

/// Device memory content uses its own extension, even if its bytes match an asset's.
const MEMORY_EXTENSION: &str = "elec.mem";

impl Content {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Strokes(a), Self::Strokes(b)) => Arc::ptr_eq(a, b),
            (Self::Mesh(a), Self::Mesh(b)) => Arc::ptr_eq(a, b),
            (Self::Facets(a), Self::Facets(b)) => Arc::ptr_eq(a, b),
            (Self::Picture(a), Self::Picture(b)) => Arc::ptr_eq(a, b),
            (Self::Doc(a), Self::Doc(b)) => Arc::ptr_eq(a, b),
            (Self::Asset(a), Self::Asset(b)) => Arc::ptr_eq(a, b),
            (Self::Memory(a), Self::Memory(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn bytes(&self) -> Cow<'_, [u8]> {
        match self {
            Self::Text(text) => Cow::Borrowed(text.as_bytes()),
            Self::Strokes(log) => Cow::Owned(log.to_bytes()),
            Self::Mesh(mesh) => Cow::Owned(project::mesh_bytes(&mesh.data)),
            Self::Facets(facets) => Cow::Owned(project::facets_bytes(facets)),
            Self::Picture(picture) => Cow::Borrowed(&picture.bytes[..]),
            Self::Doc(text) => Cow::Borrowed(text.as_bytes()),
            Self::Asset(bytes) => Cow::Borrowed(bytes),
            Self::Memory(bytes) => Cow::Borrowed(bytes),
        }
    }
}

#[derive(Clone)]
struct SavedFile {
    hash: String,
    content: Arc<Content>,
}

#[derive(Default)]
pub(crate) struct Snapshot {
    pub head: Option<Head>,
    name: String,
    path: Option<PathBuf>,
    assets: Option<Arc<Assets>>,
    facets: Option<Arc<FacetAssets>>,
    pictures: Option<Arc<Pictures>>,
    docs: Option<Arc<Docs>>,
    elec_assets: Option<Arc<ElecAssets>>,
    elec_memory: Option<Arc<ElecMemory>>,
    files: BTreeMap<String, SavedFile>,
}

fn hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in bytes {
        hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn encode(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

pub(crate) fn decode(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            match chars.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            }
        } else { c });
    }
    Some(out)
}

fn content_path(dir: &Path, name: &str, hash: &str) -> Option<PathBuf> {
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    let (folder, ext) = if name == AUTOSAVE_FILE {
        (dir.join("scripts"), project::EXTENSION)
    } else if name == "meta" {
        (dir.join("scripts"), "meta")
    } else if let Some(file) = name.strip_prefix("scripts/") {
        project::script_file_name(file).ok()?;
        (dir.join("scripts"), if fab_model::program::is_strokes_file(file) { STROKES_EXTENSION } else { SCRIPT_EXTENSION })
    } else if let Some(file) = name.strip_prefix("elec/") {
        let ext = if file.ends_with(".splash") {
            project::doc_file_name(file).ok()?;
            DOC_EXTENSION
        } else if project::memory_file_name(file).is_ok() {
            MEMORY_EXTENSION
        } else {
            project::elec_asset_file_name(file).ok()?;
            ASSET_EXTENSION
        };
        (dir.join("scripts"), ext)
    } else if let Some(file) = name.strip_prefix("current.meshes/") {
        project::script_file_name(file).ok()?;
        let ext = if file.ends_with(&format!(".{}", project::MESH_EXTENSION)) {
            project::MESH_EXTENSION
        } else if file.ends_with(&format!(".{}", project::FACETS_EXTENSION)) {
            project::FACETS_EXTENSION
        } else if file.ends_with(&format!(".{PICTURE_EXTENSION}")) {
            PICTURE_EXTENSION
        } else { return None };
        (project::meshes_dir(&dir.join(AUTOSAVE_FILE)), ext)
    } else { return None };
    Some(folder.join(format!("{hash}.{ext}")))
}

/// Where a plate's folder keeps what no autosave may prune or write over.
const KEPT_ASIDE: &str = "kept-aside";

/// The folders of the working copy in `dir`, the one with its map last: the working copy is
/// there while its map is.
fn folders(dir: &Path) -> [PathBuf; 2] {
    [project::meshes_dir(&dir.join(AUTOSAVE_FILE)), dir.join("scripts")]
}

/// A new, empty `kept-aside/<time>/` in `dir` with its `why` file.
fn new_kept_folder(dir: &Path, why: &str) -> Result<PathBuf, String> {
    let kept = dir.join(KEPT_ASIDE);
    let secs = makepad_civil_time::now_secs();
    let mut to = kept.join(secs.to_string());
    let mut n = 1;
    while to.try_exists().map_err(|e| format!("{}: {e}", to.display()))? {
        n += 1;
        to = kept.join(format!("{secs}-{n}"));
    }
    project::write_atomic(&to.join("why"), why.as_bytes())?;
    Ok(to)
}

/// Move `paths` (files or folders in `dir`, each kept at its place below it) into a new
/// `kept-aside/<time>/` in `dir`, with `why`, and return that folder. A path not there is left
/// out. Nothing is copied or removed: a move in place.
fn keep_aside(dir: &Path, paths: &[PathBuf], why: &str) -> Result<PathBuf, String> {
    let to = new_kept_folder(dir, why)?;
    for path in paths {
        let at = to.join(path.strip_prefix(dir).map_err(|_| format!("{} is not in {}", path.display(), dir.display()))?);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        match std::fs::rename(path, &at) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        sync_dir(path.parent().unwrap_or(dir))?;
    }
    sync_dir(&to)?;
    Ok(to)
}

/// Write electronics `texts` (file name -> text) into a new `kept-aside/<time>/` in `dir`, with `why`:
/// documents that reached no plate. Returns where they are.
pub(crate) fn keep_texts_aside(dir: &Path, texts: &BTreeMap<String, Arc<str>>, why: &str) -> Result<PathBuf, String> {
    for name in texts.keys() {
        project::doc_file_name(name)?;
    }
    let to = new_kept_folder(dir, why)?;
    for (name, text) in texts {
        project::write_atomic(&to.join(name), text.as_bytes())?;
    }
    sync_dir(&to)?;
    Ok(to)
}

/// Keep the working copy in `dir` aside whole, before anything is saved over it: one that does
/// not read back ([`Snapshot::recover`]'s error), `why`. Returns where it is.
pub(crate) fn keep_working_copy_aside(dir: &Path, why: &str) -> Result<PathBuf, String> {
    keep_aside(dir, &folders(dir), &format!("its working copy did not read back: {why}"))
}

/// What `dir` keeps aside: each folder and why, the oldest first.
pub(crate) fn kept_aside(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(list) = std::fs::read_dir(dir.join(KEPT_ASIDE)) else { return Vec::new() };
    let mut kept: Vec<(PathBuf, String)> = list.flatten().map(|e| e.path()).filter(|p| p.is_dir())
        .map(|p| {
            let why = std::fs::read_to_string(p.join("why")).unwrap_or_default();
            (p, why)
        })
        .collect();
    kept.sort();
    kept
}

/// The content files in the working copy's folders in `dir` (by their extensions).
fn content_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for folder in folders(dir) {
        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", folder.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", folder.display()))?;
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let content = [SCRIPT_EXTENSION, STROKES_EXTENSION, project::EXTENSION, DOC_EXTENSION, ASSET_EXTENSION, MEMORY_EXTENSION,
                "meta", project::MESH_EXTENSION, project::FACETS_EXTENSION, PICTURE_EXTENSION]
                .iter().any(|ext| name.ends_with(&format!(".{ext}")));
            if content && entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// The content file `path` (named `<hash>.<ext>`) holds what Fab wrote in it.
fn holds_its_hash(path: &Path) -> Result<bool, String> {
    let Some(named) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.split_once('.')).map(|(hash, _)| hash) else { return Ok(false) };
    match std::fs::read(path) {
        Ok(bytes) => Ok(hash(&bytes) == named),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        match std::fs::File::open(path) {
            Ok(file) => file.sync_all().map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

impl Snapshot {
    pub fn is_current(&self, model: &FabModel, path: Option<&Path>) -> bool {
        self.head == Some(model.head()) && self.name == model.name && self.path.as_deref() == path
            && self.assets.as_ref().is_some_and(|a| Arc::ptr_eq(a, &model.assets))
            && self.facets.as_ref().is_some_and(|f| Arc::ptr_eq(f, &model.facets))
            && self.pictures.as_ref().is_some_and(|p| Arc::ptr_eq(p, &model.pictures))
            && self.docs.as_ref().is_some_and(|d| Arc::ptr_eq(d, model.docs()))
            && self.elec_assets.as_ref().is_some_and(|a| Arc::ptr_eq(a, model.elec_assets()))
            && self.elec_memory.as_ref().is_some_and(|m| Arc::ptr_eq(m, model.elec_memory()))
    }

    pub fn remember(&mut self, model: &FabModel, path: Option<&Path>) {
        self.head = Some(model.head());
        self.name = model.name.clone();
        self.path = path.map(Path::to_path_buf);
        self.assets = Some(model.assets.clone());
        self.facets = Some(model.facets.clone());
        self.pictures = Some(model.pictures.clone());
        self.docs = Some(model.docs().clone());
        self.elec_assets = Some(model.elec_assets().clone());
        self.elec_memory = Some(model.elec_memory().clone());
    }

    fn store(&self, dir: &Path, next: &mut BTreeMap<String, SavedFile>, name: String, content: Content) -> Result<(), String> {
        if let Some(saved) = self.files.get(&name).filter(|saved| saved.content.same(&content)) {
            next.insert(name, saved.clone());
            return Ok(());
        }
        let bytes = content.bytes();
        let hash = hash(&bytes);
        let path = content_path(dir, &name, &hash).ok_or_else(|| format!("invalid autosave file {name:?}"))?;
        // A file of this name holds these bytes, unless it was changed outside Fab: then it is
        // the person's, kept aside as it is, and these written in its place.
        let there = match std::fs::read(&path) {
            Ok(old) => old == *bytes || {
                let file = path.strip_prefix(dir).unwrap_or(&path).display().to_string();
                keep_aside(dir, std::slice::from_ref(&path), &format!("{file} was changed outside Fab, and the autosave needed its name for {name} as it is now"))?;
                false
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if !there {
            project::write_atomic(&path, &bytes)?;
        }
        drop(bytes);
        next.insert(name, SavedFile { hash, content: Arc::new(content) });
        Ok(())
    }

    fn store_text(&self, dir: &Path, next: &mut BTreeMap<String, SavedFile>, name: String, text: &str) -> Result<(), String> {
        if let Some(saved) = self.files.get(&name).filter(|saved| matches!(saved.content.as_ref(), Content::Text(old) if old == text)) {
            next.insert(name, saved.clone());
            return Ok(());
        }
        self.store(dir, next, name, Content::Text(text.to_string()))
    }

    /// Only exact content-file paths outside the committed set are removed, including orphans,
    /// and only Fab's own: what holds the bytes its name hashes. One changed outside Fab stays.
    fn prune(dir: &Path, files: &BTreeMap<String, SavedFile>) -> Result<(), String> {
        let keep: BTreeSet<PathBuf> = files.iter().filter_map(|(name, file)| content_path(dir, name, &file.hash)).collect();
        for path in content_files(dir)? {
            if !keep.contains(&path) && holds_its_hash(&path)? {
                remove_file(&path)?;
            }
        }
        Ok(())
    }

    pub fn save(&mut self, dir: &Path, model: &FabModel, path: Option<&Path>) -> Result<(), String> {
        if self.is_current(model, path) {
            return Ok(());
        }
        let map_path = dir.join("scripts/map");
        if model.is_empty() {
            remove_file(&map_path)?;
            sync_dir(&dir.join("scripts"))?;
            self.files.clear();
            self.head = None;
            Self::prune(dir, &self.files)?;
            self.remember(model, path);
            return Ok(());
        }
        let mut next = BTreeMap::new();
        for (name, text) in &model.state().scripts {
            self.store_text(dir, &mut next, format!("scripts/{name}"), text)?;
        }
        for (name, log) in model.state().strokes.iter() {
            self.store(dir, &mut next, format!("scripts/{name}"), Content::Strokes(log.clone()))?;
        }
        for (name, mesh) in model.assets.iter() {
            self.store(dir, &mut next, format!("current.meshes/{name}.{}", project::MESH_EXTENSION), Content::Mesh(mesh.clone()))?;
        }
        for (name, facets) in model.facets.iter() {
            self.store(dir, &mut next, format!("current.meshes/{name}.{}", project::FACETS_EXTENSION), Content::Facets(facets.clone()))?;
        }
        for (name, picture) in model.pictures.iter() {
            self.store(dir, &mut next, format!("current.meshes/{name}.{PICTURE_EXTENSION}"), Content::Picture(picture.clone()))?;
        }
        for (file, text) in model.docs().iter() {
            project::doc_file_name(file)?;
            self.store(dir, &mut next, format!("elec/{file}"), Content::Doc(text.clone()))?;
        }
        for (file, bytes) in model.elec_assets().iter() {
            project::elec_asset_file_name(file)?;
            self.store(dir, &mut next, format!("elec/{file}"), Content::Asset(bytes.clone()))?;
        }
        for (file, bytes) in model.elec_memory().iter() {
            project::memory_file_name(file)?;
            self.store(dir, &mut next, format!("elec/{file}"), Content::Memory(bytes.clone()))?;
        }
        self.store(dir, &mut next, AUTOSAVE_FILE.into(), Content::Text(model.project_text()))?;
        let meta = format!("name\t{}\npath\t{}\n", encode(&model.name), encode(&path.map(|p| p.display().to_string()).unwrap_or_default()));
        self.store(dir, &mut next, "meta".into(), Content::Text(meta))?;
        sync_dir(&dir.join("scripts"))?;
        sync_dir(&project::meshes_dir(&dir.join(AUTOSAVE_FILE)))?;
        let map: String = next.iter().map(|(name, file)| format!("{}\t{}\n", file.hash, encode(name))).collect();
        // The map is the commit point: recovery reads the project, meta and assets through it.
        project::write_atomic(&map_path, map.as_bytes())?;
        sync_dir(&dir.join("scripts"))?;
        self.files = next;
        self.head = None;
        Self::prune(dir, &self.files)?;
        self.remember(model, path);
        Ok(())
    }

    /// The working copy in `dir` as its map names it. None: there is none (no map and no files:
    /// never committed, or emptied). A file not as Fab wrote it that reads is the person's
    /// newest work: taken as it reads and said in [`Recovered::changed`]; it is no file of the
    /// snapshot, so the next autosave writes it under its own hash and leaves that file as it is.
    /// An error: what does not read back. Its caller keeps it aside
    /// ([`keep_working_copy_aside`]) before anything is saved over it.
    pub fn recover(dir: &Path) -> Result<Option<Recovered>, String> {
        let map = match std::fs::read_to_string(dir.join("scripts/map")) {
            Ok(map) => map,
            // files with no map: a commit a crash cut short, or a map gone; either way not pruned
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let files = content_files(dir)?;
                return match files.len() {
                    0 => Ok(None),
                    n => Err(format!("scripts/map is not there, and {n} of its files are")),
                };
            }
            Err(e) => return Err(format!("scripts/map: {e}")),
        };
        let mut saved = Self::default();
        let mut read = Read::default();
        let mut originals = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut changed = Vec::new();
        for line in map.lines() {
            let named = line.split_once('\t').and_then(|(digest, name)| Some((digest, decode(name)?)));
            let Some((digest, name)) = named else { return Err(format!("scripts/map: {line:?} is no file of it")) };
            let path = content_path(dir, &name, digest).ok_or_else(|| format!("scripts/map: {line:?} is no file of it"))?;
            let file = path.strip_prefix(dir).unwrap_or(&path).display().to_string();
            if !names.insert(name.clone()) {
                return Err(format!("scripts/map names {name} twice"));
            }
            let bytes = std::fs::read(&path).map_err(|e| format!("{name} ({file}): {e}"))?;
            if name != "meta" { originals.insert(name.clone(), Arc::<[u8]>::from(bytes.as_slice())); }
            let own = hash(&bytes) == digest;
            let content = read.take(&name, bytes).map_err(|e| if own {
                format!("{name} ({file}): {e}")
            } else {
                format!("{name} ({file}) was changed outside Fab and does not read: {e}")
            })?;
            if own {
                saved.files.insert(name, SavedFile { hash: digest.to_string(), content: Arc::new(content) });
            } else {
                changed.push(format!("{name} was changed outside Fab ({file}): it is taken as it reads"));
            }
        }
        let body = read.project.ok_or("scripts/map names no project")?;
        let meta = read.meta.ok_or("scripts/map names no meta")?;
        let not_meta = || format!("meta: {meta:?} is not a name and a path");
        let mut lines = meta.lines();
        let name = lines.next().and_then(|l| decode(l.strip_prefix("name\t")?)).ok_or_else(not_meta)?;
        let path = lines.next().and_then(|l| decode(l.strip_prefix("path\t")?)).ok_or_else(not_meta)?;
        if lines.next().is_some() {
            return Err(not_meta());
        }
        // A working copy names every placement's object (the writer keeps them), so its load
        // compiles no script; the session gives the model its env when it comes on the bed.
        let mut model = FabModel::load_with_assets(if name.is_empty() { "Untitled" } else { &name }, &body, read.scripts, read.strokes, Arc::new(read.assets), Arc::new(read.facets), Arc::new(read.pictures), Default::default())
            .map_err(|e| format!("the project does not load: {e}"))?;
        if !read.docs.is_empty() {
            model.set_docs(Arc::new(read.docs));
        }
        if !read.elec_assets.is_empty() {
            model.set_elec_assets(Arc::new(read.elec_assets));
        }
        if !read.elec_memory.is_empty() {
            model.set_elec_memory(Arc::new(read.elec_memory));
        }
        let manifest = Path::new(&path).file_name().and_then(|file| file.to_str()).unwrap_or(AUTOSAVE_FILE);
        let stem = manifest.strip_suffix(".fab.splash").unwrap_or(manifest);
        let files = originals.into_iter().map(|(key, bytes)| {
            let file = if key == AUTOSAVE_FILE { manifest.into() }
                else if let Some(file) = key.strip_prefix("scripts/") { file.to_owned() }
                else if let Some(file) = key.strip_prefix("elec/") { format!("{stem}.elec/{file}") }
                else {
                    let file = key.strip_prefix("current.meshes/").expect("accepted working-copy key");
                    match file.strip_suffix(&format!(".{PICTURE_EXTENSION}")) {
                        Some(picture) => picture.to_owned(),
                        None => format!("{stem}.meshes/{file}"),
                    }
                };
            (file, bytes)
        }).collect();
        model.opened_files = Some(fab_model::LoadedProject { manifest: manifest.into(), files });
        Ok(Some(Recovered { model, path: (!path.is_empty()).then(|| PathBuf::from(path)), snapshot: saved, changed }))
    }
}

/// A working copy read back ([`Snapshot::recover`]).
pub(crate) struct Recovered {
    pub model: FabModel,
    /// The file its document is of.
    pub path: Option<PathBuf>,
    /// What the next autosave goes on from.
    pub snapshot: Snapshot,
    /// Each file that was not as Fab wrote it but reads, taken as it reads: one line naming it.
    pub changed: Vec<String>,
}

/// A working copy's files as they read, by kind, as [`FabModel::load_with_assets`] takes them.
#[derive(Default)]
struct Read {
    project: Option<String>,
    meta: Option<String>,
    scripts: BTreeMap<String, String>,
    strokes: BTreeMap<String, Arc<Strokes>>,
    assets: Assets,
    facets: FacetAssets,
    pictures: Pictures,
    docs: Docs,
    elec_assets: ElecAssets,
    elec_memory: ElecMemory,
}

impl Read {
    /// The map's file `name` as what its name says it is.
    fn take(&mut self, name: &str, bytes: Vec<u8>) -> Result<Content, String> {
        let text = |bytes| String::from_utf8(bytes).map_err(|_| "it is not text".to_string());
        if name == AUTOSAVE_FILE || name == "meta" {
            let text = text(bytes)?;
            *(if name == "meta" { &mut self.meta } else { &mut self.project }) = Some(text.clone());
            return Ok(Content::Text(text));
        }
        if let Some(file) = name.strip_prefix("scripts/") {
            if fab_model::program::is_strokes_file(file) {
                let log = Arc::new(Strokes::from_bytes(&bytes)?);
                self.strokes.insert(file.to_string(), log.clone());
                return Ok(Content::Strokes(log));
            }
            let text = text(bytes)?;
            self.scripts.insert(file.to_string(), text.clone());
            return Ok(Content::Text(text));
        }
        if let Some(file) = name.strip_prefix("elec/") {
            if file.ends_with(".splash") {
                let text: Arc<str> = text(bytes)?.into();
                self.docs.insert(file.to_string(), text.clone());
                return Ok(Content::Doc(text));
            }
            let bytes: Arc<[u8]> = bytes.into();
            if project::memory_file_name(file).is_ok() {
                self.elec_memory.insert(file.to_string(), bytes.clone());
                return Ok(Content::Memory(bytes));
            }
            self.elec_assets.insert(file.to_string(), bytes.clone());
            return Ok(Content::Asset(bytes));
        }
        let file = name.strip_prefix("current.meshes/").ok_or("a working copy has no such file")?;
        if let Some(key) = file.strip_suffix(&format!(".{}", project::MESH_EXTENSION)) {
            let mesh = Arc::new(MeshField::new(project::mesh_of_bytes(&bytes)?));
            self.assets.insert(key.to_string(), mesh.clone());
            Ok(Content::Mesh(mesh))
        } else if let Some(key) = file.strip_suffix(&format!(".{PICTURE_EXTENSION}")) {
            let picture = Arc::new(Picture::new(bytes)?);
            self.pictures.insert(key.to_string(), picture.clone());
            Ok(Content::Picture(picture))
        } else {
            let key = file.strip_suffix(&format!(".{}", project::FACETS_EXTENSION)).ok_or("a working copy has no such file")?;
            let data = Arc::new(project::facets_of_bytes(&bytes)?);
            self.facets.insert(key.to_string(), data.clone());
            Ok(Content::Facets(data))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recovery needs only the working copy; unchanged binary contents stay immutable, and
    /// externally edited bytes survive both pruning and reuse of their original hash's name.
    #[test]
    fn electronics_assets_recover_without_rewrites_and_keep_outside_edits() {
        let dir = std::env::temp_dir().join(format!("fab-autosave-elec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut model = FabModel::new("plate");
        let docs = Arc::new(Docs::from([
            ("circuit.ckt.splash".into(), Arc::<str>::from("circuit\n")),
            ("parts/lcsc/C1.part.splash".into(), Arc::<str>::from("part\n")),
        ]));
        let name = "parts/lcsc/C1.glb";
        let bytes = Arc::<[u8]>::from([0x00, 0xff, 0x80, 0x01]);
        let assets = Arc::new(ElecAssets::from([(name.into(), bytes.clone())]));
        let memory_name = "board.mcu.eeprom";
        model.set_docs(docs.clone());
        model.set_elec_assets(assets.clone());
        model.set_elec_memory(Arc::new(ElecMemory::from([(memory_name.into(), bytes.clone())])));
        let content = model.content();
        let mut snapshot = Snapshot::default();
        snapshot.save(&dir, &model, None).expect("autosave electronics without a project file");
        let asset_path = content_path(&dir, &format!("elec/{name}"), &hash(&bytes)).expect("a valid binary content path");
        assert_eq!(std::fs::read(&asset_path).expect("read the binary content"), bytes.as_ref());
        let memory_path = content_path(&dir, &format!("elec/{memory_name}"), &hash(&bytes)).expect("a valid memory content path");
        assert_ne!(memory_path, asset_path, "memory and asset files stay separate when their bytes match");
        assert_eq!(std::fs::read(&memory_path).expect("read the memory content"), bytes.as_ref(), "memory bytes are saved without text conversion");
        let memory = Arc::new(ElecMemory::from([(memory_name.into(), Arc::<[u8]>::from([0x00, 0xfe, 0x81, 0x01]))]));
        model.set_elec_memory(memory.clone());
        assert!(!snapshot.is_current(&model, None), "a new device memory Arc alone needs autosave");
        snapshot.save(&dir, &model, None).expect("autosave the changed device memory");
        assert!(snapshot.is_current(&model, None), "saving remembers the changed device memory Arc");
        assert!(!memory_path.exists(), "stale device memory content is pruned after its replacement is saved");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        for path in content_files(&dir).expect("list the content").into_iter().chain([dir.join("scripts/map")]) {
            std::fs::File::options().write(true).open(path).expect("open working-copy content")
                .set_modified(old).expect("mark the saved content without sleeping");
        }
        let times = || -> BTreeMap<PathBuf, std::time::SystemTime> {
            content_files(&dir).expect("list the content").into_iter().map(|path| {
                let time = std::fs::metadata(&path).expect("content exists").modified().expect("read modification time");
                (path, time)
            }).collect()
        };
        let before = times();
        let map_time = std::fs::metadata(dir.join("scripts/map")).expect("map exists")
            .modified().expect("read map modification time");
        assert!(snapshot.is_current(&model, None), "remembered asset, document and memory maps are current");
        snapshot.save(&dir, &model, None).expect("save unchanged electronics");
        assert_eq!(times(), before, "a second save creates or rewrites no content file");
        assert_eq!(std::fs::metadata(dir.join("scripts/map")).expect("map stays")
            .modified().expect("read map modification time"), map_time, "an unchanged map is not rewritten");
        drop(model);
        drop(snapshot);

        let mut read = Snapshot::recover(&dir).expect("read the working copy").expect("electronics form a plate");
        assert_eq!(read.model.docs(), &docs, "recover both top-level and nested documents");
        assert_eq!(read.model.elec_assets(), &assets, "recover arbitrary bytes");
        assert_eq!(read.model.elec_memory(), &memory, "recover device memory byte for byte");
        assert_eq!(read.model.content(), content, "recovery keeps the plate content");
        read.snapshot.save(&dir, &read.model, None).expect("autosave the recovered plate");
        assert_eq!(times(), before, "recovered immutable content needs no new files or rewrites");

        let outside = [0x00, 0xfe, 0x81];
        std::fs::write(&asset_path, outside).expect("edit the binary outside Fab");
        let mut read = Snapshot::recover(&dir).expect("read edited binary content").expect("the plate reads");
        assert_eq!(read.model.elec_assets()[name].as_ref(), outside, "outside bytes are the person's newest work");
        assert_eq!(read.changed.len(), 1, "report the outside edit once");
        assert!(read.changed[0].contains(&format!("elec/{name} was changed outside Fab")));
        read.snapshot.save(&dir, &read.model, None).expect("save outside bytes under their own hash");
        assert_eq!(std::fs::read(&asset_path).expect("the externally changed file survives pruning"), outside);
        read.model.set_elec_assets(assets);
        assert!(!read.snapshot.is_current(&read.model, None), "an asset edit alone needs autosave");
        read.snapshot.save(&dir, &read.model, None).expect("reuse the original hash without losing the person's file");
        assert_eq!(std::fs::read(&asset_path).expect("original content is saved"), bytes.as_ref());
        let kept = kept_aside(&dir);
        assert_eq!(kept.len(), 1, "the externally changed file is kept aside once");
        let relative = asset_path.strip_prefix(&dir).expect("content is in this working copy");
        assert_eq!(std::fs::read(kept[0].0.join(relative)).expect("read the preserved outside edit"), outside);
        let outside_path = content_path(&dir, &format!("elec/{name}"), &hash(&outside)).expect("outside content path");
        assert!(!outside_path.exists(), "only Fab's own stale binary content is pruned");

        let doc_path = content_path(&dir, "elec/parts/lcsc/C1.part.splash", &hash(b"part\n"))
            .expect("nested document content path");
        std::fs::write(&doc_path, [0xff, 0x00]).expect("make a nested document unreadable as text");
        let why = match Snapshot::recover(&dir) {
            Err(why) => why,
            Ok(_) => panic!("non-text documents must not recover"),
        };
        assert!(why.contains("elec/parts/lcsc/C1.part.splash"), "name the unreadable document: {why}");
        let whole: BTreeMap<_, _> = content_files(&dir).expect("list the whole working copy").into_iter()
            .chain([dir.join("scripts/map")]).map(|path| {
                let bytes = std::fs::read(&path).expect("read content to preserve");
                (path.strip_prefix(&dir).expect("content is in this working copy").to_path_buf(), bytes)
            }).collect();
        let kept = keep_working_copy_aside(&dir, &why).expect("keep an unreadable working copy aside whole");
        for (path, bytes) in whole {
            assert_eq!(std::fs::read(kept.join(path)).expect("every file is kept aside"), bytes);
        }
        let kept_docs = keep_texts_aside(&dir, &docs, "documents reached no plate").expect("keep nested docs aside");
        assert_eq!(std::fs::read_to_string(kept_docs.join("parts/lcsc/C1.part.splash")).expect("nested doc stays"), "part\n");
        std::fs::remove_dir_all(&dir).expect("remove the test working copy");
    }

    /// A binary content file left by a cut-short save is evidence of a working copy, even with
    /// no map or text files; it must be kept aside rather than treated as an empty plate.
    #[test]
    fn an_orphan_electronics_asset_is_not_an_empty_working_copy() {
        let dir = std::env::temp_dir().join(format!("fab-autosave-elec-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bytes = [0x00, 0xff];
        let path = content_path(&dir, "elec/parts/lcsc/C1.glb", &hash(&bytes)).expect("binary content path");
        project::write_atomic(&path, &bytes).expect("write the orphaned binary content");
        let why = match Snapshot::recover(&dir) {
            Err(why) => why,
            Ok(_) => panic!("an orphaned asset is a working copy that needs keeping aside"),
        };
        assert!(why.contains("scripts/map is not there, and 1 of its files are"), "{why}");
        let kept = keep_working_copy_aside(&dir, &why).expect("keep the orphan aside");
        let relative = path.strip_prefix(&dir).expect("the asset is in the working copy");
        assert_eq!(std::fs::read(kept.join(relative)).expect("orphan survives"), bytes);
        std::fs::remove_dir_all(&dir).expect("remove the test working copy");
    }
}
