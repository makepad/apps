//! The part library: one folder per part number, holding the raw responses exactly as received and the
//! part converted from them. A number already in the library is never fetched again: importing it reads
//! the folder, and a newer converter converts again from the stored responses. Only [`Library::delete`]
//! lets a number be fetched again.
//!
//! Layout under the root (Fab's is `~/.makepad/fab/parts`):
//!
//! ```text
//! requests.log                    one line per request: UTC time, number, GET, url, status or error, bytes
//! <source>/<N>/raw/<name>         each response body as received, never rewritten
//! <source>/<N>/<N>.part.splash    the converted part, and <N>.glb its body (when it has one)
//! <source>/<N>/convert.txt        converter version, digest of raw/, the conversion's warnings
//! <source>/<N>/refused.txt        instead of the part when the converter refused: version, digest, reason
//! ```
//!
//! A new entry is built in `<source>/.partial-<N>-...` and renamed into place, so a failed request leaves
//! nothing behind. A refused conversion keeps the entry (its raw responses), and a newer converter
//! retries from them.
//!
//! Cost: a hit is a folder check and one small read; a miss is the source's requests, one conversion and
//! a few file writes. All of it runs on a worker; nothing here is called per frame.

use crate::Issue;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One GET over the network: the status and the body, or why there is no response.
pub trait Fetch {
    fn get(&mut self, url: &str) -> Result<(u16, Vec<u8>), String>;
}

/// A part's responses by name (the source names them), in name order.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Raw {
    pub files: Vec<(String, Vec<u8>)>,
}

impl Raw {
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.files.iter().find(|(n, _)| n == name).map(|(_, b)| b.as_slice())
    }

    /// Adds or replaces a response, keeping name order.
    pub fn put(&mut self, name: &str, bytes: Vec<u8>) {
        match self.files.binary_search_by(|(n, _)| n.as_str().cmp(name)) {
            Ok(i) => self.files[i].1 = bytes,
            Err(i) => self.files.insert(i, (name.to_string(), bytes)),
        }
    }

    /// SHA-256 over every response's name and bytes in name order: what a converted part was made from.
    pub fn digest(&self) -> String {
        let mut all = Vec::new();
        for (name, bytes) in &self.files {
            all.extend_from_slice(name.as_bytes());
            all.push(0);
            all.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            all.extend_from_slice(bytes);
        }
        makepad_core_util::sha256::sha256_hex(&all)
    }
}

/// What a converter makes of a part's responses.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Converted {
    /// The Splash part file.
    pub part: String,
    /// The body as `.glb`, when the part has one.
    pub mesh: Option<Vec<u8>>,
    pub warnings: Vec<Issue>,
}

/// A part source: how its numbers look, which requests a part takes, and its converter.
pub trait Source {
    /// The library folder of this source's parts (`"lcsc"`).
    fn folder(&self) -> &'static str;
    /// The converter's version. An entry converted (or refused) by another version is converted again
    /// from its raw responses.
    fn version(&self) -> u32;
    /// The canonical number for what a person typed (letters and digits only), or why it is none; no
    /// request is made for a refused number.
    fn number(&self, text: &str) -> Result<String, Issue>;
    /// Every request the part takes, made through `get` (which fails on a status other than 200), each
    /// response stored by name into `raw`. Called only when the number is not in the library.
    fn fetch(&self, number: &str, get: &mut dyn FnMut(&str) -> Result<Vec<u8>, ImportError>, raw: &mut Raw) -> Result<(), ImportError>;
    /// The part from the stored responses: pure, makes no request.
    fn convert(&self, number: &str, raw: &Raw) -> Result<Converted, Issue>;
}

/// Why an import has no entry.
#[derive(Clone, PartialEq, Debug)]
pub enum ImportError {
    /// Not a part number of this source; nothing was requested.
    Number(Issue),
    /// A request failed (no response, or a status other than 200); nothing was stored.
    Request { number: String, url: String, message: String },
    /// A response the source cannot take for its next request (the part does not exist, a field the
    /// next request needs is missing); nothing was stored.
    Response { number: String, url: String, issue: Issue },
    /// The library could not be read or written.
    Io { path: PathBuf, message: String },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Number(issue) => write!(f, "{issue}"),
            ImportError::Request { number, url, message } => write!(f, "{number}: GET {url}: {message}"),
            ImportError::Response { number, url, issue } => write!(f, "{number}: the response of {url}: {issue}"),
            ImportError::Io { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

/// Where an entry came from on this import.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Read from the library as it was.
    Library,
    /// In the library, converted again from its raw responses by this converter version.
    Reconverted,
    /// Fetched now.
    Fetched,
}

/// A part in the library.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub number: String,
    pub dir: PathBuf,
    pub origin: Origin,
    /// The part file, when the converter took the part.
    pub part: Option<PathBuf>,
    /// Its body, when it has one.
    pub mesh: Option<PathBuf>,
    /// The conversion's warnings, as text.
    pub warnings: Vec<String>,
    /// Why the converter refused the part, as text; its raw responses are kept.
    pub refused: Option<String>,
}

pub struct Library {
    root: PathBuf,
}

