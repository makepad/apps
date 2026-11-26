//! KiCad's 3D model library (EKMODELS): where its
//! models are, which library version a board's models come from, and the hosts Fab downloads one
//! model file from.
//!
//! The library is CC-BY-SA 4.0, with KiCad's exception for designs that use it and the files made
//! from them. Fab never ships or commits a model: a person's Fab reads them from their KiCad install
//! or downloads the ones their boards use into their own part library.

use crate::glb::write_glb;
use crate::wrl::read_wrl;
use crate::{Affine, Issue};
use std::path::{Path, PathBuf};

/// The library's licence, as KiCad states it.
pub const LICENCE: &str = "CC-BY-SA 4.0, with KiCad's exception: designs that use the models, and the files made from them, are not bound by the licence";

/// Where KiCad states the licence and its exception.
pub const LICENCE_URL: &str = "https://www.kicad.org/libraries/license/";

/// The canonical host: KiCad's library team publishes and tags the library on GitLab since the
/// 2020 move off GitHub.
pub const GITLAB_HOST: &str = "gitlab.com";

/// The fallback host: the library's pre-move home on GitHub, kept read-only; it may lack newer tags.
pub const GITHUB_HOST: &str = "raw.githubusercontent.com";

/// The library's release tag Fab downloads for each KiCad major version, oldest first. A model is
/// downloaded at one of these tags only, and the tag is kept with it, so a model never changes
/// under its user. A new KiCad release is one row here.
pub const TAGS: &[(u32, &str)] = &[(5, "5.1.0"), (6, "6.0.0"), (7, "7.0.0"), (8, "8.0.0"), (9, "9.0.0")];

/// The tag for a library of KiCad `major`, and whether it is that major's own (false: the newest
/// tag this Fab knows stands in for a major it does not know).
pub fn tag_for(major: Option<u32>) -> (&'static str, bool) {
    match major.and_then(|m| TAGS.iter().find(|(known, _)| *known == m)) {
        Some((_, tag)) => (tag, true),
        None => (newest_tag(), false),
    }
}

/// The newest library tag this Fab knows.
pub fn newest_tag() -> &'static str {
    TAGS.last().map_or("9.0.0", |(_, tag)| tag)
}

/// KiCad's library on GitLab, by tag and library path: `<base><tag>/<Lib>.3dshapes/<Name>.wrl`.
pub const GITLAB_RAW: &str = "https://gitlab.com/kicad/libraries/kicad-packages3D/-/raw/";
/// The library's earlier home on GitHub, by tag and library path.
pub const GITHUB_RAW: &str = "https://raw.githubusercontent.com/KiCad/kicad-packages3D/";

/// The two raw-file URLs of `model` at `tag`, the canonical host first.
pub fn raw_urls(tag: &str, model: &LibModel) -> [(&'static str, String); 2] {
    let file = model.wrl();
    [(GITLAB_HOST, format!("{GITLAB_RAW}{tag}/{file}")), (GITHUB_HOST, format!("{GITHUB_RAW}{tag}/{file}"))]
}

/// Whether `url` is one [`raw_urls`] builds: a host's raw-file base, a tag this Fab knows and the
/// path of a `.wrl` in one of the library's `.3dshapes` folders, compared byte for byte with the
/// URL built from them (no query, fragment, port, other path or other host).
pub fn allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(GITLAB_RAW).or_else(|| url.strip_prefix(GITHUB_RAW)) else { return false };
    let mut parts = rest.split('/');
    let (Some(tag), Some(lib), Some(file), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return false };
    let Some(&(major, _)) = TAGS.iter().find(|(_, t)| *t == tag) else { return false };
    let Some(name) = file.strip_suffix(".wrl") else { return false };
    if !lib.ends_with(".3dshapes") || !plain(lib) || !plain(name) {
        return false;
    }
    let model = LibModel { major, lib: lib.to_string(), name: name.to_string() };
    raw_urls(tag, &model).iter().any(|(_, built)| built == url)
}

/// A host's answer to one GET.
pub struct Got {
    pub status: u16,
    pub body: Vec<u8>,
    /// `Retry-After` in seconds, when the answer has one.
    pub retry_after: Option<u64>,
}

/// Why a download gave no model.
#[derive(Clone, PartialEq, Debug)]
pub enum DownloadError {
    /// Neither host has the file at the tag.
    NotFound(String),
    /// Every host that did not refuse asked to wait: ask again at `until` (Unix seconds).
    Paused { until: u64, status: u16 },
    /// A network, host or file failure, said for the person.
    Failed(String),
}

/// The record beside a downloaded `.wrl`: where it came from and what it is.
#[derive(Clone, PartialEq, Debug)]
pub struct DownloadRecord {
    pub url: String,
    pub host: String,
    pub tag: String,
    /// `YYYY-MM-DD`, UTC.
    pub date: String,
    pub size: u64,
    pub sha256: String,
}

impl DownloadRecord {
    fn text(&self) -> String {
        format!("url {}\nhost {}\ntag {}\ndate {}\nsize {}\nsha256 {}\n", self.url, self.host, self.tag, self.date, self.size, self.sha256)
    }

    fn parse(text: &str) -> Option<DownloadRecord> {
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')).map(str::to_string);
        Some(DownloadRecord {
            url: field("url")?, host: field("host")?, tag: field("tag")?, date: field("date")?,
            size: field("size")?.parse().ok()?, sha256: field("sha256")?,
        })
    }
}


/// `YYYY-MM-DD` of Unix seconds, UTC (days to civil date, proleptic Gregorian).
pub fn date(secs: u64) -> String {
    let days = (secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02}")
}

/// How long a model whose download failed is not asked for again: a model neither host has at its
/// tag, a week; any other failure, an hour. A pause a host asks for is the host's own time.
const NOT_FOUND_WAIT: u64 = 7 * 86_400;
const FAILED_WAIT: u64 = 3_600;

/// The downloads that failed lately, by `<tag>/<Lib>/<Name>`: when to ask again and why
/// (`backoff.tsv` in the cache, one `key\tuntil\treason` a line).
struct Backoff {
    path: PathBuf,
    entries: std::collections::BTreeMap<String, (u64, String)>,
}

