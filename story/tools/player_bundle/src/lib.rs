//! The film bundle a browser player loads (`makepad-story-player-bundle`, and
//! in memory for Stage's exporter).
//!
//! ```text
//! let mut bundle = build_bundle(Path::new("film.cut.splash"), Path::new("media.txt"))?;
//! bundle.set("makepad_widgets/resources/Inter.ttf", "Inter.ttf", subset_bytes); // replace an entry
//! let manifest = bundle.manifest_json();          // from the entries as they are now
//! bundle.write(Path::new("out"), false)?;          // or serve bundle.entries yourself
//! ```
//!
//! [`build_bundle`] reads the cut against its tree folder, or a media table (`<asset id>=<file>`
//! and `lyrics:<song id>=<file>` lines, paths relative to the table, as
//! `makepad-edit-render --media-file` reads it), and loads
//! every Graphic's Motion document with its params the way Stage does
//! (`MotionDoc::load_with`). Its [`Bundle::entries`] are every file the film
//! needs by logical name, each with its file name `<stem>.<16 hex>.<ext>`
//! from its content (FNV-1a 64, the hash `cargo makepad` names wasm builds
//! by), so a new revision is a new name and nothing a browser cached goes
//! stale:
//!
//! - `cut`: the cut's source;
//! - `<id>:<rev>`: the documents, the modules they import (`import(asset(..))`,
//!   transitively), the images/SVGs/models they use (`MotionDoc::
//!   dependencies`) and the song;
//! - `lyrics:<id>:<rev>`: the song's lyrics sidecar;
//! - `<crate>/resources/<file>` (the path a wasm build serves a crate
//!   resource at, what `crate_resource` fetches): the font files of every
//!   family the documents' strings or Text layers name, resolved by Motion's
//!   own font table, each family with every face Motion can pick, and Inter
//!   (every text style's fallback); an installed external family's file is
//!   `fonts/<family>/<file>`;
//! - `features`: the song as graphics hear it (`AudioFeatures::to_json`, on
//!   the cut's clock): its sung words as voice `@<clip id>` when it has
//!   lyrics, and its music analysis (`music(@<clip id>)`) when a document
//!   reads music, Stage's renderer's rule.
//!
//! [`Bundle::manifest_json`] is built from the entries as they are when it
//! is called (an entry replaced with [`Bundle::set`] is named by its new
//! bytes everywhere the manifest names it). Nothing is cached between
//! builds: the same inputs give the same bundle byte for byte.
//!
//! Tree folders and render jobs (Stage's: every file where it lies) share
//! one builder. Their `project`
//! manifest retains logical paths, file kinds, pinned revisions and optional
//! declarations; Splash sources are copied unchanged and absent media has no
//! payload entry. The player reconstructs one shared tree draft from it.

use makepad_motion::{AudioFeatures, InputValue, LayerKind, MotionContext, MotionDoc};
use makepad_story_cut::{Animated, CutDoc, CutEval, CutNodeKind, GraphicSrc, MediaRef};
use makepad_strict_json::{self as json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// FNV-1a over the bytes, 16 hex chars (`cargo makepad`'s content_hash_hex).
pub fn content_hash_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// What a bundle file is, for the size report and the loader's order:
/// `docs`, `fonts`, `textures`, `audio` or `data`.
pub fn kind_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "splash" => "docs",
        "ttf" | "otf" | "woff" | "woff2" => "fonts",
        "png" | "jpg" | "jpeg" | "webp" | "svg" | "glb" | "gltf" | "hdr" | "ktx2" => "textures",
        "mp3" | "ogg" | "wav" | "flac" | "m4a" | "opus" => "audio",
        _ => "data",
    }
}

/// One file of the bundle: its content-named file name and its bytes.
#[derive(Clone, Debug)]
pub struct Entry {
    pub file: String,
    pub bytes: Vec<u8>,
}

impl Entry {
    /// `bytes` named from `source` (its stem and extension) and their hash.
    pub fn new(source: &str, bytes: Vec<u8>) -> Entry {
        let base = source.rsplit(['/', '\\']).next().unwrap_or(source);
        let (stem, ext) = base.rsplit_once('.').unwrap_or((base, "bin"));
        let stem: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
        Entry { file: format!("{stem}.{}.{}", content_hash_hex(&bytes), ext.to_ascii_lowercase()), bytes }
    }
}

