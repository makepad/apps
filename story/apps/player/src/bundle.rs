//! A film bundle: `manifest.json` and the files it names, in one directory
//! served over HTTP (the browser) or read from disk (native).
//!
//! ```text
//! {"film": {"title", "duration", "fps", "width", "height", "bpm"},
//!  "song": {"file", "bytes"},
//!  "lyrics": {"voice": "song", "file", "offset"},            (optional)
//!  "plates": [{"name", "start", "dur", "doc", "params": {..}}],
//!  "files": {<logical name>: {"file", "bytes"}},
//!  "total_bytes": n}
//! ```
//!
//! A plate's `doc` and its media handles resolve through `files`. Tree films
//! also carry the path manifest in `project`; imports keep their authored paths.
//! A name not in `files` is a file name itself.

use makepad_motion::InputValue;
use makepad_strict_json::Value;
use makepad_widgets::makepad_network::{HttpMethod, HttpRequest, NetworkResponse};
use makepad_widgets::*;
use std::collections::HashMap;
use std::sync::Arc;
use crate::job::Job;
use makepad_story_film_pack::{names, KernelEntry, Pack};

#[derive(Clone, Debug)]
pub struct Film {
    pub title: String,
    pub duration: f64,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fps: f64,
    pub width: f64,
    pub height: f64,
    pub bpm: f64,
}

#[derive(Clone, Debug)]
pub struct PlateInfo {
    pub path: Option<String>,
    pub name: String,
    pub start: f64,
    pub dur: f64,
    /// The document's logical name.
    pub doc: String,
    pub params: Vec<(String, InputValue)>,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub file: String,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    /// The voice the documents hear the sung words as (`voice(@song)`).
    pub voice: String,
    pub file: String,
    /// The song's time at the film's 0 (lyrics times are the song's).
    pub offset: f64,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub project: Option<Arc<makepad_asset_data::tree::TreeDraft>>,
    pub film: Film,
    pub song: Option<FileEntry>,
    /// The film's edit audio (`"edit_audio"`; `"cut"` is the cut
    /// document's file): its whole sound, mixed by the player,
    /// replaces the song.
    pub edit: Option<crate::edit_audio::CutManifest>,
    pub lyrics: Option<Lyrics>,
    /// The documents' audio features (`AudioFeatures` JSON on the film's
    /// clock: the song's sung words, its analysis); preferred to `lyrics`.
    pub features: Option<FileEntry>,
    /// Font files by family (wasm resource paths, as the documents' faces
    /// name them).
    pub fonts: Vec<(String, Vec<String>)>,
    pub plates: Vec<PlateInfo>,
    pub files: HashMap<String, FileEntry>,
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Int(i) => Some(*i as f64),
        Value::F64(f) => Some(*f),
        _ => None,
    }
}

fn entry(v: Option<&Value>) -> Option<FileEntry> {
    let v = v?;
    Some(FileEntry { file: v.get("file")?.as_str()?.to_string(), bytes: num(v.get("bytes")).unwrap_or(0.0) as u64 })
}