impl Backoff {
    fn load(root: &Path) -> Backoff {
        let path = root.join("backoff.tsv");
        let entries = std::fs::read_to_string(&path).unwrap_or_default().lines().filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let (key, until, reason) = (fields.next()?, fields.next()?.parse().ok()?, fields.next()?);
            Some((key.to_string(), (until, reason.to_string())))
        }).collect();
        Backoff { path, entries }
    }

    fn set(&mut self, key: &str, entry: Option<(u64, String)>) -> Result<(), Issue> {
        let changed = match entry {
            Some(entry) => self.entries.insert(key.to_string(), entry.clone()) != Some(entry),
            None => self.entries.remove(key).is_some(),
        };
        if !changed {
            return Ok(());
        }
        let text: String = self.entries.iter().map(|(k, (until, why))| format!("{k}\t{until}\t{}\n", why.replace(['\t', '\n'], " "))).collect();
        put(&self.path, text.as_bytes())
    }
}


/// One `.wrl` unit in millimetres: KiCad's VRML models are drawn in tenths of an inch.
pub const WRL_UNIT_MM: f64 = 2.54;

/// The converter's version, kept with every converted model: another version converts again from
/// the stored or installed `.wrl`.
pub const CONVERTER: u32 = 1;

/// The project folder of KiCad models: `parts/kicad/3d/<major>/<Lib>.3dshapes/<Name>.glb`.
pub const PROJECT_PREFIX: &str = "parts/kicad/3d/";

/// The file kind a footprint names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    Wrl,
    Step,
}

/// A model of KiCad's library by its path in the library: `<lib>/<name>.wrl` (`Package_SO.3dshapes`,
/// `SOIC-8_3.9x4.9mm_P1.27mm`), and the KiCad major whose library the footprint names.
#[derive(Clone, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct LibModel {
    pub major: u32,
    pub lib: String,
    pub name: String,
}

impl LibModel {
    /// `Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl`.
    pub fn wrl(&self) -> String {
        format!("{}/{}.wrl", self.lib, self.name)
    }

    /// The project path of this model at `scale` (KiCad's per-footprint scale, baked into the file).
    pub fn project_path(&self, scale: [f64; 3]) -> String {
        format!("{PROJECT_PREFIX}{}/{}/{}{}.glb", self.major, self.lib, self.name, scale_suffix(scale))
    }

    /// The model and scale a project path names, or None when it is no KiCad model's.
    pub fn of_project_path(path: &str) -> Option<(LibModel, [f64; 3])> {
        let rest = path.strip_prefix(PROJECT_PREFIX)?.strip_suffix(".glb")?;
        let mut parts = rest.split('/');
        let (major, lib, file) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || !lib.ends_with(".3dshapes") || !plain(lib) {
            return None;
        }
        let major: u32 = major.parse().ok().filter(|m: &u32| m.to_string() == major)?;
        let (name, scale) = match file.find(".scale-") {
            Some(at) => (&file[..at], parse_scale(&file[at + 7..])?),
            None => (file, [1.0; 3]),
        };
        if name.is_empty() || !plain(name) {
            return None;
        }
        let model = LibModel { major, lib: lib.to_string(), name: name.to_string() };
        (model.project_path(scale) == path).then_some((model, scale))
    }
}

/// Letters, digits and `. _ - , +`, as KiCad's library names its folders and files.
fn plain(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".."
        && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b',' | b'+'))
}

fn number_text(v: f64) -> String {
    let text = format!("{v}");
    text.replace('-', "m")
}

/// `""` for 1 1 1, else `.scale-<x>_<y>_<z>` (`m` for a minus sign).
fn scale_suffix(scale: [f64; 3]) -> String {
    if scale == [1.0; 3] {
        return String::new();
    }
    format!(".scale-{}_{}_{}", number_text(scale[0]), number_text(scale[1]), number_text(scale[2]))
}

fn parse_scale(text: &str) -> Option<[f64; 3]> {
    let mut values = text.split('_').map(|v| v.replace('m', "-").parse::<f64>().ok().filter(|v| v.is_finite()));
    let scale = [values.next()??, values.next()??, values.next()??];
    values.next().is_none().then_some(scale)
}

/// A footprint's model path (`${KICAD9_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl`)
/// as a library model, the file kind it names, or why it names none Fab can find. `board_major`
/// stands in for a path with no version in its variable (or none at all).
pub fn parse_model_path(text: &str, board_major: Option<u32>) -> Result<(LibModel, Kind), Issue> {
    let field = "model";
    let path = text.trim().replace('\\', "/");
    let (major, rest) = if let Some(rest) = path.strip_prefix("${").or_else(|| path.strip_prefix("$(")) {
        let close = rest.find(['}', ')']).ok_or_else(|| Issue::new(0, field, format!("{text}: an unclosed variable")))?;
        let var = &rest[..close];
        let rest = rest[close + 1..].trim_start_matches('/');
        let major = if var == "KISYS3DMOD" {
            Some(5)
        } else if let Some(n) = var.strip_prefix("KICAD").and_then(|v| v.strip_suffix("_3DMODEL_DIR")) {
            if n.is_empty() { board_major } else { Some(n.parse::<u32>().map_err(|_| Issue::new(0, field, format!("{text}: {var} is not a KiCad 3D library variable")))?) }
        } else if var == "KIPRJMOD" {
            return Err(Issue::new(0, field, format!("{text}: a model of the board's own project folder, which Fab does not have; the part keeps its plain body")));
        } else {
            return Err(Issue::new(0, field, format!("{text}: a model in the folder of ${{{var}}} on the board author's computer; Fab cannot find it, and the part keeps its plain body")));
        };
        (major, rest.to_string())
    } else if path.starts_with('/') || path.get(1..2) == Some(":") {
        return Err(Issue::new(0, field, format!("{text}: a file on the board author's computer; Fab cannot find it, and the part keeps its plain body")));
    } else {
        (board_major, path.clone())
    };
    let mut segments = rest.split('/');
    let (lib, file) = match (segments.next(), segments.next(), segments.next()) {
        (Some(lib), Some(file), None) if lib.ends_with(".3dshapes") => (lib, file),
        _ => return Err(Issue::new(0, field, format!("{text}: not a model of KiCad's library (<Library>.3dshapes/<Name>.wrl)"))),
    };
    let (name, ext) = file.rsplit_once('.').ok_or_else(|| Issue::new(0, field, format!("{text}: the file has no extension")))?;
    let kind = match ext.to_ascii_lowercase().as_str() {
        "wrl" => Kind::Wrl,
        "step" | "stp" => Kind::Step,
        other => return Err(Issue::new(0, field, format!("{text}: a .{other} model; Fab reads KiCad's .wrl models"))),
    };
    for segment in [lib, name] {
        if let Some(c) = segment.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ',' | '+'))) {
            return Err(Issue::new(0, field, format!("{text}: the name has a character Fab does not take in a library path ({c:?})")));
        }
        if !plain(segment) {
            return Err(Issue::new(0, field, format!("{text}: not a model of KiCad's library")));
        }
    }
    let major = major.unwrap_or_else(|| TAGS.last().map_or(9, |(m, _)| *m));
    Ok((LibModel { major, lib: lib.to_string(), name: name.to_string() }, kind))
}

