//! Continuous autosave (Final Cut's way): every change to the open cut is
//! written, about a second later, to a local drafts store, apart from
//! publishing to the library. A draft is the cut's Splash source plus the
//! page around it (playhead, zoom, selection, library lane), so a restart,
//! a crash or a `kill -9` reopens the cut exactly where it was, untitled
//! ones included. SAVE still publishes to the library; a draft whose source
//! differs from its last published revision is unsaved work, listed under
//! Edits as a draft until it is saved or deleted.
//!
//! Files, in `$STAGE_EDIT_DRAFTS` (else `<makepad home>/stage-product/edit-drafts`):
//! `<id>.splash` and `<id>.json` per draft, `last` (the id open last), and
//! `state.json` (`{"unsaved": N, …}`, what a restart can check first). Every
//! write is a temporary file renamed into place, on the drafts thread.

use makepad_asset_data::{AssetAlias, AssetKind, AssetRevisionRef, tree::{TreeDraft, TreeFile, TreeRole}};
use makepad_strict_json::{self as json, Value};
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

/// How long after a change the draft is written.
pub const DEBOUNCE: f64 = 1.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftMeta {
    pub id: String,
    pub label: String,
    /// The library revision it was opened from or last saved as.
    pub head: Option<AssetRevisionRef>,
    pub tree: Option<DraftTree>,
    pub edit_stamp: u64,
    /// Changed since `head` (or never saved).
    pub dirty: bool,
    pub playhead: f64,
    pub selected: Option<String>,
    /// The library lane (`video`, `edits`, `effects`, …).
    pub lane: String,
    pub px_per_sec: f64,
    pub scroll: f64,
    /// Epoch ms of the write.
    pub updated: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftTree {
    pub draft: Arc<TreeDraft>,
    pub path: String,
    pub saved_path: String,
    pub alias: AssetAlias,
    pub pending: Vec<DraftSource>,
    pub selected: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftSource {
    pub path: String,
    pub text: String,
    pub base: Option<String>,
}

impl DraftTree {
    /// Only source bytes are kept locally; imported binary files retain their
    /// exact library revision and are fetched by the store worker on reopen.
    fn json(&self) -> Value {
        let reference = |r: Option<AssetRevisionRef>| r.map_or(Value::Null, |r| json::s(r.to_string()));
        json::obj(vec![
            ("name", json::s(&self.draft.name)), ("role", json::s(self.draft.root.0.as_str())),
            ("root", json::s(&self.draft.root.1)), ("path", json::s(&self.path)), ("project", json::s(self.alias.to_string())),
            ("selected", json::s(&self.selected)),
            ("saved_path", json::s(&self.saved_path)),
            ("optional", Value::Arr(self.draft.optional.iter().map(json::s).collect())),
            ("pending", Value::Arr(self.pending.iter().map(|source| json::obj(vec![
                ("path", json::s(&source.path)), ("text", json::s(&source.text)), ("base", source.base.as_ref().map_or(Value::Null, json::s)),
            ])).collect())),
            ("files", Value::Arr(self.draft.files.values().map(|file| json::obj(vec![
                ("path", json::s(&file.path)), ("kind", json::s(makepad_asset_client::dto::kind_name(file.kind))),
                ("reference", reference(file.reference)), ("base", reference(file.base)),
                ("text", if matches!(file.kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio) { Value::Null } else { file.text().map(json::s).unwrap_or(Value::Null) }),
            ])).collect())),
        ])
    }

    fn parse(value: &Value) -> Option<Self> {
        let text = |key| value.get(key)?.as_str();
        let role = match text("role")? { "cut" => TreeRole::Cut, "deck" => TreeRole::Deck, "score" => TreeRole::Score, "desk" => TreeRole::Desk, _ => return None };
        let optional = value.get("optional").and_then(Value::as_arr).unwrap_or(&[]).iter().map(|value| {
            let path = value.as_str()?;
            makepad_asset_data::tree::validate_path(path).ok()?;
            Some(path.to_string())
        }).collect::<Option<_>>()?;
        let mut draft = TreeDraft { name: text("name")?.into(), root: (role, text("root")?.into()), files: Default::default(), optional };
        for file in value.get("files")?.as_arr()? {
            let path = file.get("path")?.as_str()?.to_string();
            makepad_asset_data::tree::validate_path(&path).ok()?;
            let kind = makepad_asset_client::dto::kind_parse(file.get("kind")?.as_str()?)?;
            let reference = |key| -> Option<Option<AssetRevisionRef>> {
                let value = file.get(key)?;
                if value.is_null() { return Some(None); }
                let (asset, revision) = value.as_str()?.split_once('@')?;
                Some(Some(AssetRevisionRef { asset_id: asset.parse().ok()?, revision: revision.parse().ok()? }))
            };
            let bytes: Arc<[u8]> = file.get("text").and_then(Value::as_str).unwrap_or("").as_bytes().into();
            draft.files.insert(path.clone(), TreeFile { path, kind, reference: reference("reference")?, base: reference("base")?, bytes });
        }
        if !draft.files.contains_key(&draft.root.1) { return None; }
        let path = text("path")?.to_string();
        if !draft.files.contains_key(&path) { return None; }
        let mut pending = Vec::new();
        for source in value.get("pending").and_then(Value::as_arr).into_iter().flatten() {
            let path = source.get("path")?.as_str()?.to_string();
            makepad_asset_data::tree::validate_path(&path).ok()?;
            pending.push(DraftSource { path, text: source.get("text")?.as_str()?.into(), base: source.get("base").and_then(Value::as_str).map(str::to_string) });
        }
        let selected = text("selected").unwrap_or(&path).to_string();
        let saved_path = text("saved_path").unwrap_or(&path).to_string();
        Some(Self { draft: Arc::new(draft), path, saved_path, alias: text("project")?.parse().ok()?, pending, selected })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub meta: DraftMeta,
    pub source: String,
}

pub enum DraftJob {
    /// The last open draft and every draft's meta.
    Load,
    /// `log`: say so in the log (Stage is closing).
    Write { draft: Draft, log: bool },
    Delete(String),
    /// Changes not in a draft yet (for `state.json`; `log` as above).
    Pending { unsaved: usize, log: bool },
}

pub enum DraftReply {
    Loaded { last: Option<Draft>, drafts: Vec<DraftMeta> },
    Written { id: String, drafts: Vec<DraftMeta> },
    Deleted { id: String, drafts: Vec<DraftMeta> },
    Failed(String),
}

pub struct Drafts {
    jobs: Option<Sender<DraftJob>>,
    replies: Receiver<DraftReply>,
}

impl Drafts {
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = channel();
        let (reply_tx, reply_rx) = channel();
        let dir = dir();
        let options = ThreadOptions { name: Some(std::sync::Arc::from("stage-edit-drafts")), stack_size: None, priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || worker(&dir, job_rx, reply_tx)).map_err(|error| format!("{error:?}"))?;
        handle.detach();
        Ok(Self { jobs: Some(job_tx), replies: reply_rx })
    }

    pub fn send(&self, job: DraftJob) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    pub fn try_recv(&self) -> Option<DraftReply> {
        self.replies.try_recv().ok()
    }

    /// No more jobs; the thread ends after the queued ones (the last write
    /// included).
    pub fn stop(&mut self) {
        self.jobs.take();
    }
}

pub fn dir() -> PathBuf {
    makepad_app_shell::user_state::dir(Some("STAGE_EDIT_DRAFTS"), "edit-drafts")
}

/// A new untitled draft's id.
pub fn new_id() -> String {
    format!("untitled-{}", now_ms())
}

/// An opened library edit's draft id: one draft per edit.
pub fn id_of(head: &AssetRevisionRef) -> String {
    head.asset_id.to_string()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

fn worker(dir: &Path, jobs: Receiver<DraftJob>, replies: Sender<DraftReply>) {
    while let Ok(job) = jobs.recv() {
        let reply = match job {
            DraftJob::Load => {
                let last = std::fs::read_to_string(dir.join("last")).ok().map(|id| id.trim().to_string()).filter(|id| valid_id(id));
                let last = last.and_then(|id| read(dir, &id));
                let drafts = list(dir);
                trace!("stage.drafts", "edit: drafts in {}: {} unsaved, reopening {}", dir.display(), drafts.iter().filter(|meta| meta.dirty).count(), last.as_ref().map_or("none", |last| last.meta.id.as_str()));
                DraftReply::Loaded { last, drafts }
            }
            DraftJob::Write { draft, log } => match write(dir, &draft) {
                Ok(()) => {
                    state(dir, &draft.meta.id, 0, log);
                    DraftReply::Written { id: draft.meta.id.clone(), drafts: list(dir) }
                }
                Err(error) => DraftReply::Failed(format!("draft not written: {error}")),
            },
            DraftJob::Delete(id) => {
                if valid_id(&id) {
                    // Literal paths built from a checked id, never a glob.
                    let _ = std::fs::remove_file(dir.join(format!("{id}.splash")));
                    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
                }
                DraftReply::Deleted { id, drafts: list(dir) }
            }
            DraftJob::Pending { unsaved, log } => {
                let id = std::fs::read_to_string(dir.join("last")).unwrap_or_default();
                state(dir, id.trim(), unsaved, log);
                continue;
            }
        };
        if replies.send(reply).is_err() {
            break;
        }
    }
}

/// `state.json` and the log line a restart checks for.
fn state(dir: &Path, id: &str, unsaved: usize, log: bool) {
    let text = json::obj(vec![("unsaved", Value::Int(unsaved as i64)), ("draft", json::s(id)), ("updated", Value::Int(now_ms() as i64))]).to_json();
    let _ = std::fs::create_dir_all(dir);
    let _ = atomic_write(&dir.join("state.json"), text.as_bytes());
    if log {
        log!("edit: unsaved changes: {unsaved} (draft {id} in {})", dir.display());
    }
}

/// Ids are ours (`untitled-<ms>` or an asset id): letters, digits, `-`, `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() < 128 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|ext| ext.to_str()).unwrap_or("")));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn write(dir: &Path, draft: &Draft) -> std::io::Result<()> {
    if !valid_id(&draft.meta.id) {
        return Err(std::io::Error::other(format!("bad draft id `{}`", draft.meta.id)));
    }
    std::fs::create_dir_all(dir)?;
    // The source first: a meta always has its source.
    atomic_write(&dir.join(format!("{}.splash", draft.meta.id)), draft.source.as_bytes())?;
    let mut metadata = json::parse(meta_json(&draft.meta).as_bytes()).map_err(|error| std::io::Error::other(error.to_string()))?;
    // Source and all file drafts are committed in one rename. The separate
    // .splash remains readable by older drafts, never the authority for a tree.
    if let Value::Obj(fields) = &mut metadata { fields.push(("source".into(), json::s(&draft.source))); }
    atomic_write(&dir.join(format!("{}.json", draft.meta.id)), metadata.to_json().as_bytes())?;
    atomic_write(&dir.join("last"), draft.meta.id.as_bytes())
}

pub fn read(dir: &Path, id: &str) -> Option<Draft> {
    if !valid_id(id) {
        return None;
    }
    let bytes = std::fs::read(dir.join(format!("{id}.json"))).ok()?;
    let value = json::parse(&bytes).ok()?;
    let source = value.get("source").and_then(Value::as_str).map(str::to_string).or_else(|| std::fs::read_to_string(dir.join(format!("{id}.splash"))).ok())?;
    let meta = parse_meta(&bytes)?;
    Some(Draft { meta, source })
}

/// Every draft's meta, newest first.
pub fn list(dir: &Path) -> Vec<DraftMeta> {
    let mut drafts: Vec<DraftMeta> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json") && entry.file_name() != "state.json")
        .filter_map(|entry| parse_meta(&std::fs::read(entry.path()).ok()?))
        .filter(|meta| dir.join(format!("{}.splash", meta.id)).exists())
        .collect();
    drafts.sort_by(|a, b| b.updated.cmp(&a.updated));
    drafts
}

fn meta_json(meta: &DraftMeta) -> String {
    let num = |value: f64| json::s(format!("{value}"));
    json::obj(vec![
        ("id", json::s(&meta.id)),
        ("label", json::s(&meta.label)),
        ("head", meta.head.map_or(Value::Null, |head| json::obj(vec![("asset", json::s(head.asset_id.to_string())), ("revision", json::s(head.revision.to_string()))]))),
        ("tree", meta.tree.as_ref().map_or(Value::Null, DraftTree::json)),
        ("edit_stamp", Value::Int(meta.edit_stamp as i64)),
        ("dirty", Value::Bool(meta.dirty)),
        ("playhead", num(meta.playhead)),
        ("selected", meta.selected.as_ref().map_or(Value::Null, json::s)),
        ("lane", json::s(&meta.lane)),
        ("px_per_sec", num(meta.px_per_sec)),
        ("scroll", num(meta.scroll)),
        ("updated", Value::Int(meta.updated as i64)),
    ]).to_json()
}

fn parse_meta(bytes: &[u8]) -> Option<DraftMeta> {
    let value = json::parse(bytes).ok()?;
    let text = |key: &str| value.get(key).and_then(|value| value.as_str()).map(str::to_string);
    let num = |key: &str| text(key).and_then(|text| text.parse::<f64>().ok()).filter(|value| value.is_finite()).unwrap_or(0.0);
    let id = text("id").filter(|id| valid_id(id))?;
    let head = value.get("head").filter(|head| !head.is_null()).and_then(|head| {
        Some(AssetRevisionRef {
            asset_id: head.get("asset")?.as_str()?.parse().ok()?,
            revision: head.get("revision")?.as_str()?.parse().ok()?,
        })
    });
    Some(DraftMeta {
        id,
        label: text("label").unwrap_or_default(),
        head,
        tree: match value.get("tree") { Some(tree) if !tree.is_null() => Some(DraftTree::parse(tree)?), _ => None },
        edit_stamp: value.get("edit_stamp").and_then(Value::as_u64).unwrap_or(0),
        dirty: value.get("dirty").and_then(|dirty| dirty.as_bool()).unwrap_or(true),
        playhead: num("playhead"),
        selected: text("selected"),
        lane: text("lane").unwrap_or_default(),
        px_per_sec: num("px_per_sec"),
        scroll: num("scroll"),
        updated: value.get("updated").and_then(|updated| updated.as_u64()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_round_trips_and_lists_newest_first() {
        let dir = std::env::temp_dir().join(format!("stage-drafts-test-{}", now_ms()));
        let mut meta = DraftMeta { id: "untitled-1".into(), label: "Untitled 16:9 \"x\"".into(), dirty: true, playhead: 3.25, selected: Some("clip2".into()), lane: "edits".into(), px_per_sec: 58.5, scroll: 1.5, updated: 10, ..DraftMeta::default() };
        let draft = Draft { meta: meta.clone(), source: "let main = Stack{layers: []}\n".into() };
        write(&dir, &draft).unwrap();
        assert_eq!(read(&dir, "untitled-1"), Some(draft));
        meta.id = "untitled-2".into();
        meta.updated = 20;
        write(&dir, &Draft { meta, source: String::new() }).unwrap();
        assert_eq!(list(&dir).iter().map(|meta| meta.id.as_str()).collect::<Vec<_>>(), vec!["untitled-2", "untitled-1"]);
        assert_eq!(std::fs::read_to_string(dir.join("last")).unwrap(), "untitled-2");
        assert!(read(&dir, "../etc").is_none());
        // Tidy up the test's own files, by literal names.
        for name in ["untitled-1.splash", "untitled-1.json", "untitled-2.splash", "untitled-2.json", "last"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_dir(&dir);
    }
}