/// A plate: one Graphic of the cut.
#[derive(Clone, Debug)]
pub struct Plate {
    pub name: String,
    pub start: f64,
    pub dur: f64,
    pub track: u32,
    pub fit: String,
    /// The logical name of its document (a tree path or `<id>:<rev>`).
    pub asset: String,
    pub params: Value,
}

/// The song clip.
#[derive(Clone, Debug)]
pub struct Song {
    pub clip: String,
    /// Logical names of the audio and its lyrics.
    pub asset: String,
    pub lyrics: Option<String>,
    pub start: f64,
    pub dur: f64,
    pub source_in: f64,
    pub gain: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Bundle {
    /// Logical tree files and optional declarations; Splash stays path-based.
    pub project: Option<Value>,
    pub title: String,
    pub duration: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub bpm: f64,
    pub song: Option<Song>,
    pub plates: Vec<Plate>,
    /// Family to the logical names of its faces.
    pub fonts: BTreeMap<String, Vec<String>>,
    /// The family names the documents name (before resolution).
    pub fonts_named: Vec<String>,
    /// Whether a document reads `music(..)` (the song's analysis is in `features`).
    pub reads_music: bool,
    /// Every file by logical name.
    pub entries: BTreeMap<String, Entry>,
}

/// What [`Bundle::write`] did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Written {
    pub files: usize,
    pub written: usize,
    pub pruned: usize,
}

impl Bundle {
    /// Put `bytes` under `logical` (named from `source`'s stem and
    /// extension), replacing what was there: how a subset font replaces
    /// the original.
    pub fn set(&mut self, logical: &str, source: &str, bytes: Vec<u8>) {
        self.entries.insert(logical.to_string(), Entry::new(source, bytes));
    }

    fn add(&mut self, logical: &str, source: &str, bytes: Vec<u8>) -> Result<(), String> {
        let entry = Entry::new(source, bytes);
        match self.entries.get(logical) {
            Some(existing) if existing.file != entry.file => Err(format!("`{logical}` names two different files ({}, {})", existing.file, entry.file)),
            Some(_) => Ok(()),
            None => {
                self.entries.insert(logical.to_string(), entry);
                Ok(())
            }
        }
    }

    /// The distinct files (several logical names may share one).
    pub fn files(&self) -> BTreeMap<&str, &[u8]> {
        self.entries.values().map(|e| (e.file.as_str(), e.bytes.as_slice())).collect()
    }

    pub fn total_bytes(&self) -> usize {
        self.files().values().map(|b| b.len()).sum()
    }