/// Where a local KiCad 3D library folder came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RootFrom {
    /// The folder the person chose in 3D model sources.
    Setting,
    /// An environment variable, by name.
    Env(String),
    /// A standard install location.
    Install,
}

/// A KiCad 3D library folder on this computer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Root {
    pub dir: PathBuf,
    /// The KiCad major it belongs to, when its variable or path says.
    pub major: Option<u32>,
    pub from: RootFrom,
}

/// The standard install locations of KiCad's 3D library on this platform (folders that may not
/// exist), with the major each belongs to when its path says. `home` is the person's home;
/// `program_files` Windows' `%ProgramFiles%`. Windows' version folders are listed newest first.
pub fn install_candidates(home: &Path, program_files: Option<&Path>) -> Vec<(PathBuf, Option<u32>)> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        out.push((PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/3dmodels"), None));
        out.push((PathBuf::from("/Library/Application Support/kicad/3dmodels"), Some(6)));
        out.push((PathBuf::from("/Library/Application Support/kicad/modules/packages3d"), Some(5)));
    } else if cfg!(target_os = "windows") {
        if let Some(base) = program_files.map(|p| p.join("KiCad")) {
            let mut versions: Vec<(u32, u32, PathBuf)> = std::fs::read_dir(&base).into_iter().flatten().flatten().filter_map(|item| {
                let name = item.file_name().into_string().ok()?;
                let (major, minor) = name.split_once('.').unwrap_or((&name, "0"));
                Some((major.parse().ok()?, minor.parse().unwrap_or(0), item.path()))
            }).collect();
            versions.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));
            for (major, _, dir) in versions {
                out.push((dir.join("share").join("kicad").join("3dmodels"), Some(major)));
            }
            out.push((base.join("share").join("kicad").join("modules").join("packages3d"), Some(5)));
        }
    } else {
        out.push((PathBuf::from("/usr/share/kicad/3dmodels"), None));
        out.push((PathBuf::from("/usr/local/share/kicad/3dmodels"), None));
        out.push((PathBuf::from("/usr/share/kicad/modules/packages3d"), Some(5)));
        out.push((PathBuf::from("/var/lib/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad/3dmodels"), None));
        out.push((home.join(".local/share/flatpak/app/org.kicad.KiCad/current/active/files/share/kicad/3dmodels"), None));
    }
    out
}

/// The KiCad 3D library folders that exist, in the order a model is looked for: the person's
/// folder, then the environment's (`KICAD<n>_3DMODEL_DIR` newest first, then `KISYS3DMOD`), then
/// `installs` ([`install_candidates`]). Each folder once. Cost: one `stat` per candidate; called on
/// a worker.
pub fn roots(setting: Option<&Path>, env: &dyn Fn(&str) -> Option<String>, installs: &[(PathBuf, Option<u32>)]) -> Vec<Root> {
    let mut out: Vec<Root> = Vec::new();
    let mut add = |dir: PathBuf, major: Option<u32>, from: RootFrom| {
        if dir.is_dir() && !out.iter().any(|r| r.dir == dir) {
            out.push(Root { dir, major, from });
        }
    };
    if let Some(dir) = setting {
        add(dir.to_path_buf(), None, RootFrom::Setting);
    }
    for major in (5..=20u32).rev() {
        let var = format!("KICAD{major}_3DMODEL_DIR");
        if let Some(dir) = env(&var).filter(|d| !d.is_empty()) {
            add(PathBuf::from(dir), Some(major), RootFrom::Env(var));
        }
    }
    if let Some(dir) = env("KISYS3DMOD").filter(|d| !d.is_empty()) {
        add(PathBuf::from(dir), Some(5), RootFrom::Env("KISYS3DMOD".into()));
    }
    for (dir, major) in installs {
        add(dir.clone(), *major, RootFrom::Install);
    }
    out
}

/// What the local KiCad libraries hold of a model.
#[derive(Clone, PartialEq, Debug)]
pub enum Local {
    /// Its `.wrl`, in this root.
    Found { root: Root, file: PathBuf },
    /// Only its STEP: Fab has no STEP reader.
    StepOnly { file: PathBuf },
    /// No root holds it (the roots looked in, by folder).
    Missing { looked: Vec<PathBuf> },
}

/// Find `model` in `roots`: the person's folder first, then the roots of the model's own major,
/// then the others, each in the given order. Cost: one or two `stat`s per root.
pub fn find_local(model: &LibModel, roots: &[Root]) -> Local {
    let rank = |r: &Root| match (&r.from, r.major) {
        (RootFrom::Setting, _) => 0,
        (_, Some(m)) if m == model.major => 1,
        _ => 2,
    };
    let mut order: Vec<&Root> = roots.iter().collect();
    order.sort_by_key(|r| rank(r));
    let mut step = None;
    for root in &order {
        let wrl = root.dir.join(&model.lib).join(format!("{}.wrl", model.name));
        if wrl.is_file() {
            return Local::Found { root: (*root).clone(), file: wrl };
        }
        if step.is_none() {
            step = ["step", "stp"].iter().map(|ext| root.dir.join(&model.lib).join(format!("{}.{ext}", model.name))).find(|f| f.is_file());
        }
    }
    match step {
        Some(file) => Local::StepOnly { file },
        None => Local::Missing { looked: order.iter().map(|r| r.dir.clone()).collect() },
    }
}

/// A KiCad `.wrl` as Fab's body mesh: read, scaled from tenths of an inch to millimetres and by the
/// footprint's `scale`, written as `.glb`; with the reader's warnings. KiCad's offset and rotation
/// stay with the part's body ([`body_pose`]).
pub fn convert_wrl(bytes: &[u8], scale: [f64; 3]) -> Result<(Vec<u8>, Vec<Issue>), Issue> {
    let mut read = read_wrl(bytes)?;
    if read.mesh.is_empty() {
        return Err(Issue::new(0, "VRML", "the model has no faces"));
    }
    read.mesh.transform(&Affine::scale(scale.map(|s| s * WRL_UNIT_MM)));
    Ok((write_glb(&read.mesh), read.warnings))
}