fn param(v: &Value) -> Option<InputValue> {
    Some(match v {
        Value::Int(i) => InputValue::Number(*i as f64),
        Value::F64(f) => InputValue::Number(*f),
        Value::Bool(b) => InputValue::Bool(*b),
        Value::Str(s) => InputValue::Text(s.clone()),
        Value::Arr(a) if a.len() == 2 && a.iter().all(|value| num(Some(value)).is_some()) => InputValue::Vec2([num(a.first())?, num(a.get(1))?]),
        Value::Arr(a) if a.len() == 4 && a.iter().all(|value| num(Some(value)).is_some()) => InputValue::Color([num(a.first())?, num(a.get(1))?, num(a.get(2))?, num(a.get(3))?]),
        Value::Arr(a) => InputValue::List(a.iter().map(param).collect::<Option<Vec<_>>>()?),
        Value::Obj(fields) => match v.get("asset").and_then(Value::as_str) {
            Some(asset) => { let (id, rev) = asset.split_once(':')?; InputValue::Asset(makepad_motion::AssetRef { id: id.into(), rev: rev.into() }) }
            None => InputValue::Record(fields.iter().map(|(key, value)| Some((key.clone(), param(value)?))).collect::<Option<Vec<_>>>()?),
        },
        Value::Null => InputValue::Empty,
    })
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let root = makepad_strict_json::parse(bytes).map_err(|e| format!("manifest.json: {e}"))?;
        let f = root.get("film").ok_or("manifest.json: no `film`")?;
        let project = root.get("project").filter(|value| !value.is_null()).map(|project| -> Result<_, String> {
            use makepad_asset_data::{AssetRevisionRef, tree::{TreeDraft, TreeFile, TreeRole}};
            let mut files = std::collections::BTreeMap::new();
            for file in project.get("files").and_then(Value::as_arr).ok_or("project has no files")? {
                let path = file.get("path").and_then(Value::as_str).ok_or("project file has no path")?.to_string();
                makepad_asset_data::tree::validate_path(&path)?;
                let kind = file.get("kind").and_then(Value::as_str).and_then(makepad_asset_client::dto::kind_parse).ok_or("invalid project file kind")?;
                let reference = AssetRevisionRef {
                    asset_id: file.get("asset").and_then(Value::as_str).ok_or("missing project reference")?.parse().map_err(|e| format!("project reference: {e}"))?,
                    revision: file.get("revision").and_then(Value::as_str).ok_or("missing project revision")?.parse().map_err(|e| format!("project revision: {e}"))?,
                };
                files.insert(path.clone(), TreeFile { path, kind, reference: Some(reference), base: Some(reference), bytes: Arc::from([]) });
            }
            let optional = project.get("optional").and_then(Value::as_arr).unwrap_or(&[]).iter().map(|value| {
                let path = value.as_str().ok_or("invalid optional project path")?;
                makepad_asset_data::tree::validate_path(path)?;
                Ok(path.to_string())
            }).collect::<Result<_, String>>()?;
            Ok(Arc::new(TreeDraft { name: project.get("name").and_then(Value::as_str).unwrap_or("").into(), root: (TreeRole::Cut, project.get("root").and_then(Value::as_str).ok_or("project has no root")?.into()), files, optional }))
        }).transpose()?;
        let film = Film {
            title: f.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            duration: num(f.get("duration")).ok_or("manifest.json: no film.duration")?,
            fps: num(f.get("fps")).unwrap_or(60.0),
            width: num(f.get("width")).unwrap_or(1920.0),
            height: num(f.get("height")).unwrap_or(1080.0),
            bpm: num(f.get("bpm")).unwrap_or(120.0),
        };
        let mut plates = Vec::new();
        for p in root.get("plates").and_then(|v| v.as_arr()).unwrap_or(&[]) {
            let params = match p.get("params") {
                Some(Value::Obj(pairs)) => pairs.iter().filter_map(|(k, v)| Some((k.clone(), param(v)?))).collect(),
                _ => Vec::new(),
            };
            plates.push(PlateInfo {
                path: p.get("asset").and_then(Value::as_str).filter(|path| project.as_ref().is_some_and(|tree| tree.files.contains_key(*path))).map(str::to_string),
                name: p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                start: num(p.get("start")).ok_or("manifest.json: a plate without `start`")?,
                dur: num(p.get("dur")).ok_or("manifest.json: a plate without `dur`")?,
                doc: p.get("doc").and_then(|v| v.as_str()).ok_or("manifest.json: a plate without `doc`")?.to_string(),
                params,
            });
        }
        plates.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut files = HashMap::new();
        if let Some(Value::Obj(pairs)) = root.get("files") {
            for (name, v) in pairs {
                if let Some(e) = entry(Some(v)) {
                    files.insert(name.clone(), e);
                }
            }
        }
        if let Some(tree) = &project {
            for file in tree.files.values() {
                if let (Some(reference), Some(entry)) = (file.reference, files.get(&file.path).cloned()) {
                    files.insert(format!("{}:{}", reference.asset_id, reference.revision), entry);
                }
            }
        }
        let lyrics = root.get("lyrics").and_then(|l| {
            Some(Lyrics {
                voice: l.get("voice").and_then(|v| v.as_str()).unwrap_or("song").to_string(),
                file: l.get("file")?.as_str()?.to_string(),
                offset: num(l.get("offset")).unwrap_or(0.0),
            })
        });
        let fonts = match root.get("fonts") {
            Some(Value::Obj(pairs)) => pairs.iter().map(|(family, files)| (family.clone(), files.as_arr().unwrap_or(&[]).iter().filter_map(|f| f.as_str().map(str::to_string)).collect())).collect(),
            _ => Vec::new(),
        };
        Ok(Manifest {
            project,
            film,
            song: entry(root.get("song")),
            edit: root.get("edit_audio").map(crate::edit_audio::CutManifest::parse).transpose()?,
            lyrics,
            features: entry(root.get("features")),
            fonts,
            plates,
            files,
        })
    }

    /// The file a logical name (a document, an asset id) is in.
    pub fn file_of(&self, logical: &str) -> FileEntry {
        self.files.get(logical).cloned().unwrap_or_else(|| FileEntry { file: logical.to_string(), bytes: 0 })
    }

    /// The file of a document's asset: by `id:rev`, else by id.
    pub fn asset_file(&self, asset: &makepad_motion::AssetRef) -> FileEntry {
        match self.files.get(&format!("{}:{}", asset.id, asset.rev)) {
            Some(e) => e.clone(),
            None => self.file_of(&asset.id),
        }
    }

    /// The plate showing at `t` (the last one started).
    pub fn plate_at(&self, t: f64) -> Option<usize> {
        let i = self.plates.partition_point(|p| p.start <= t);
        (i > 0).then(|| i - 1)
    }
}