    /// Files and bytes by kind.
    pub fn kinds(&self) -> BTreeMap<&'static str, (usize, usize)> {
        let mut kinds: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for (file, bytes) in self.files() {
            let entry = kinds.entry(kind_of(file)).or_default();
            entry.0 += 1;
            entry.1 += bytes.len();
        }
        kinds
    }

    fn file_of(&self, logical: &str) -> Value {
        self.entries.get(logical).map_or(Value::Null, |e| json::s(e.file.clone()))
    }

    fn file_ref(&self, logical: &str) -> Value {
        match self.entries.get(logical) {
            Some(e) => json::obj(vec![("file", json::s(e.file.clone())), ("bytes", Value::Int(e.bytes.len() as i64))]),
            None => Value::Null,
        }
    }

    /// `manifest.json`: the film, the song, the plates, the fonts and
    /// `files` (logical name to file, bytes and kind), from the entries as
    /// they are now.
    pub fn manifest_json(&self) -> String {
        let song = match &self.song {
            Some(s) => {
                let audio = &self.entries[&s.asset];
                json::obj(vec![
                    ("clip", json::s(s.clip.clone())),
                    ("asset", json::s(s.asset.clone())),
                    ("file", json::s(audio.file.clone())),
                    ("bytes", Value::Int(audio.bytes.len() as i64)),
                    ("start", Value::F64(s.start)),
                    ("dur", Value::F64(s.dur)),
                    ("in", Value::F64(s.source_in)),
                    ("gain", s.gain.map_or(Value::Null, Value::F64)),
                    ("lyrics", s.lyrics.clone().map_or(Value::Null, json::s)),
                ])
            }
            None => Value::Null,
        };
        let mut features = self.file_ref("features");
        if let Value::Obj(fields) = &mut features {
            fields.push(("clock".into(), json::s("cut")));
        }
        let plates = self
            .plates
            .iter()
            .map(|p| {
                json::obj(vec![
                    ("name", json::s(p.name.clone())),
                    ("start", Value::F64(p.start)),
                    ("dur", Value::F64(p.dur)),
                    ("track", Value::Int(p.track as i64)),
                    ("fit", json::s(p.fit.clone())),
                    ("asset", json::s(p.asset.clone())),
                    ("doc", self.file_of(&p.asset)),
                    ("params", p.params.clone()),
                ])
            })
            .collect();
        let manifest = json::obj(vec![
            ("format", json::s("makepad-story-player-bundle")),
            ("version", Value::Int(1)),
            (
                "film",
                json::obj(vec![
                    ("title", json::s(self.title.clone())),
                    ("duration", Value::F64(self.duration)),
                    ("fps", Value::F64(self.fps)),
                    ("width", Value::Int(self.width as i64)),
                    ("height", Value::Int(self.height as i64)),
                    ("bpm", Value::F64(self.bpm)),
                ]),
            ),
            ("cut", self.file_of("cut")),
            ("project", self.project.clone().unwrap_or(Value::Null)),
            ("song", song),
            ("features", features),
            ("plates", Value::Arr(plates)),
            ("fonts", Value::Obj(self.fonts.iter().map(|(family, faces)| (family.clone(), Value::Arr(faces.iter().cloned().map(json::s).collect()))).collect())),
            (
                "files",
                Value::Obj(
                    self.entries
                        .iter()
                        .map(|(logical, e)| (logical.clone(), json::obj(vec![("file", json::s(e.file.clone())), ("bytes", Value::Int(e.bytes.len() as i64)), ("kind", json::s(kind_of(&e.file)))])))
                        .collect(),
                ),
            ),
            ("kinds", Value::Obj(self.kinds().iter().map(|(kind, (n, bytes))| (kind.to_string(), json::obj(vec![("files", Value::Int(*n as i64)), ("bytes", Value::Int(*bytes as i64))]))).collect())),
            ("total_bytes", Value::Int(self.total_bytes() as i64)),
        ]);
        manifest.to_json()
    }

    /// A bundle written by [`Bundle::write`], read back from `dir` (its
    /// manifest and files): an export can then be built again from a fixed
    /// snapshot of a film.
    pub fn read(dir: &Path) -> Result<Bundle, String> {
        let text = std::fs::read(dir.join("manifest.json")).map_err(|e| format!("{}: {e}", dir.join("manifest.json").display()))?;
        let m = json::parse(&text).map_err(|e| format!("manifest.json: {e}"))?;
        let num = |v: Option<&Value>| match v {
            Some(Value::F64(f)) => *f,
            Some(Value::Int(i)) => *i as f64,
            _ => 0.0,
        };
        let text_of = |v: Option<&Value>| v.and_then(|v| v.as_str()).unwrap_or("").to_string();
        let film = m.get("film").ok_or("manifest.json: no film")?;
        let mut b = Bundle {
            project: m.get("project").filter(|value| !value.is_null()).cloned(),
            title: text_of(film.get("title")),
            duration: num(film.get("duration")),
            fps: num(film.get("fps")),
            width: num(film.get("width")) as u32,
            height: num(film.get("height")) as u32,
            bpm: num(film.get("bpm")),
            ..Bundle::default()
        };
        if let Some(Value::Obj(files)) = m.get("files") {
            for (logical, f) in files {
                let file = text_of(f.get("file"));
                let bytes = std::fs::read(dir.join(&file)).map_err(|e| format!("{file}: {e}"))?;
                b.entries.insert(logical.clone(), Entry { file, bytes });
            }
        }
        if let Some(Value::Obj(fonts)) = m.get("fonts") {
            for (family, faces) in fonts {
                b.fonts.insert(family.clone(), faces.as_arr().unwrap_or(&[]).iter().filter_map(|f| f.as_str().map(String::from)).collect());
            }
        }
        for p in m.get("plates").and_then(|p| p.as_arr()).unwrap_or(&[]) {
            b.plates.push(Plate {
                name: text_of(p.get("name")),
                start: num(p.get("start")),
                dur: num(p.get("dur")),
                track: num(p.get("track")) as u32,
                fit: text_of(p.get("fit")),
                asset: text_of(p.get("asset")),
                params: p.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        if let Some(s) = m.get("song").filter(|s| !s.is_null()) {
            b.song = Some(Song {
                clip: text_of(s.get("clip")),
                asset: text_of(s.get("asset")),
                lyrics: s.get("lyrics").and_then(|l| l.as_str()).map(String::from),
                start: num(s.get("start")),
                dur: num(s.get("dur")),
                source_in: num(s.get("in")),
                gain: match s.get("gain") {
                    Some(Value::F64(g)) => Some(*g),
                    Some(Value::Int(g)) => Some(*g as f64),
                    _ => None,
                },
            });
        }
        Ok(b)
    }

    /// Write the bundle into `out`: the files (an existing file of the same
    /// name and size is that content), then `manifest.json` whole (written
    /// aside, then renamed in), then, unless `keep_old`, every content-named
    /// file of an earlier bundle there that this one does not name (a
    /// deployment keeps them so a client holding the previous manifest
    /// finishes its film).
    pub fn write(&self, out: &Path, keep_old: bool) -> Result<Written, String> {
        std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
        let files = self.files();
        let mut done = Written { files: files.len(), ..Written::default() };
        for (file, bytes) in &files {
            let path = out.join(file);
            if std::fs::metadata(&path).map_or(true, |m| m.len() != bytes.len() as u64) {
                std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
                done.written += 1;
            }
        }
        let tmp = out.join(".manifest.json.tmp");
        std::fs::write(&tmp, self.manifest_json()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, out.join("manifest.json")).map_err(|e| format!("manifest.json: {e}"))?;
        if !keep_old {
            for entry in std::fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))?.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let hashed = name.split('.').any(|part| part.len() == 16 && part.bytes().all(|b| b.is_ascii_hexdigit()));
                if hashed && !files.contains_key(name.as_str()) && entry.path().is_file() {
                    std::fs::remove_file(entry.path()).map_err(|e| format!("{name}: {e}"))?;
                    done.pruned += 1;
                }
            }
        }
        Ok(done)
    }
}