/// A footprint model's pose for the part's body, in Fab's terms: the offset (mm) and the rotation
/// `[rx, ry, rz]` in degrees that Fab applies as `Rz * Ry * Rx`. KiCad turns a model by the negated
/// angles in the same order.
pub fn body_pose(offset: [f64; 3], rotate: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    (offset, rotate.map(|a| if a == 0.0 { 0.0 } else { -a }))
}

/// The folder of a local root in the cache: its major when known, and a digest of its path.
fn root_key(root: &Root) -> String {
    let digest = makepad_core_util::sha256::sha256_hex(root.dir.to_string_lossy().as_bytes());
    match root.major {
        Some(m) => format!("kicad{m}-{}", &digest[..8]),
        None => format!("kicad-{}", &digest[..8]),
    }
}

/// The record kept beside a converted local model: its source file and what it was made from.
#[derive(Clone, PartialEq, Debug)]
pub struct LocalRecord {
    pub source: PathBuf,
    pub major: Option<u32>,
    pub size: u64,
    pub mtime: u64,
    pub sha256: String,
    pub converter: u32,
}

impl LocalRecord {
    fn text(&self) -> String {
        format!("source {}\nmajor {}\nsize {}\nmtime {}\nsha256 {}\nconverter {}\n", self.source.display(),
            self.major.map_or("unknown".into(), |m| m.to_string()), self.size, self.mtime, self.sha256, self.converter)
    }

    fn parse(text: &str) -> Option<LocalRecord> {
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' '));
        Some(LocalRecord {
            source: PathBuf::from(field("source")?),
            major: field("major").and_then(|m| m.parse().ok()),
            size: field("size")?.parse().ok()?,
            mtime: field("mtime")?.parse().ok()?,
            sha256: field("sha256")?.to_string(),
            converter: field("converter")?.parse().ok()?,
        })
    }
}

fn stat(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((meta.len(), mtime))
}

/// Where a model came from, for the part's provenance.
#[derive(Clone, PartialEq, Debug)]
pub enum Origin {
    /// Read from a KiCad installed on this computer.
    Local { root: PathBuf, major: Option<u32>, source: PathBuf },
    /// Downloaded at a library tag from a host on a date.
    Download { tag: String, host: String, url: String, date: String },
}

/// A model ready for a design: its project path, the `.glb` in the cache, where it came from, and
/// the conversion's warnings.
#[derive(Clone, PartialEq, Debug)]
pub struct Ready {
    pub path: String,
    pub glb: PathBuf,
    pub origin: Origin,
    pub warnings: Vec<String>,
}