const LOG: &str = "requests.log";
const CONVERT: &str = "convert.txt";
const REFUSED: &str = "refused.txt";
/// A partial entry older than this belongs to a process that is gone; the next import sweeps it.
const STALE: Duration = Duration::from_secs(3600);

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Library { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of `number` (canonical) of `source`, whether or not it exists.
    pub fn entry_dir(&self, source: &dyn Source, number: &str) -> PathBuf {
        self.root.join(source.folder()).join(number)
    }

    /// Every part of `source` whose entry holds a converted body mesh, by number in order, with
    /// the mesh file's path: the models a build reads without fetching. A folder that is no
    /// canonical number (a partial import) and an entry without a mesh (refused, or a part with
    /// none) are left out; an entry an older converter made is listed as it is. No library lists
    /// nothing. Cost: one directory read and one stat per entry; no file is read.
    pub fn meshes(&self, source: &dyn Source) -> Vec<(String, PathBuf)> {
        let Ok(items) = std::fs::read_dir(self.root.join(source.folder())) else { return Vec::new() };
        let mut meshes: Vec<(String, PathBuf)> = items.flatten().filter_map(|item| {
            let number = item.file_name().into_string().ok()?;
            canonical(source, &number).ok().filter(|canonical| *canonical == number)?;
            let mesh = item.path().join(format!("{number}.glb"));
            mesh.is_file().then_some((number, mesh))
        }).collect();
        meshes.sort_unstable();
        meshes
    }

    /// Converted part documents already on disk, in canonical part-number order.
    /// Refused and incomplete entries are never offered. This does not run the converter.
    pub fn parts(&self, source: &dyn Source) -> Vec<(String, PathBuf)> {
        let Ok(items) = std::fs::read_dir(self.root.join(source.folder())) else { return Vec::new() };
        let mut parts: Vec<_> = items.flatten().filter_map(|item| {
            let number = item.file_name().into_string().ok()?;
            canonical(source, &number).ok().filter(|canonical| *canonical == number)?;
            let dir = item.path();
            let part = dir.join(format!("{number}.part.splash"));
            (part.is_file() && !dir.join(REFUSED).exists()).then_some((number, part))
        }).collect();
        parts.sort_unstable();
        parts
    }

    /// The part for what a person typed: from the library when it is there (converted again when this
    /// converter's version differs), else fetched once through `fetch` and stored.
    pub fn import(&self, source: &dyn Source, text: &str, fetch: &mut dyn Fetch) -> Result<Entry, ImportError> {
        let number = canonical(source, text)?;
        let dir = self.entry_dir(source, &number);
        if dir.is_dir() {
            return self.open(source, &number, &dir);
        }
        let folder = self.root.join(source.folder());
        std::fs::create_dir_all(&folder).map_err(|e| io(&folder, e))?;
        sweep_partials(&folder);
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = folder.join(format!(".partial-{number}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let built = self.build(source, &number, &tmp, fetch);
        let entry = built.and_then(|()| match std::fs::rename(&tmp, &dir) {
            Ok(()) => self.read_entry(source, &number, &dir, Origin::Fetched),
            // Another process stored the number meanwhile: its entry stands.
            Err(_) if dir.is_dir() => self.open(source, &number, &dir),
            Err(e) => Err(io(&dir, e)),
        });
        if tmp.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
        }
        entry
    }

    /// Removes `number`'s entry, so the next import fetches it again. False when there was none.
    pub fn delete(&self, source: &dyn Source, text: &str) -> Result<bool, ImportError> {
        let number = canonical(source, text)?;
        let dir = self.entry_dir(source, &number);
        if !dir.is_dir() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir).map_err(|e| io(&dir, e))?;
        Ok(true)
    }

    fn build(&self, source: &dyn Source, number: &str, tmp: &Path, fetch: &mut dyn Fetch) -> Result<(), ImportError> {
        let raw_dir = tmp.join("raw");
        std::fs::create_dir_all(&raw_dir).map_err(|e| io(&raw_dir, e))?;
        let log = self.root.join(LOG);
        let mut get = |url: &str| -> Result<Vec<u8>, ImportError> {
            let answer = fetch.get(url);
            let outcome = match &answer {
                Ok((status, body)) => format!("{status} {}", body.len()),
                Err(message) => format!("error {}", message.replace('\n', " ")),
            };
            append(&log, &format!("{} {number} GET {url} {outcome}\n", utc(SystemTime::now())))?;
            match answer {
                Ok((200, body)) => Ok(body),
                Ok((status, _)) => Err(ImportError::Request { number: number.to_string(), url: url.to_string(), message: format!("status {status}") }),
                Err(message) => Err(ImportError::Request { number: number.to_string(), url: url.to_string(), message }),
            }
        };
        let mut raw = Raw::default();
        source.fetch(number, &mut get, &mut raw)?;
        for (name, bytes) in &raw.files {
            if !plain_name(name) {
                return Err(ImportError::Io { path: raw_dir.join(name), message: "a response name must be letters, digits, '.', '-' or '_'".into() });
            }
            let path = raw_dir.join(name);
            std::fs::write(&path, bytes).map_err(|e| io(&path, e))?;
        }
        write_outputs(source, number, tmp, &raw)
    }

    /// An existing entry, converted again from raw when another converter version made it.
    fn open(&self, source: &dyn Source, number: &str, dir: &Path) -> Result<Entry, ImportError> {
        let marker = if dir.join(CONVERT).is_file() { dir.join(CONVERT) } else { dir.join(REFUSED) };
        let version = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|t| t.lines().next().and_then(|l| l.strip_prefix("converter ")).and_then(|v| v.trim().parse::<u32>().ok()));
        if version == Some(source.version()) {
            return self.read_entry(source, number, dir, Origin::Library);
        }
        let raw = read_raw(&dir.join("raw"))?;
        write_outputs(source, number, dir, &raw)?;
        self.read_entry(source, number, dir, Origin::Reconverted)
    }

    fn read_entry(&self, _source: &dyn Source, number: &str, dir: &Path, origin: Origin) -> Result<Entry, ImportError> {
        let part = dir.join(format!("{number}.part.splash"));
        let mesh = dir.join(format!("{number}.glb"));
        let lines = |name: &str, key: &str| -> Vec<String> {
            std::fs::read_to_string(dir.join(name))
                .map(|t| t.lines().filter_map(|l| l.strip_prefix(key)).map(str::to_string).collect())
                .unwrap_or_default()
        };
        let refused = lines(REFUSED, "refused ").into_iter().next();
        Ok(Entry {
            number: number.to_string(),
            dir: dir.to_path_buf(),
            origin,
            part: (refused.is_none() && part.is_file()).then_some(part),
            mesh: (refused.is_none() && mesh.is_file()).then_some(mesh),
            warnings: lines(CONVERT, "warning "),
            refused,
        })
    }
}