/// The media table: asset id to file, song id to lyrics file.
#[derive(Default)]
struct Media {
    files: BTreeMap<String, PathBuf>,
    lyrics: BTreeMap<String, PathBuf>,
    paths: BTreeMap<String, String>,
}

impl Media {
    fn from_tree(tree: &makepad_asset_data::tree::TreeDraft, files: &Path) -> Result<Media, String> {
        let mut media = Media::default();
        for file in tree.files.values() {
            let reference = file.reference.ok_or_else(|| format!("{}: tree file has no revision", file.path))?;
            let key = format!("{}:{}", reference.asset_id, reference.revision);
            media.files.insert(key.clone(), files.join(&file.path));
            media.paths.insert(key, file.path.clone());
        }
        Ok(media)
    }

    /// A render job's tree: each file where it lies, named by its logical path.
    fn from_job(job: &makepad_story_edit_render::job::Job) -> Result<Media, String> {
        let tree = job.tree.as_ref().ok_or("the render job names no tree")?;
        let mut media = Media::default();
        for file in &tree.files {
            let reference = file.reference.ok_or_else(|| format!("{}: tree file has no revision", file.path))?;
            let key = format!("{}:{}", reference.asset_id, reference.revision);
            media.files.insert(key.clone(), file.location.clone());
            media.paths.insert(key, file.path.clone());
        }
        for (key, location) in &job.lyrics {
            media.lyrics.insert(key.replacen('@', ":", 1), location.clone());
        }
        Ok(media)
    }

    /// What a file's bundle entry is named from: its logical path in a
    /// tree, else its file.
    fn source(&self, id: &str, rev: &str, path: &Path) -> String {
        self.paths.get(&format!("{id}:{rev}")).cloned().unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    fn read(path: &Path) -> Result<Media, String> {
        let text = read_text(path)?;
        let base = path.parent().unwrap_or(Path::new("."));
        let mut media = Media::default();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let (id, file) = line.split_once('=').ok_or_else(|| format!("{}: `{line}` is not id=file", path.display()))?;
            let file = base.join(file.trim());
            match id.trim().strip_prefix("lyrics:") {
                Some(id) => media.lyrics.insert(id.to_string(), file),
                None => media.files.insert(id.trim().to_string(), file),
            };
        }
        Ok(media)
    }