/// The person's KiCad model cache: `<root>/local/<root key>/<Lib>.3dshapes/<Name>[.scale-..].glb`
/// with a `.source` record beside it, and the downloads (`<root>/download/<tag>/..`).
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn local_glb(&self, root: &Root, model: &LibModel, scale: [f64; 3]) -> PathBuf {
        self.root.join("local").join(root_key(root)).join(&model.lib).join(format!("{}{}.glb", model.name, scale_suffix(scale)))
    }

    /// The model at `path` (a project path) from the local `file` in `root`: the cached conversion
    /// when its source is unchanged, else converted again and kept. Cost: a `stat` on a hit; a read,
    /// a hash and a conversion otherwise.
    pub fn local(&self, path: &str, model: &LibModel, scale: [f64; 3], root: &Root, file: &Path) -> Result<Ready, Issue> {
        let glb = self.local_glb(root, model, scale);
        let record_path = glb.with_extension("source");
        let origin = Origin::Local { root: root.dir.clone(), major: root.major, source: file.to_path_buf() };
        let (size, mtime) = stat(file).ok_or_else(|| Issue::new(0, "model", format!("{} could not be read", file.display())))?;
        let old = std::fs::read_to_string(&record_path).ok().and_then(|t| LocalRecord::parse(&t));
        if let Some(old) = &old {
            if old.source == file && old.size == size && old.mtime == mtime && old.converter == CONVERTER && glb.is_file() {
                return Ok(Ready { path: path.to_string(), glb, origin, warnings: Vec::new() });
            }
        }
        let bytes = std::fs::read(file).map_err(|e| Issue::new(0, "model", format!("{}: {e}", file.display())))?;
        let sha256 = makepad_core_util::sha256::sha256_hex(&bytes);
        let record = LocalRecord { source: file.to_path_buf(), major: root.major, size, mtime, sha256, converter: CONVERTER };
        let same = old.as_ref().is_some_and(|o| o.sha256 == record.sha256 && o.converter == CONVERTER) && glb.is_file();
        let mut warnings = Vec::new();
        if !same {
            let (data, issues) = convert_wrl(&bytes, scale).map_err(|i| Issue::new(i.line, i.field, format!("{}: {}", file.display(), i.message)))?;
            put(&glb, &data)?;
            warnings = issues.iter().map(|i| i.to_string()).collect();
        }
        put(&record_path, record.text().as_bytes())?;
        Ok(Ready { path: path.to_string(), glb, origin, warnings })
    }

    fn download_wrl(&self, tag: &str, model: &LibModel) -> PathBuf {
        self.root.join("download").join(tag).join(&model.lib).join(format!("{}.wrl", model.name))
    }

    /// One line of `requests.log`: every request Fab makes for a model, with its answer.
    fn log_request(&self, now: u64, url: &str, answer: &str) {
        use std::io::Write;
        let line = format!("{} {now} GET {url} -> {answer}\n", date(now));
        let _ = std::fs::create_dir_all(&self.root);
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(self.root.join("requests.log")) {
            let _ = file.write_all(line.as_bytes());
        }
    }

    /// A model of KiCad's library at the tag of its major ([`tag_for`]): the stored download when
    /// there is one (no request), else one GET of its single `.wrl` from the canonical host, then
    /// the other; kept with its record (URL, host, tag, date, size, SHA-256), each request logged in
    /// `requests.log`, converted at `scale`. A model that failed lately is not asked for until its
    /// wait is over. `now`: Unix seconds. Cost: on a hit a read and a hash of the stored file; on a
    /// miss up to two requests and a conversion. Called on a worker.
    pub fn download(&self, path: &str, model: &LibModel, scale: [f64; 3], now: u64,
        get: &mut dyn FnMut(&str) -> Result<Got, String>) -> Result<Ready, DownloadError>
    {
        let tag = tag_for(Some(model.major)).0;
        let shown = format!("{}/{}", model.lib, model.name);
        let failed = |what: String| DownloadError::Failed(format!("{shown}: {what}"));
        let io = |issue: Issue| failed(issue.message);
        let wrl = self.download_wrl(tag, model);
        let record_path = wrl.with_extension("source");
        let stored = std::fs::read_to_string(&record_path).ok().and_then(|t| DownloadRecord::parse(&t))
            .and_then(|record| std::fs::read(&wrl).ok().filter(|b| makepad_core_util::sha256::sha256_hex(b) == record.sha256).map(|b| (record, b)));
        let (record, bytes) = match stored {
            Some(stored) => stored,
            None => {
                let key = format!("{tag}/{}/{}", model.lib, model.name);
                let mut backoff = Backoff::load(&self.root);
                if let Some((until, why)) = backoff.entries.get(&key).filter(|(until, _)| *until > now) {
                    return Err(failed(format!("{why}; Fab asks again after {}", date(*until))));
                }
                let mut not_found = 0;
                let mut pause: Option<(u64, u16)> = None;
                let mut errors = Vec::new();
                let mut got = None;
                for (host, url) in raw_urls(tag, model) {
                    if !allowed(&url) {
                        errors.push(format!("{url} is not a KiCad library URL Fab may fetch"));
                        continue;
                    }
                    match get(&url) {
                        Ok(answer) => {
                            self.log_request(now, &url, &format!("{} {} bytes", answer.status, answer.body.len()));
                            match answer.status {
                                200 if answer.body.starts_with(b"#VRML V2.0") => {
                                    got = Some((host, url, answer.body));
                                    break;
                                }
                                200 => errors.push(format!("{host} answered with a file that is not a VRML 2.0 model")),
                                404 => not_found += 1,
                                429 | 503 => {
                                    let until = now.saturating_add(answer.retry_after.unwrap_or(600));
                                    pause = Some(pause.map_or((until, answer.status), |(u, s)| if u <= until { (u, s) } else { (until, answer.status) }));
                                }
                                status => errors.push(format!("{host} answered HTTP {status}")),
                            }
                        }
                        Err(error) => {
                            self.log_request(now, &url, &format!("failed: {error}"));
                            errors.push(format!("{host} did not answer ({error})"));
                        }
                    }
                }
                let Some((host, url, body)) = got else {
                    if not_found == 2 {
                        let why = format!("not in KiCad's library {tag} on {GITLAB_HOST} or {GITHUB_HOST}");
                        backoff.set(&key, Some((now + NOT_FOUND_WAIT, why.clone()))).map_err(io)?;
                        return Err(DownloadError::NotFound(format!("{shown}: {why}; the part keeps its plain body")));
                    }
                    if let (Some((until, status)), true) = (pause, errors.is_empty()) {
                        return Err(DownloadError::Paused { until, status });
                    }
                    if let Some((until, status)) = pause {
                        errors.push(format!("a host asked to wait until {} (HTTP {status})", date(until)));
                    }
                    let why = format!("the download did not work ({})", errors.join("; "));
                    backoff.set(&key, Some((now + FAILED_WAIT, why.clone()))).map_err(io)?;
                    return Err(failed(format!("{why}; the part keeps its plain body and Fab tries again later")));
                };
                let record = DownloadRecord {
                    url, host: host.to_string(), tag: tag.to_string(), date: date(now), size: body.len() as u64,
                    sha256: makepad_core_util::sha256::sha256_hex(&body),
                };
                put(&wrl, &body).map_err(io)?;
                put(&record_path, record.text().as_bytes()).map_err(io)?;
                backoff.set(&key, None).map_err(io)?;
                (record, body)
            }
        };
        let glb = wrl.with_file_name(format!("{}{}.glb", model.name, scale_suffix(scale)));
        let convert_path = glb.with_extension("convert");
        let convert = format!("converter {CONVERTER}\nsha256 {}\n", record.sha256);
        let mut warnings = Vec::new();
        if !(glb.is_file() && std::fs::read_to_string(&convert_path).is_ok_and(|t| t == convert)) {
            let (data, issues) = convert_wrl(&bytes, scale).map_err(|i| failed(format!("KiCad's model could not be read ({})", i)))?;
            put(&glb, &data).map_err(io)?;
            put(&convert_path, convert.as_bytes()).map_err(io)?;
            warnings = issues.iter().map(|i| i.to_string()).collect();
        }
        let origin = Origin::Download { tag: record.tag, host: record.host, url: record.url, date: record.date };
        Ok(Ready { path: path.to_string(), glb, origin, warnings })
    }

    /// Every converted local model whose source is unchanged, by project path: what a design reads
    /// without converting. The first root (by `roots` order of rank for each model) wins. Cost: one
    /// directory walk of the local cache and one `stat` of each source.
    pub fn local_meshes(&self, roots: &[Root]) -> Vec<(String, PathBuf)> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for root in roots {
            let dir = self.root.join("local").join(root_key(root));
            let Ok(libs) = std::fs::read_dir(&dir) else { continue };
            for lib in libs.flatten() {
                let Ok(lib_name) = lib.file_name().into_string() else { continue };
                let Ok(files) = std::fs::read_dir(lib.path()) else { continue };
                for file in files.flatten() {
                    let Ok(file_name) = file.file_name().into_string() else { continue };
                    let Some(stem) = file_name.strip_suffix(".glb") else { continue };
                    let Some(record) = std::fs::read_to_string(file.path().with_extension("source")).ok().and_then(|t| LocalRecord::parse(&t)) else { continue };
                    if record.converter != CONVERTER || stat(&record.source) != Some((record.size, record.mtime)) {
                        continue;
                    }
                    let (name, scale) = match stem.find(".scale-") {
                        Some(at) => (&stem[..at], parse_scale(&stem[at + 7..])),
                        None => (stem, Some([1.0; 3])),
                    };
                    let Some(scale) = scale else { continue };
                    // Listed under every major: a design names the major of its own library, and
                    // a local model stands in for any (local first).
                    for major in TAGS.iter().map(|(m, _)| *m) {
                        let model = LibModel { major, lib: lib_name.clone(), name: name.to_string() };
                        let path = model.project_path(scale);
                        if !out.iter().any(|(p, _)| *p == path) {
                            out.push((path, file.path()));
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }
}

fn put(path: &Path, bytes: &[u8]) -> Result<(), Issue> {
    let io = |e: std::io::Error| Issue::new(0, "cache", format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let name = path.file_name().map_or_else(Default::default, |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(".{name}.new-{}", std::process::id()));
    std::fs::write(&tmp, bytes).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_major_takes_its_own_tag_else_the_newest() {
        assert_eq!(tag_for(Some(9)), ("9.0.0", true), "KiCad 9's library is its own tag");
        assert_eq!(tag_for(Some(5)), ("5.1.0", true), "KiCad 5 boards name the 5.1 library");
        assert_eq!(tag_for(Some(10)), (newest_tag(), false), "a newer major takes the newest tag, said so");
        assert_eq!(tag_for(None), (newest_tag(), false), "an unknown major takes the newest tag");
        assert!(TAGS.windows(2).all(|w| w[0].0 < w[1].0), "the table runs oldest to newest");
    }

    /// A box in KiCad's style: one material defined on its own shape and used by the box, points
    /// in tenths of an inch, x and y from -0.5 to 0.5 and z from 0 to 0.5 (2.54 x 2.54 x 1.27 mm).
    pub(crate) const BOX_WRL: &str = "#VRML V2.0 utf8
#created by hand, in the style of KiCad's library
Shape {
  appearance Appearance {material DEF IC-BODY-EPOXY-04 Material {
    ambientIntensity 0.293
    diffuseColor 0.148 0.145 0.145
    specularColor 0.180 0.168 0.160
    emissiveColor 0.0 0.0 0.0
    transparency 0.0
    shininess 0.35
    }
  }
}
Shape {
  geometry IndexedFaceSet {
    creaseAngle 0.50
    coordIndex [0,3,2,1,-1,4,5,6,7,-1,0,1,5,4,-1,1,2,6,5,-1,2,3,7,6,-1,3,0,4,7,-1]
    coord Coordinate { point [-0.5 -0.5 0, 0.5 -0.5 0, 0.5 0.5 0, -0.5 0.5 0, -0.5 -0.5 0.5, 0.5 -0.5 0.5, 0.5 0.5 0.5, -0.5 0.5 0.5] }
  }
  appearance Appearance{material USE IC-BODY-EPOXY-04 }
}
";

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("fab-kicad3d-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a scratch folder");
            Scratch(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn bounds(glb: &[u8]) -> ([f32; 3], [f32; 3]) {
        crate::glb::read_glb(glb).expect("our glb reads back").mesh.bounds().expect("a mesh with points")
    }

    #[test]
    fn model_paths_name_a_library_model_or_say_why_not() {
        let soic = LibModel { major: 9, lib: "Package_SO.3dshapes".into(), name: "SOIC-8_3.9x4.9mm_P1.27mm".into() };
        for (text, major) in [
            ("${KICAD9_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl", 9),
            ("$(KICAD9_3DMODEL_DIR)/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl", 9),
            ("${KICAD9_3DMODEL_DIR}\\Package_SO.3dshapes\\SOIC-8_3.9x4.9mm_P1.27mm.wrl", 9),
            ("${KISYS3DMOD}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl", 5),
            ("Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl", 8),
        ] {
            let (model, kind) = parse_model_path(text, Some(8)).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!((model, kind), (LibModel { major, ..soic.clone() }, Kind::Wrl), "{text}");
        }
        let (model, kind) = parse_model_path("${KICAD8_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.step", None).expect("a STEP path");
        assert_eq!((model.wrl(), kind), ("Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl".to_string(), Kind::Step), "a STEP names the .wrl beside it");
        assert_eq!(parse_model_path("TerminalBlock_Phoenix.3dshapes/TerminalBlock_Phoenix_MKDS-1,5-2_1x02_P5.00mm_Horizontal.wrl", None)
            .expect("commas are library names").0.major, newest_tag_major(), "no version anywhere: the newest library");
        for (text, says) in [
            ("${KIPRJMOD}/models/x.wrl", "project folder"),
            ("${MYLIB}/x.3dshapes/y.wrl", "author's computer"),
            ("/opt/models/y.wrl", "author's computer"),
            ("C:/models/y.wrl", "author's computer"),
            ("${KICAD9_3DMODEL_DIR}/Package_SO.3dshapes/SOIC-8.igs", ".igs model"),
            ("${KICAD9_3DMODEL_DIR}/x.3dshapes/../y.wrl", "not a model of KiCad's library"),
            ("${KICAD9_3DMODEL_DIR}/x.3dshapes/a b.wrl", "character"),
            ("${KICAD9_3DMODEL_DIR}/y.wrl", "not a model of KiCad's library"),
        ] {
            let issue = parse_model_path(text, Some(9)).expect_err(text);
            assert!(issue.message.contains(says) && issue.message.contains(text.split('/').next().unwrap()), "{text}: {issue}");
        }
        for scale in [[1.0; 3], [0.3937, 0.3937, 0.3937], [-1.0, 1.0, 2.5]] {
            let path = soic.project_path(scale);
            assert_eq!(LibModel::of_project_path(&path), Some((soic.clone(), scale)), "{path} names its model and scale");
        }
        assert_eq!(soic.project_path([1.0; 3]), "parts/kicad/3d/9/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.glb");
        for other in ["parts/lcsc/C1.glb", "parts/kicad/3d/9/x.3dshapes/../y.glb", "parts/kicad/3d/09/x.3dshapes/y.glb",
            "parts/kicad/3d/9/x/y.glb", "parts/kicad/3d/9/x.3dshapes/y.scale-1_1.glb"] {
            assert_eq!(LibModel::of_project_path(other), None, "{other} is no KiCad model path");
        }
    }

    fn newest_tag_major() -> u32 {
        TAGS.last().unwrap().0
    }

    #[test]
    fn local_roots_find_the_wrl_say_step_only_and_convert_once() {
        let scratch = Scratch::new("local");
        let lib = |root: &str| scratch.0.join(root).join("Package_SO.3dshapes");
        for root in ["mine", "env8", "install9"] { std::fs::create_dir_all(lib(root)).expect("a fixture library"); }
        std::fs::write(lib("env8").join("BOX.wrl"), BOX_WRL).expect("a model in KiCad 8's library");
        std::fs::write(lib("install9").join("BOX.wrl"), BOX_WRL).expect("a model in KiCad 9's library");
        std::fs::write(lib("install9").join("ONLY_STEP.step"), "ISO-10303-21;").expect("a STEP-only model");
        let env = |var: &str| (var == "KICAD8_3DMODEL_DIR").then(|| scratch.0.join("env8").display().to_string());
        let installs = vec![(scratch.0.join("install9"), Some(9)), (scratch.0.join("absent"), None), (scratch.0.join("env8"), Some(8))];
        let found = roots(Some(&scratch.0.join("mine")), &env, &installs);
        assert_eq!(found.iter().map(|r| (r.dir.file_name().unwrap().to_str().unwrap(), r.major)).collect::<Vec<_>>(),
            [("mine", None), ("env8", Some(8)), ("install9", Some(9))], "the person's folder, the environment, the installs; each once, absent ones left out");
        assert_eq!(found[1].from, RootFrom::Env("KICAD8_3DMODEL_DIR".into()), "a root says where it came from");

        let model = |major: u32, name: &str| LibModel { major, lib: "Package_SO.3dshapes".into(), name: name.into() };
        let Local::Found { root, file } = find_local(&model(9, "BOX"), &found) else { panic!("the box is installed") };
        assert_eq!((root.major, file.clone()), (Some(9), lib("install9").join("BOX.wrl")), "a model of KiCad 9's library comes from KiCad 9's root first");
        let Local::Found { root: other, .. } = find_local(&model(7, "BOX"), &found) else { panic!("any local root stands in") };
        assert_eq!(other.major, Some(8), "a model of a library no root belongs to takes the first root that has it");
        assert_eq!(find_local(&model(9, "ONLY_STEP"), &found), Local::StepOnly { file: lib("install9").join("ONLY_STEP.step") }, "STEP only is said");
        assert!(matches!(find_local(&model(9, "NONE"), &found), Local::Missing { looked } if looked.len() == 3), "missing names the roots looked in");

        let (glb, warnings) = convert_wrl(BOX_WRL.as_bytes(), [1.0; 3]).expect("the box converts");
        assert!(warnings.is_empty(), "a clean model: {warnings:?}");
        let (lo, hi) = bounds(&glb);
        for (got, want) in lo.iter().chain(hi.iter()).zip([-1.27, -1.27, 0.0, 1.27, 1.27, 1.27]) {
            assert!((got - want).abs() < 1e-4, "tenths of an inch become millimetres: {lo:?} {hi:?}");
        }
        let (glb, _) = convert_wrl(BOX_WRL.as_bytes(), [2.0, 1.0, 0.5]).expect("a scaled box");
        let (lo, hi) = bounds(&glb);
        assert!((hi[0] - 2.54).abs() < 1e-4 && (hi[1] - 1.27).abs() < 1e-4 && (hi[2] - 0.635).abs() < 1e-4 && (lo[0] + 2.54).abs() < 1e-4,
            "the footprint's scale goes into the file: {lo:?} {hi:?}");

        let cache = Cache::new(scratch.0.join("cache"));
        let path = model(9, "BOX").project_path([1.0; 3]);
        let ready = cache.local(&path, &model(9, "BOX"), [1.0; 3], &root, &file).expect("the local model converts");
        assert_eq!(ready.origin, Origin::Local { root: root.dir.clone(), major: Some(9), source: file.clone() }, "it says where it came from");
        let first = std::fs::metadata(&ready.glb).and_then(|m| m.modified()).expect("the converted file");
        let again = cache.local(&path, &model(9, "BOX"), [1.0; 3], &root, &file).expect("a cached hit");
        assert_eq!((again.glb.clone(), std::fs::metadata(&again.glb).and_then(|m| m.modified()).ok()), (ready.glb.clone(), Some(first)),
            "an unchanged source is not converted again");
        let listed = cache.local_meshes(&found);
        assert!(listed.contains(&(path.clone(), ready.glb.clone())), "the cache lists the model under its project path: {listed:?}");
        assert!(listed.contains(&(model(8, "BOX").project_path([1.0; 3]), ready.glb.clone())), "and for a design naming another library's major");
        std::fs::write(&file, BOX_WRL.replace("0.5 0.5 0.5,", "0.5 0.5 0.75,")).expect("KiCad's model changes");
        let record = std::fs::read_to_string(ready.glb.with_extension("source")).expect("the record");
        std::fs::write(ready.glb.with_extension("source"), record.replace("mtime ", "mtime 1")).expect("make the record's time differ");
        assert!(!cache.local_meshes(&found).iter().any(|(p, _)| *p == path), "a changed source is not listed until converted again");
        let changed = cache.local(&path, &model(9, "BOX"), [1.0; 3], &root, &file).expect("converted again");
        assert!((bounds(&std::fs::read(&changed.glb).expect("the new file")).1[2] - 1.905).abs() < 1e-4, "the new model is the changed one");
    }

    #[test]
    fn the_allow_list_takes_only_the_raw_files_fab_builds() {
        let model = LibModel { major: 8, lib: "Package_SO.3dshapes".into(), name: "SOIC-8_3.9x4.9mm_P1.27mm".into() };
        let [(gitlab_host, gitlab), (github_host, github)] = raw_urls("8.0.0", &model);
        assert_eq!((gitlab_host, gitlab.as_str()), (GITLAB_HOST,
            "https://gitlab.com/kicad/libraries/kicad-packages3D/-/raw/8.0.0/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl"), "the canonical host first");
        assert_eq!((github_host, github.as_str()), (GITHUB_HOST,
            "https://raw.githubusercontent.com/KiCad/kicad-packages3D/8.0.0/Package_SO.3dshapes/SOIC-8_3.9x4.9mm_P1.27mm.wrl"));
        assert!(allowed(&gitlab) && allowed(&github), "both built URLs are allowed");
        for refused in [
            gitlab.replace("https:", "http:"), gitlab.replace("8.0.0", "master"), gitlab.replace("8.0.0", "8.0.1"),
            gitlab.replace(".wrl", ".step"), format!("{gitlab}?x=1"), format!("{gitlab}#x"), gitlab.replace("gitlab.com", "gitlab.com:443"),
            gitlab.replace("Package_SO.3dshapes/", "Package_SO.3dshapes/../"), gitlab.replace("Package_SO.3dshapes", "Package_SO"),
            gitlab.replace("/raw/", "/blob/"), github.replace("KiCad", "kicad"), github.replace("raw.githubusercontent.com", "evil.example"),
            gitlab.replace("SOIC-8", "SOIC 8"), gitlab.replace("SOIC-8", "SOIC%2F8"),
            "https://gitlab.com/kicad/libraries/kicad-packages3D/-/raw/8.0.0/Package_SO.3dshapes/a/b.wrl".into(),
        ] {
            assert!(!allowed(&refused), "{refused} is refused");
        }
        assert_eq!((date(0), date(1_795_000_000), date(951_782_400)), ("1970-01-01".into(), "2026-11-18".into(), "2000-02-29".into()));
    }

    /// A local fake of the two hosts: the canonical host first, the other when it has no file or
    /// fails; the file kept with its record and never asked for again; a model neither has waits a
    /// week; a host's pause is the run's; every request is logged.
    #[test]
    fn a_model_downloads_once_from_the_first_host_that_has_it() {
        let scratch = Scratch::new("download");
        let cache = Cache::new(scratch.0.join("cache"));
        let model = |name: &str| LibModel { major: 8, lib: "Package_SO.3dshapes".into(), name: name.into() };
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let reply = |status: u16, body: &str, retry_after: Option<u64>| Ok(Got { status, body: body.as_bytes().to_vec(), retry_after });
        let now = 1_795_000_000;
        let mut gitlab_has_box = |url: &str| {
            asked.borrow_mut().push(url.to_string());
            if url.contains("gitlab.com") && url.contains("/BOX.wrl") { reply(200, BOX_WRL, None) } else { reply(404, "", None) }
        };
        let path = model("BOX").project_path([1.0; 3]);
        let ready = cache.download(&path, &model("BOX"), [1.0; 3], now, &mut gitlab_has_box).expect("the canonical host has it");
        let url = raw_urls("8.0.0", &model("BOX"))[0].1.clone();
        assert_eq!(ready.origin, Origin::Download { tag: "8.0.0".into(), host: GITLAB_HOST.into(), url: url.clone(), date: "2026-11-18".into() });
        assert_eq!(*asked.borrow(), [url.clone()], "one request, to the canonical host");
        assert!((bounds(&std::fs::read(&ready.glb).expect("the converted model")).1[2] - 1.27).abs() < 1e-4, "converted to millimetres");
        let record = std::fs::read_to_string(scratch.0.join("cache/download/8.0.0/Package_SO.3dshapes/BOX.source")).expect("the record");
        assert!(record.contains(&format!("url {url}")) && record.contains("tag 8.0.0") && record.contains("date 2026-11-18")
            && record.contains(&format!("sha256 {}", makepad_core_util::sha256::sha256_hex(BOX_WRL.as_bytes()))), "{record}");
        let again = cache.download(&model("BOX").project_path([2.0, 1.0, 1.0]), &model("BOX"), [2.0, 1.0, 1.0], now + 5, &mut gitlab_has_box).expect("stored");
        assert_eq!(asked.borrow().len(), 1, "a stored download is never asked for again, at any scale");
        assert!((bounds(&std::fs::read(&again.glb).expect("the scaled model")).1[0] - 2.54).abs() < 1e-4 && again.glb != ready.glb, "its own scale");

        // The canonical host refuses: the next request is the fallback's raw file, exactly, and only
        // a VRML 2.0 model from it is taken.
        let mut github_only = |url: &str| {
            asked.borrow_mut().push(url.to_string());
            if url.contains("gitlab.com") { reply(403, "", None) } else { reply(200, BOX_WRL, None) }
        };
        let other = cache.download(&model("OTHER").project_path([1.0; 3]), &model("OTHER"), [1.0; 3], now, &mut github_only).expect("the fallback host");
        let github = "https://raw.githubusercontent.com/KiCad/kicad-packages3D/8.0.0/Package_SO.3dshapes/OTHER.wrl";
        assert_eq!(asked.borrow()[1..], [raw_urls("8.0.0", &model("OTHER"))[0].1.clone(), github.to_string()], "GitLab, then that exact GitHub URL");
        assert!(matches!(&other.origin, Origin::Download { host, url, .. } if host == GITHUB_HOST && url == github), "{:?}", other.origin);
        let mut github_v1 = |url: &str| {
            asked.borrow_mut().push(url.to_string());
            if url.contains("gitlab.com") { Err("connection reset".into()) } else { reply(200, "#VRML V1.0 ascii\nSeparator { }\n", None) }
        };
        let old = cache.download(&model("OLD").project_path([1.0; 3]), &model("OLD"), [1.0; 3], now, &mut github_v1);
        assert!(matches!(&old, Err(DownloadError::Failed(why)) if why.contains("raw.githubusercontent.com answered with a file that is not a VRML 2.0 model")), "{old:?}");
        assert!(!scratch.0.join("cache/download/8.0.0/Package_SO.3dshapes/OLD.wrl").exists(), "a VRML 1.0 file is not kept");

        let none = cache.download(&model("NONE").project_path([1.0; 3]), &model("NONE"), [1.0; 3], now, &mut gitlab_has_box);
        assert!(matches!(&none, Err(DownloadError::NotFound(why)) if why.contains("not in KiCad's library 8.0.0")), "{none:?}");
        let before = asked.borrow().len();
        let later = cache.download(&model("NONE").project_path([1.0; 3]), &model("NONE"), [1.0; 3], now + 3600, &mut gitlab_has_box);
        assert!(matches!(&later, Err(DownloadError::Failed(why)) if why.contains("asks again after 2026-11-25")), "{later:?}");
        assert_eq!(asked.borrow().len(), before, "a model neither host has is not asked for again for a week");

        let mut busy = |url: &str| { asked.borrow_mut().push(url.to_string()); reply(429, "", Some(120)) };
        let paused = cache.download(&model("BUSY").project_path([1.0; 3]), &model("BUSY"), [1.0; 3], now, &mut busy);
        assert_eq!(paused, Err(DownloadError::Paused { until: now + 120, status: 429 }), "both hosts asked to wait");
        let mut wrong = |_: &str| reply(200, "<html>sign in</html>", None);
        let refused = cache.download(&model("HTML").project_path([1.0; 3]), &model("HTML"), [1.0; 3], now, &mut wrong);
        assert!(matches!(&refused, Err(DownloadError::Failed(why)) if why.contains("not a VRML 2.0 model")), "{refused:?}");
        assert!(!scratch.0.join("cache/download/8.0.0/Package_SO.3dshapes/HTML.wrl").exists(), "nothing but a model is kept");
        let log = std::fs::read_to_string(scratch.0.join("cache/requests.log")).expect("the request log");
        assert_eq!(log.lines().count(), 1 + 2 + 2 + 2 + 2 + 2, "every request is logged: {log}");
        assert!(log.lines().next().is_some_and(|l| l.starts_with("2026-11-18 1795000000 GET https://gitlab.com/") && l.ends_with("-> 200 1092 bytes") || l.contains("-> 200 ")), "{log}");
    }

}
