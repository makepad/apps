//! The project's documents as text, the installed libraries, and the loader of a project folder.
//!
//! A document's kind comes from its file name (`main.ckt.splash` is a circuit). Positions are
//! byte offsets into the document's text; `Sources::line_col` turns one into the 1-based line and
//! character column an editor shows.
//!
//! Ids: a project document is `DocId(i)`, its index in `Sources`; a library document is
//! `DocId(LIB_DOC | i)`, its index in `Libs`, so one `SourceRange` names either without
//! ambiguity. Cloning `Sources` or `Libs` shares every document's text (`Arc<str>`): snapshots for
//! a worker are cheap, and an edit replaces only the edited document's text.

use fab_elec_core::{Diag, DocId, Sel};
use fab_elec_design::TextEdit;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::map::SourceRange;

/// The bit that marks a library document's id.
pub const LIB_DOC: u32 = 1 << 31;

/// The fixed name of an implicit empty system document when a project has none. Unit and
/// folder renames never move it; a non-empty system document keeps its name.
pub const SYSTEM_DOC: &str = "system.sys.splash";

/// What a document holds, by its file name's suffix (ARCHITECTURE 2.1).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum DocKind {
    /// `*.ckt.splash`: a unit's circuit as code.
    Circuit,
    /// `*.pcb.splash`: a board's layout statements.
    Board,
    /// `*.sch.splash`: drawn sheets.
    Sheets,
    /// `*.sys.splash`: literal links between units and unit mounts.
    System,
    /// `*.cable.splash`: a cable or harness.
    Cable,
    /// `*.part.splash`: part definitions and generators.
    Part,
    /// `*.blocks.splash`: block definitions.
    Blocks,
    /// `*.bench.splash`: stimuli, instruments, assertions.
    Bench,
    /// `*.export.splash`: a factory profile.
    Profile,
    /// `*.view.splash`: a custom view.
    View,
    /// `*.fw.splash`: an MCU's firmware. The firmware language compiles it; circuit evaluation never
    /// reads it, so its edits leave the design as it is.
    Firmware,
}

impl DocKind {
    const SUFFIXES: [(&'static str, DocKind); 11] = [
        (".ckt.splash", DocKind::Circuit),
        (".pcb.splash", DocKind::Board),
        (".sch.splash", DocKind::Sheets),
        (".sys.splash", DocKind::System),
        (".cable.splash", DocKind::Cable),
        (".part.splash", DocKind::Part),
        (".blocks.splash", DocKind::Blocks),
        (".bench.splash", DocKind::Bench),
        (".export.splash", DocKind::Profile),
        (".view.splash", DocKind::View),
        (".fw.splash", DocKind::Firmware),
    ];

    /// The kind a file name says, if it is an electronics document.
    pub fn of_name(name: &str) -> Option<DocKind> {
        Self::SUFFIXES.iter().find(|(s, _)| name.ends_with(s)).map(|(_, k)| *k)
    }

    /// Whether typed text of this kind brings the design up to date by an evaluation. Firmware is
    /// never evaluated; typed system text stays text-only until the system lowering reads it (G1).
    pub fn edit_reevaluates(self) -> bool {
        !matches!(self, DocKind::System | DocKind::Firmware)
    }

    /// The file name suffix of the kind (`.ckt.splash`).
    pub fn suffix(self) -> &'static str {
        Self::SUFFIXES.iter().find(|(_, k)| *k == self).map(|(s, _)| *s).unwrap_or(".splash")
    }
}

/// One document.
#[derive(Clone, PartialEq, Debug)]
pub struct Doc {
    pub id: DocId,
    /// Whether the document is present; absent project documents keep their ids.
    pub present: bool,
    /// The file name, relative to the project (or library) folder: `ledctl.ckt.splash`.
    pub name: String,
    pub kind: DocKind,
    pub text: Arc<str>,
    /// The library it belongs to (`std`); `None` for a project document.
    pub lib: Option<String>,
}

impl Doc {
    /// An empty system document is not a file. Savers skip it; a saver that deletes stale files
    /// removes its file when a person empties it, whatever its name.
    pub fn is_file(&self) -> bool {
        self.kind != DocKind::System || !self.text.is_empty()
    }
}

/// The documents of one project.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Sources {
    docs: Vec<Doc>,
    assets: Vec<String>,
    /// Models the person's part library serves under project paths ([`Sources::set_library`]),
    /// shared by clones as the texts are.
    library: Arc<[String]>,
    /// `assets` and `library` merged, sorted, each once, while the library lists any.
    mesh: Arc<[String]>,
}