/// The source's number for `text`, checked again here before it names a folder.
fn canonical(source: &dyn Source, text: &str) -> Result<String, ImportError> {
    let number = source.number(text).map_err(ImportError::Number)?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ImportError::Number(Issue::new(0, "number", format!("{number:?} is not letters and digits"))));
    }
    Ok(number)
}

/// Converts `raw` and writes the outcome into `dir`: the part, its mesh and `convert.txt`, or
/// `refused.txt`. Each file is written beside its place and renamed over it, the version marker last,
/// so an interrupted write is converted again at the next import.
fn write_outputs(source: &dyn Source, number: &str, dir: &Path, raw: &Raw) -> Result<(), ImportError> {
    let version = source.version();
    let digest = raw.digest();
    let part = format!("{number}.part.splash");
    let mesh = format!("{number}.glb");
    match source.convert(number, raw) {
        Ok(c) => {
            put(dir, &part, c.part.as_bytes())?;
            match &c.mesh {
                Some(bytes) => put(dir, &mesh, bytes)?,
                None => remove(dir, &mesh)?,
            }
            let mut text = format!("converter {version}\nraw {digest}\n");
            for w in &c.warnings {
                text.push_str(&format!("warning {}\n", w.to_string().replace('\n', " ")));
            }
            put(dir, CONVERT, text.as_bytes())?;
            remove(dir, REFUSED)
        }
        Err(issue) => {
            let text = format!("converter {version}\nraw {digest}\nrefused {}\n", issue.to_string().replace('\n', " "));
            put(dir, REFUSED, text.as_bytes())?;
            remove(dir, CONVERT)?;
            remove(dir, &part)?;
            remove(dir, &mesh)
        }
    }
}

fn read_raw(dir: &Path) -> Result<Raw, ImportError> {
    let mut raw = Raw::default();
    for item in std::fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let item = item.map_err(|e| io(dir, e))?;
        let name = item.file_name().to_string_lossy().into_owned();
        if item.path().is_file() && plain_name(&name) {
            let bytes = std::fs::read(item.path()).map_err(|e| io(&item.path(), e))?;
            raw.put(&name, bytes);
        }
    }
    Ok(raw)
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), ImportError> {
    let tmp = dir.join(format!(".new-{name}"));
    let path = dir.join(name);
    std::fs::write(&tmp, bytes).map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| io(&path, e))
}

fn remove(dir: &Path, name: &str) -> Result<(), ImportError> {
    let path = dir.join(name);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&path, e)),
        _ => Ok(()),
    }
}

fn append(path: &Path, line: &str) -> Result<(), ImportError> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| io(path, e))
}

/// Removes partial entries a gone process left behind (older than [`STALE`]).
fn sweep_partials(folder: &Path) {
    let Ok(items) = std::fs::read_dir(folder) else { return };
    for item in items.flatten() {
        let stale = item.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE);
        if stale && item.file_name().to_string_lossy().starts_with(".partial-") && item.path().is_dir() {
            let _ = std::fs::remove_dir_all(item.path());
        }
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn io(path: &Path, e: std::io::Error) -> ImportError {
    ImportError::Io { path: path.to_path_buf(), message: e.to_string() }
}

/// `2026-11-15T12:34:56Z`.
pub fn utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a civil date (proleptic Gregorian), by 400-year eras.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