    fn key(&self, id: &str, rev: &str) -> String {
        self.paths.get(&format!("{id}:{rev}")).cloned().unwrap_or_else(|| format!("{id}:{rev}"))
    }

    fn file(&self, id: &str, rev: &str) -> Result<&PathBuf, String> {
        self.files.get(&format!("{id}:{rev}")).or_else(|| self.files.get(id)).ok_or_else(|| format!("the media table has no file for asset {id}"))
    }
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The double-quoted string literals of a Splash source outside comments.
fn string_literals(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(next) = chars.next() {
                                s.push(next);
                            }
                        }
                        '"' => break,
                        c => s.push(c),
                    }
                }
                out.push(s);
            }
            _ => {}
        }
    }
    out
}

/// The web path a wasm build serves a font file at: the crate that holds
/// it (its nearest Cargo.toml's package name, `-` as `_`) and the path
/// inside it (`makepad_widgets/resources/Inter.ttf`); an installed external
/// family's file is its install id `fonts/<family>/<file>`.
fn font_web_path(file: &Path) -> Result<String, String> {
    let file = file.canonicalize().map_err(|e| format!("{}: {e}", file.display()))?;
    let weights = makepad_doc_fonts::weights_root();
    if let Ok(rest) = file.strip_prefix(weights.canonicalize().unwrap_or(weights)) {
        return Ok(rest.to_string_lossy().replace('\\', "/"));
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() {
            let text = read_text(&manifest)?;
            let name = text
                .lines()
                .skip_while(|line| line.trim() != "[package]")
                .find_map(|line| line.trim().strip_prefix("name").map(|rest| rest.trim_start().trim_start_matches('=').trim().trim_matches('"').to_string()))
                .ok_or_else(|| format!("{}: no package name", manifest.display()))?;
            let rest = file.strip_prefix(d).map_err(|e| e.to_string())?.to_string_lossy().replace('\\', "/");
            return Ok(format!("{}/{rest}", name.replace('-', "_")));
        }
        dir = d.parent();
    }
    Err(format!("{}: in no crate", file.display()))
}

fn input_json(value: &InputValue) -> Value {
    match value {
        InputValue::Text(s) | InputValue::Markup(s) => json::s(s.clone()),
        InputValue::Number(n) => Value::F64(*n),
        InputValue::Color(c) => Value::Arr(c.iter().map(|v| Value::F64(*v)).collect()),
        InputValue::Vec2(v) => Value::Arr(v.iter().map(|v| Value::F64(*v)).collect()),
        InputValue::Bool(b) => Value::Bool(*b),
        InputValue::Asset(a) => json::obj(vec![("asset", json::s(format!("{}:{}", a.id, a.rev)))]),
        InputValue::Empty => Value::Null,
        InputValue::List(items) => Value::Arr(items.iter().map(input_json).collect()),
        InputValue::Record(fields) => Value::Obj(fields.iter().map(|(k, v)| (k.clone(), input_json(v))).collect()),
    }
}

/// Build a cut against the tree folder or media table at `media` (see the
/// crate docs). Declared absent optional media stays silent/blank; required
/// missing files or invalid documents fail with their diagnostics.
pub fn build_bundle(edit: &Path, media: &Path) -> Result<Bundle, String> {
    if media.is_dir() {
        let dir = media.canonicalize().map_err(|e| format!("{}: {e}", media.display()))?;
        let edit = edit.canonicalize().map_err(|e| format!("{}: {e}", edit.display()))?;
        let path = edit.strip_prefix(&dir).map_err(|_| format!("{}: the cut must be inside its tree folder", edit.display()))?
            .to_str().ok_or("tree path is not UTF-8")?.replace('\\', "/");
        let snapshot = makepad_motion::tree::read_folder(&dir, &path, makepad_asset_data::tree::TreeRole::Cut)?;
        let tree = std::sync::Arc::new(makepad_asset_data::tree::TreeDraft::from_memory(&snapshot)?);
        let media = Media::from_tree(&tree, &dir)?;
        return build(&edit, media, Some((tree, path)));
    }
    build(edit, Media::read(media)?, None)
}