/// The installed libraries' documents (parts and blocks).
#[derive(Clone, Debug, Default)]
pub struct Libs {
    docs: Vec<Doc>,
    pub(crate) defs: Arc<OnceLock<Arc<crate::catalog::LibDefs>>>,
}

impl PartialEq for Libs {
    fn eq(&self, other: &Self) -> bool {
        self.docs == other.docs
    }
}

fn new_doc(id: DocId, name: String, text: Arc<str>, lib: Option<String>, taken: &[Doc]) -> Result<Doc, Diag> {
    let Some(kind) = DocKind::of_name(&name) else {
        return Err(Diag::error(format!("{name}: not an electronics document (a name ends in .ckt.splash, .pcb.splash, .part.splash, ...)"), vec![]));
    };
    if taken.iter().any(|d| d.present && d.name == name && d.lib == lib) {
        return Err(Diag::error(format!("{name}: two documents with this name"), vec![]));
    }
    Ok(Doc { id, present: true, name, kind, text, lib })
}

impl Sources {
    pub fn new() -> Sources {
        Sources::default()
    }

    /// Add a document; its kind comes from its name. Fails on a name that is no electronics
    /// document or that is taken by a present document. Never reuses an id.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<Arc<str>>) -> Result<DocId, Diag> {
        let id = DocId(self.docs.len() as u32);
        let doc = new_doc(id, name.into(), text.into(), None, &self.docs)?;
        self.docs.push(doc);
        Ok(id)
    }

    /// Change presence without changing text; undo restores text with `apply` first.
    pub fn set_present(&mut self, id: DocId, present: bool) -> Result<(), Diag> {
        let Some(doc) = self.docs.get(id.ix()).filter(|_| id.0 & LIB_DOC == 0) else {
            return Err(Diag::error(format!("document {} is not in the project", id.0), vec![]));
        };
        if present && self.docs.iter().any(|d| d.present && d.id != id && d.name == doc.name) {
            return Err(located(id, 0..0, format!("a document named {} is in the project already", doc.name)));
        }
        self.docs[id.ix()].present = present;
        Ok(())
    }

    /// Return the first system document with text, keeping its name. Empty system documents
    /// count as missing files: reuse an empty `SYSTEM_DOC`, otherwise rename the first empty
    /// system document to `SYSTEM_DOC` in place, keeping its id, or add it if none exists.
    /// Absent documents are never reused or renamed.
    pub fn ensure_system(&mut self) -> Result<DocId, Diag> {
        if let Some(doc) = self.docs.iter().find(|d| d.present && d.kind == DocKind::System && !d.text.is_empty()) {
            return Ok(doc.id);
        }
        if let Some(doc) = self.docs.iter().find(|d| d.present && d.kind == DocKind::System && d.name == SYSTEM_DOC) {
            return Ok(doc.id);
        }
        // SYSTEM_DOC cannot be taken by another kind: its .sys.splash suffix determines the kind.
        if let Some(doc) = self.docs.iter_mut().find(|d| d.present && d.kind == DocKind::System) {
            doc.name = SYSTEM_DOC.to_string();
            return Ok(doc.id);
        }
        self.add(SYSTEM_DOC, "")
    }

    pub fn doc(&self, id: DocId) -> Option<&Doc> {
        if id.0 & LIB_DOC != 0 {
            return None;
        }
        self.docs.get(id.ix()).filter(|d| d.present)
    }

    /// The present document with this file name.
    pub fn find(&self, name: &str) -> Option<DocId> {
        self.docs.iter().find(|d| d.present && d.name == name).map(|d| d.id)
    }

    /// Present documents, in id order.
    pub fn docs(&self) -> Vec<&Doc> {
        self.docs.iter().filter(|d| d.present).collect()
    }

    /// Every document, including absent documents retained for undo, in id order.
    pub fn all_docs(&self) -> &[Doc] {
        &self.docs
    }

    /// Record an asset without reading it. Paths are project-relative, with `/` separators.
    pub fn add_asset(&mut self, path: impl Into<String>) {
        let path = path.into();
        if let Err(i) = self.assets.binary_search(&path) {
            self.assets.insert(i, path);
            self.merge_library();
        }
    }

    /// Take an asset off the list (undo of an added asset); an unlisted path changes nothing.
    pub fn remove_asset(&mut self, path: &str) {
        if let Ok(i) = self.assets.binary_search_by(|listed| listed.as_str().cmp(path)) {
            self.assets.remove(i);
            self.merge_library();
        }
    }

    pub fn assets(&self) -> &[String] {
        &self.assets
    }

    /// The models the person's part library holds, under the project paths a part's mesh names
    /// (`parts/lcsc/C2040.glb`): a mesh may name one the project has no file for, and it is
    /// present. They are no project files ([`Self::assets`] stays the project's own). Sorted, each
    /// once. Cost: O(n log n) for `names`; while the library lists any, an asset added or taken
    /// off merges the two lists again, O(assets + library).
    pub fn set_library(&mut self, mut names: Vec<String>) {
        names.sort_unstable();
        names.dedup();
        self.library = names.into();
        self.merge_library();
    }

    fn merge_library(&mut self) {
        if self.library.is_empty() {
            self.mesh = Arc::default();
            return;
        }
        let mut mesh: Vec<String> = self.assets.iter().chain(self.library.iter()).cloned().collect();
        mesh.sort_unstable();
        mesh.dedup();
        self.mesh = mesh.into();
    }

    pub fn library(&self) -> &[String] {
        &self.library
    }

    /// Every name a part's mesh finds: the project's assets and the part library's models, in
    /// name order, each once. No work: the merge is kept as either list changes.
    pub fn mesh_assets(&self) -> &[String] {
        if self.library.is_empty() { &self.assets[..] } else { &self.mesh[..] }
    }

    /// Apply text edits, all or nothing. Each edit's span is in the text as it is before this
    /// call; edits of one document must not overlap. Absent documents may be edited too.
    /// A failed call changes nothing.
    pub fn apply(&mut self, edits: &[TextEdit]) -> Result<(), Diag> {
        let mut by_doc: Vec<(DocId, Vec<&TextEdit>)> = Vec::new();
        for e in edits {
            match by_doc.iter_mut().find(|(d, _)| *d == e.doc) {
                Some((_, list)) => list.push(e),
                None => by_doc.push((e.doc, vec![e])),
            }
        }
        let mut out: Vec<(usize, Arc<str>)> = Vec::new();
        for (doc, mut list) in by_doc {
            let Some(d) = self.docs.get(doc.ix()).filter(|_| doc.0 & LIB_DOC == 0) else {
                return Err(Diag::error(format!("an edit names document {} that is not in the project", doc.0), vec![]));
            };
            list.sort_by_key(|e| (e.span.start, e.span.end));
            let text: &str = &d.text;
            let mut s = String::with_capacity(text.len());
            let mut at = 0usize;
            for e in list {
                let (a, b) = (e.span.start as usize, e.span.end as usize);
                if a < at || b < a || b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                    return Err(Diag::error(format!("{}: an edit at {}..{} overlaps another or is outside the text", d.name, a, b), vec![]));
                }
                s.push_str(&text[at..a]);
                s.push_str(&e.text);
                at = b;
            }
            s.push_str(&text[at..]);
            out.push((doc.ix(), s.into()));
        }
        for (i, text) in out {
            self.docs[i].text = text;
        }
        Ok(())
    }

    /// 1-based line and 1-based character column of a byte offset of a document.
    pub fn line_col(&self, doc: DocId, offset: u32) -> Option<(u32, u32)> {
        line_col(&self.doc(doc)?.text, offset)
    }

    /// `file:line:col` of a range of a project or library document.
    pub fn describe(&self, libs: &Libs, r: &SourceRange) -> String {
        let doc = self.doc(r.doc).or_else(|| libs.doc(r.doc));
        match doc {
            Some(d) => match line_col(&d.text, r.span.start) {
                Some((l, c)) => format!("{}:{l}:{c}", d.name),
                None => d.name.clone(),
            },
            None => format!("document {}", r.doc.0 & !LIB_DOC),
        }
    }
}