/// Where a bundle is.
pub enum Source {
    /// A directory on disk (native).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Dir(std::path::PathBuf),
    /// A URL prefix (the browser: `film/` beside the page).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Url(String),
    /// A film pack in the binary (read in place); the song is not in it:
    /// it streams from beside the page.
    #[cfg_attr(not(feature = "film"), allow(dead_code))]
    Pack(Pack<'static>),
}

/// A file's bytes: shared from a fetch, or borrowed from a film pack.
#[derive(Clone)]
pub enum Bytes {
    Shared(Arc<[u8]>),
    #[cfg_attr(not(feature = "film"), allow(dead_code))]
    Static(&'static [u8]),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Shared(b) => b,
            Bytes::Static(b) => b,
        }
    }
}

enum Pending {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Read(Job<Result<Vec<u8>, String>>),
    Http,
}

/// Fetches bundle files: file reads on the pool natively, HTTP requests in
/// the browser. [`Fetcher::handle_event`] returns what arrived.
pub struct Fetcher {
    source: Source,
    pending: HashMap<LiveId, (String, Pending)>,
    /// Pack files, answered on the next event.
    ready: Vec<(LiveId, Result<Bytes, String>)>,
    /// Bytes arrived per request (HTTP progress) and in total.
    progress: HashMap<LiveId, u64>,
    pub received: u64,
    /// Files that arrived.
    pub done: std::collections::HashSet<String>,
}

impl Fetcher {
    pub fn new(source: Source) -> Self {
        Self { source, pending: HashMap::new(), ready: Vec::new(), progress: HashMap::new(), received: 0, done: Default::default() }
    }