/// Build a tree cut from a render job (see [`makepad_story_edit_render::job`]).
pub fn build_job(job: &Path) -> Result<Bundle, String> {
    let job = makepad_story_edit_render::job::Job::read(job)?;
    let (edit, local) = job.load()?;
    let (tree, path) = local.tree.ok_or("the render job names no tree")?;
    let media = Media::from_job(&job)?;
    build(&edit, media, Some((tree, path)))
}

fn build(edit: &Path, media: Media, tree: Option<(std::sync::Arc<makepad_asset_data::tree::TreeDraft>, String)>) -> Result<Bundle, String> {
    makepad_doc_fonts::register_root("makepad_widgets", makepad_widgets_core::widgets_dir());
    let cut_source = read_text(edit)?;
    let file_name = edit.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "cut.splash".into());
    let doc = match &tree { Some((tree, path)) => CutDoc::load_tree(&cut_source, tree.context(path)), None => CutDoc::load_named(&cut_source, &file_name) }
        .map_err(|diags| diags.iter().map(|d| format!("{}:{d}", edit.display())).collect::<Vec<_>>().join("\n"))?;
    let eval = CutEval::new(&doc);
    let mut bundle = Bundle {
        title: doc.label.clone(),
        duration: doc.duration,
        fps: doc.output.fps,
        width: doc.output.width,
        height: doc.output.height,
        bpm: doc.bpm,
        ..Bundle::default()
    };
    bundle.add("cut", &file_name, cut_source.clone().into_bytes())?;
    if let Some((tree, path)) = &tree {
        bundle.project = Some(json::obj(vec![("name", json::s(&tree.name)), ("root", json::s(path)), ("optional", Value::Arr(tree.optional.iter().map(json::s).collect())), ("files", Value::Arr(tree.files.values().map(|file| {
            let reference = file.reference.expect("snapshot files have revisions");
            json::obj(vec![("path", json::s(&file.path)), ("kind", json::s(makepad_asset_client::dto::kind_name(file.kind))), ("asset", json::s(reference.asset_id.to_string())), ("revision", json::s(reference.revision.to_string()))])
        }).collect()))]));
        for file in tree.files.values().filter(|file| !matches!(file.kind, makepad_asset_data::AssetKind::Texture | makepad_asset_data::AssetKind::Video | makepad_asset_data::AssetKind::Audio)) {
            bundle.add(&file.path, &file.path, file.bytes.to_vec())?;
        }
    }

    // The documents and the modules they import, transitively.
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    for graphic in &doc.graphics {
        if let GraphicSrc::Tree(context) = &graphic.src {
            sources.insert(context.document.clone(), context.resolve(&context.document)?.text()?.into());
            continue;
        }
        let GraphicSrc::Asset(asset) = &graphic.src else { continue };
        let key = format!("{}:{}", asset.id, asset.rev);
        if sources.contains_key(&key) {
            continue;
        }
        let path = media.file(&asset.id, &asset.rev)?;
        let text = read_text(path)?;
        bundle.add(&key, &media.source(&asset.id, &asset.rev, path), text.clone().into_bytes())?;
        sources.insert(key, text);
    }
    let mut modules: BTreeMap<String, (String, String, String)> = BTreeMap::new();
    let mut todo: Vec<makepad_motion::AssetRef> = sources.values().flat_map(|s| makepad_motion::modules::asset_imports(s)).collect();
    while let Some(m) = todo.pop() {
        let key = format!("{}:{}", m.id, m.rev);
        if modules.contains_key(&key) {
            continue;
        }
        let path = media.file(&m.id, &m.rev).map_err(|e| format!("an imported module: {e}"))?;
        let text = read_text(path)?;
        bundle.add(&key, &media.source(&m.id, &m.rev, path), text.clone().into_bytes())?;
        todo.extend(makepad_motion::modules::asset_imports(&text));
        modules.insert(key, (m.id, m.rev, text));
    }
    let module_host = makepad_motion::modules::Modules::from_sources(modules.values().map(|(i, r, t)| (i.as_str(), r.as_str(), t.as_str())));

    // Every graphic loaded with its params, as Stage does, on threads: what
    // each uses and the fonts its Text layers name.
    let jobs: Vec<(usize, &str, &[(String, InputValue)])> = doc
        .graphics
        .iter()
        .enumerate()
        .filter_map(|(i, g)| match &g.src {
            GraphicSrc::Asset(a) => Some((i, sources.get(&format!("{}:{}", a.id, a.rev))?.as_str(), g.params.as_slice())),
            GraphicSrc::Tree(tree) => Some((i, sources.get(&tree.document)?.as_str(), g.params.as_slice())),
            GraphicSrc::Builtin(_) => None,
        })
        .collect();
    type Loaded = Result<(Vec<makepad_motion::AssetRef>, Vec<String>), String>;
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 4).min(jobs.len().max(1));
    let mut loaded: Vec<(usize, Loaded)> = Vec::new();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((i, source, params)) = jobs.get(at) else { break };
                        let modules = match &doc.graphics[*i].src {
                            GraphicSrc::Tree(tree) => makepad_motion::modules::Modules::from_tree(tree.clone()),
                            _ => module_host.clone(),
                        };
                        let context = MotionContext { modules, ..Default::default() };
                        let result = MotionDoc::load_with(source, &makepad_story_cut::graphic::motion_inputs(params), context)
                            .map(|motion| {
                                let fonts = motion
                                    .all_layers()
                                    .iter()
                                    .filter_map(|layer| match &layer.kind {
                                        LayerKind::Text { font, .. } => Some(font.clone()),
                                        _ => None,
                                    })
                                    .collect();
                                (motion.dependencies(), fonts)
                            })
                            .map_err(|diags| diags.iter().take(8).map(|d| d.to_string()).collect::<Vec<_>>().join("\n  "));
                        out.push((*i, result));
                    }
                    out
                })
            })
            .collect();
        for worker in workers {
            loaded.extend(worker.join().unwrap_or_default());
        }
    });
    loaded.sort_by_key(|(i, _)| *i);
    let mut errors = Vec::new();
    let mut used: BTreeSet<(String, String)> = BTreeSet::new();
    let mut font_names: BTreeSet<String> = BTreeSet::new();
    for (i, result) in loaded {
        match result {
            Ok((deps, fonts)) => {
                used.extend(deps.into_iter().map(|a| (a.id, a.rev)));
                font_names.extend(fonts);
            }
            Err(error) => errors.push(format!("`{}` does not load:\n  {error}", doc.graphics[i].id)),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    for (id, rev) in &used {
        let key = media.key(id, rev);
        if !bundle.entries.contains_key(&key) {
            let path = media.file(id, rev)?;
            bundle.add(&key, &media.source(id, rev, path), read(path)?)?;
        }
    }

    // Fonts: every family a string of the documents or modules names (the
    // canvas's `c.text(.., {font: ..})`, the constants documents keep names
    // in) or a Text layer draws in, resolved by Motion's own table, each with
    // every face Motion can pick for it; and Inter.
    font_names.insert("Inter".into());
    for text in sources.values().map(String::as_str).chain(modules.values().map(|(_, _, t)| t.as_str())).chain(tree.iter().flat_map(|(tree, _)| tree.files.values().filter_map(|file| file.text().ok()))) {
        for literal in string_literals(text) {
            if literal.len() <= 40 && makepad_doc_fonts::document_font(&literal).is_some() {
                font_names.insert(literal);
            }
        }
    }
    for name in &font_names {
        let Some(family) = makepad_doc_fonts::document_font(name) else { continue };
        let mut faces: BTreeSet<String> = bundle.fonts.remove(family).unwrap_or_default().into_iter().collect();
        for weight in (2..=18).map(|w| w as f64 * 50.0) {
            for italic in [false, true] {
                let Some(file) = makepad_doc_fonts::font_file(name, weight, italic) else { continue };
                let web = font_web_path(&file)?;
                if !bundle.entries.contains_key(&web) {
                    bundle.add(&web, &file.to_string_lossy(), read(&file)?)?;
                }
                faces.insert(web);
            }
        }
        bundle.fonts.insert(family.to_string(), faces.into_iter().collect());
    }
    bundle.fonts_named = font_names.into_iter().collect();

    // The song: its file, lyrics, and what graphics hear of it.
    bundle.reads_music = sources.values().map(String::as_str).chain(modules.values().map(|(_, _, t)| t.as_str())).chain(tree.iter().flat_map(|(tree, _)| tree.files.values().filter_map(|file| file.text().ok()))).any(|s| s.contains("music("));
    let mut features = AudioFeatures::default();
    for node in &doc.nodes {
        let (CutNodeKind::AudioClip { src: MediaRef::Asset(song), gain, .. }, Some(span)) = (&node.kind, node.span) else { continue };
        let source_in = eval.source_time(node, span.start).unwrap_or(0.0);
        let by = span.start - source_in;
        let key = media.key(&song.id, &song.rev);
        let path = media.file(&song.id, &song.rev)?;
        let bytes = read(path)?;
        let mut lyrics = None;
        if let Some(lyrics_path) = media.lyrics.get(&format!("{}:{}", song.id, song.rev)).or_else(|| media.lyrics.get(&song.id)) {
            let text = read_text(lyrics_path)?;
            let voice = makepad_motion::lyrics::voice_from_lyrics(&text, by).map_err(|e| format!("{}: {e}", lyrics_path.display()))?;
            features.voices.push((node.id.clone(), voice));
            let lkey = format!("lyrics:{key}");
            let name = media.paths.get(&format!("{}:{}", song.id, song.rev)).map_or_else(|| lyrics_path.to_string_lossy().into_owned(), |path| format!("{path}.json"));
            bundle.add(&lkey, &name, text.into_bytes())?;
            lyrics = Some(lkey);
        }
        if bundle.reads_music {
            let audio = makepad_audio_decode::decode_any(&bytes).map_err(|e| format!("{}: {e:?}", path.display()))?;
            let channels = audio.channels.max(1) as usize;
            let mono: Vec<f32> = audio.pcm_interleaved_f32.chunks(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect();
            let analysis = makepad_story_audio_features::analyze_music(&mono, audio.rate as f64);
            let music = makepad_story_audio_features::to_motion::music(&analysis).shifted(by);
            features.music.push((node.id.clone(), music));
        }
        bundle.add(&key, &media.source(&song.id, &song.rev, path), bytes)?;
        if bundle.song.is_none() {
            let gain = match gain {
                Animated::Const(g) => Some(g[0]),
                _ => None,
            };
            bundle.song = Some(Song { clip: node.id.clone(), asset: key, lyrics, start: span.start, dur: span.length, source_in, gain });
        }
    }
    bundle.add("features", "features.json", features.to_json().into_bytes())?;

    // The plates, in the cut's order.
    for graphic in &doc.graphics {
        let asset = match &graphic.src {
            GraphicSrc::Asset(asset) => format!("{}:{}", asset.id, asset.rev),
            GraphicSrc::Tree(tree) => tree.document.clone(),
            GraphicSrc::Builtin(_) => continue,
        };
        let span = graphic.span.unwrap_or(makepad_story_cut::Span { start: 0.0, length: doc.duration });
        bundle.plates.push(Plate {
            name: graphic.id.clone(),
            start: span.start,
            dur: span.length,
            track: graphic.track,
            fit: format!("{:?}", graphic.fit).to_lowercase(),
            asset,
            params: Value::Obj(graphic.params.iter().map(|(k, v)| (k.clone(), input_json(v))).collect()),
        });
    }
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cargo_makepads_content_hash() {
        assert_eq!(content_hash_hex(b""), "cbf29ce484222325");
        assert_eq!(content_hash_hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(Entry::new("dir/open.motion.splash", b"a".to_vec()).file, "open.motion.af63dc4c8601ec8c.splash");
    }

    #[test]
    fn a_replaced_entry_is_named_by_its_new_bytes_in_the_manifest() {
        let mut bundle = Bundle::default();
        bundle.set("makepad_widgets/resources/Inter.ttf", "Inter.ttf", b"full".to_vec());
        bundle.fonts.insert("Inter".into(), vec!["makepad_widgets/resources/Inter.ttf".into()]);
        bundle.set("makepad_widgets/resources/Inter.ttf", "Inter.ttf", b"sub".to_vec());
        let manifest = bundle.manifest_json();
        assert!(manifest.contains(&format!("Inter.{}.ttf", content_hash_hex(b"sub"))));
        assert!(!manifest.contains(&content_hash_hex(b"full")));
        assert_eq!(bundle.total_bytes(), 3);
    }

    #[test]
    fn literals_skip_comments() {
        assert_eq!(string_literals("// \"Archivo\"\nlet a = \"Roboto Flex\" /* \"x\" */ \"a\\\"b\""), vec!["Roboto Flex".to_string(), "a\"b".to_string()]);
    }
}