impl Libs {
    pub fn new() -> Libs {
        Libs::default()
    }

    /// The same library documents in id order, sharing the same text allocations.
    pub fn same(&self, other: &Libs) -> bool {
        self.docs.len() == other.docs.len() && self.docs.iter().zip(&other.docs).all(|(a, b)| {
            a.lib == b.lib && a.name == b.name && Arc::ptr_eq(&a.text, &b.text)
        })
    }

    /// Add a document of library `lib` (`std`, `mcu`); its kind comes from its name.
    pub fn add(&mut self, lib: &str, name: impl Into<String>, text: impl Into<Arc<str>>) -> Result<DocId, Diag> {
        let id = DocId(LIB_DOC | self.docs.len() as u32);
        let doc = new_doc(id, name.into(), text.into(), Some(lib.to_string()), &self.docs)?;
        self.docs.push(doc);
        self.defs = Arc::new(OnceLock::new());
        Ok(id)
    }

    pub fn doc(&self, id: DocId) -> Option<&Doc> {
        if id.0 & LIB_DOC == 0 {
            return None;
        }
        self.docs.get((id.0 & !LIB_DOC) as usize)
    }

    pub fn docs(&self) -> &[Doc] {
        &self.docs
    }

    /// The documents of one library.
    pub fn of(&self, lib: &str) -> impl Iterator<Item = &Doc> + '_ {
        let lib = lib.to_string();
        self.docs.iter().filter(move |d| d.lib.as_deref() == Some(lib.as_str()))
    }
}