    /// The film pack's kernels (module, entries), when the source is a pack
    /// that has them.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pack_kernels(&self) -> Option<(&'static [u8], Vec<KernelEntry>)> {
        let Source::Pack(pack) = &self.source else { return None };
        let module = pack.get(names::KERNELS)?;
        match KernelEntry::read_all(pack.get(names::KERNEL_KEYS).unwrap_or(&[])) {
            Ok(entries) => Some((module, entries)),
            Err(e) => {
                error!("player: the film pack's {e}");
                None
            }
        }
    }

    /// The film pack's audio programs (module, program key of each export
    /// in order), when the source is a pack that has them.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn pack_audio(&self) -> Option<(&'static [u8], Vec<u64>)> {
        let Source::Pack(pack) = &self.source else { return None };
        let module = pack.get(names::AUDIO)?;
        match makepad_story_film_pack::read_audio_keys(pack.get(names::AUDIO_KEYS).unwrap_or(&[])) {
            Ok(keys) => Some((module, keys)),
            Err(e) => {
                error!("player: the film pack's {e}");
                None
            }
        }
    }

    /// The URL of a bundle file, for what the page streams itself (the song
    /// in a single-threaded browser build).
    pub fn url_of(&self, file: &str) -> Option<String> {
        match &self.source {
            Source::Url(base) => Some(format!("{base}{file}")),
            // The pack names the song's URL, else it is beside the page.
            Source::Pack(pack) => Some(pack.get(names::SONG_URL).map(|u| String::from_utf8_lossy(u).trim().to_string()).unwrap_or_else(|| file.to_string())),
            Source::Dir(_) => None,
        }
    }


    /// Bytes of `file` here so far: all of them once it arrived, its HTTP
    /// progress while it comes.
    pub fn bytes_of(&self, file: &str, size: u64) -> u64 {
        if self.done.contains(file) {
            return size;
        }
        self.pending.iter().find(|(_, (f, _))| f == file).and_then(|(id, _)| self.progress.get(id)).copied().unwrap_or(0).min(size)
    }


    /// Fetch a URL relative to the page (the app's own resources) in the
    /// browser; natively, a bundle file of that name.
    pub fn fetch_app(&mut self, cx: &mut Cx, path: &str) -> LiveId {
        if let Source::Pack(pack) = &self.source {
            if let Some(bytes) = pack.get(path) {
                let id = LiveId::unique();
                self.ready.push((id, Ok(Bytes::Static(bytes))));
                return id;
            }
        }
        if let Source::Url(_) | Source::Pack(..) = &self.source {
            let id = LiveId::unique();
            cx.http_request(id, HttpRequest::new(path.to_string(), HttpMethod::GET));
            self.pending.insert(id, (path.to_string(), Pending::Http));
            return id;
        }
        self.fetch(cx, path)
    }

    pub fn fetch(&mut self, cx: &mut Cx, file: &str) -> LiveId {
        let id = LiveId::unique();
        match &self.source {
            Source::Dir(dir) => {
                let path = dir.join(file);
                let read = Job::run(cx, move || std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display())));
                self.pending.insert(id, (file.to_string(), Pending::Read(read)));
            }
            Source::Url(base) => {
                let url = format!("{base}{file}");
                cx.http_request(id, HttpRequest::new(url, HttpMethod::GET));
                self.pending.insert(id, (file.to_string(), Pending::Http));
            }
            Source::Pack(pack) => match pack.get(file) {
                Some(bytes) => self.ready.push((id, Ok(Bytes::Static(bytes)))),
                // Large files (pictures) sit beside the page, not in the pack.
                None => {
                    cx.http_request(id, HttpRequest::new(file.to_string(), HttpMethod::GET));
                    self.pending.insert(id, (file.to_string(), Pending::Http));
                }
            },
        }
        id
    }

    /// Files that arrived (or failed) since the last call.
    pub fn handle_event(&mut self, event: &Event) -> Vec<(LiveId, Result<Bytes, String>)> {
        let mut out = Vec::new();
        let mut done = std::mem::take(&mut self.ready);
        for (id, (file, pending)) in self.pending.iter_mut() {
            if let Pending::Read(read) = pending {
                if let Some(result) = read.try_take() {
                    done.push((*id, result.map(|b| Bytes::Shared(b.into()))));
                } else if read.is_finished() {
                    done.push((*id, Err(format!("{file}: the read failed"))));
                }
            }
        }
        if let Event::NetworkResponses(responses) = event {
            for response in responses.iter() {
                match response {
                    NetworkResponse::HttpResponse { request_id, response } => {
                        if let Some((file, _)) = self.pending.get(request_id) {
                            let result = if response.status_code == 200 {
                                Ok(Bytes::Shared(response.body.clone().unwrap_or_else(|| Arc::from(Vec::new()))))
                            } else {
                                Err(format!("{file}: HTTP {}", response.status_code))
                            };
                            done.push((*request_id, result));
                        }
                    }
                    NetworkResponse::HttpError { request_id, error } => {
                        if let Some((file, _)) = self.pending.get(request_id) {
                            done.push((*request_id, Err(format!("{file}: {error:?}"))));
                        }
                    }
                    NetworkResponse::HttpProgress { request_id, progress } => {
                        if self.pending.contains_key(request_id) {
                            self.progress.insert(*request_id, progress.loaded);
                        }
                    }
                    _ => {}
                }
            }
        }
        for (id, result) in done {
            if let (Some((file, _)), Ok(_)) = (self.pending.get(&id), &result) {
                self.done.insert(file.clone());
            }
            self.pending.remove(&id);
            self.progress.remove(&id);
            if let Ok(bytes) = &result {
                self.received += bytes.len() as u64;
            }
            out.push((id, result));
        }
        out
    }
}

/// Hand the platform the pack's copies of web resources (a manifest file
/// whose logical name is `<crate>/resources/...`, such as the subset UI
/// fonts), so they are not fetched.
#[cfg(feature = "film")]
pub fn provide_pack_resources(cx: &mut Cx, pack: &'static [u8]) {
    let Ok(pack) = Pack::parse(pack) else { return };
    let Some(Ok(m)) = pack.get("manifest.json").map(Manifest::parse) else { return };
    for (logical, entry) in &m.files {
        if logical.contains("/resources/") {
            if let Some(bytes) = pack.get(&entry.file) {
                cx.provide_dependency(logical, std::rc::Rc::new(bytes.to_vec()));
            }
        }
    }
}