/// 1-based line and character column of byte `offset` in `text`.
pub fn line_col(text: &str, offset: u32) -> Option<(u32, u32)> {
    let at = offset as usize;
    if at > text.len() || !text.is_char_boundary(at) {
        return None;
    }
    let before = &text[..at];
    let line = before.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    Some((line, text[start..at].chars().count() as u32 + 1))
}

/// A located error: `message` about `span` of `doc`.
pub fn located(doc: DocId, span: std::ops::Range<u32>, message: impl Into<String>) -> Diag {
    Diag::error(message, vec![Sel::Source { doc, start: span.start, end: span.end }])
}

/// The non-document files below `parts/`, in name order, relative to `dir` with `/` separators.
/// Only names are read, so callers can list assets without loading documents or binary contents.
/// A missing `parts/` folder has no assets.
pub fn part_assets(dir: &Path) -> Result<Vec<String>, Vec<Diag>> {
    let (mut docs, mut assets, mut errors) = (Vec::new(), Vec::new(), Vec::new());
    part_files(dir, "parts", &mut docs, &mut assets, &mut errors);
    if errors.is_empty() {
        assets.sort();
        Ok(assets)
    } else {
        Err(errors)
    }
}

/// One walk below `folder`: documents into `docs`, every other file into `assets`.
fn part_files(dir: &Path, folder: &str, docs: &mut Vec<String>, assets: &mut Vec<String>, errors: &mut Vec<Diag>) {
    let entries = match std::fs::read_dir(dir.join(folder)) {
        Ok(entries) => entries,
        Err(e) if folder == "parts" && e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            errors.push(Diag::error(format!("{folder}: {e}"), vec![]));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                errors.push(Diag::error(format!("{folder}: {e}"), vec![]));
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(|name| format!("{folder}/{name}")) else { continue };
        match entry.file_type() {
            Ok(t) if t.is_dir() => part_files(dir, &name, docs, assets, errors),
            Ok(t) if t.is_file() => {
                if DocKind::of_name(&name).is_some() { docs.push(name) } else { assets.push(name) }
            }
            Ok(_) => {}
            Err(e) => errors.push(Diag::error(format!("{name}: {e}"), vec![])),
        }
    }
}

/// Top-level electronics documents and documents below `parts/`, in name order, followed by an
/// empty system document named `SYSTEM_DOC` when none exists on disk. Other files below
/// `parts/` are recorded as assets without reading their contents.
pub fn load_dir(dir: &Path) -> Result<Sources, Vec<Diag>> {
    let entries = std::fs::read_dir(dir).map_err(|e| vec![Diag::error(format!("{}: {e}", dir.display()), vec![])])?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| DocKind::of_name(n).is_some())
        .collect();
    let mut sources = Sources::new();
    let mut errors = Vec::new();
    let mut assets = Vec::new();
    part_files(dir, "parts", &mut names, &mut assets, &mut errors);
    for name in assets {
        sources.add_asset(name);
    }
    names.sort();
    for name in names {
        match std::fs::read_to_string(dir.join(&name)) {
            Ok(text) => {
                if let Err(e) = sources.add(name, text) {
                    errors.push(e);
                }
            }
            Err(e) => errors.push(Diag::error(format!("{name}: {e}"), vec![])),
        }
    }
    if errors.is_empty() {
        sources.ensure_system().map_err(|e| vec![e])?;
        Ok(sources)
    } else {
        Err(errors)
    }
}
